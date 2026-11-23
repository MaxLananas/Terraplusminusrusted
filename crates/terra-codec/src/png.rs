//! PNG decoding to packed ARGB, matching `BufferedImage.getRGB` after `ImageIO.read` (OpenJDK's
//! `PNGImageReader`) for every colour type, bit depth and interlace method.
//!
//! What ImageIO does, measured exhaustively against the oracle JDK (`tools/oracle`):
//!
//! * truecolour and indexed samples are used as they are; 16-bit samples become
//!   `(v * 255 + 32767) / 65535`;
//! * 8- and 16-bit grayscale goes through Java's linear gray colour space, which `getRGB` converts
//!   to sRGB: `round(255 * sRGB(v / max))`, held here as the 256 thresholds of that step curve so
//!   no floating-point library is involved;
//! * 1-, 2- and 4-bit grayscale is scaled to 8 bits (`v * 255 / max`) without that curve, unless
//!   the image has a `tRNS` chunk: then ImageIO expands it to 8-bit gray with alpha, applies the
//!   curve, and compares the *expanded* sample with the raw `tRNS` value (so a key of 2 in a 2-bit
//!   image never matches, while 170 does);
//! * alpha is never premultiplied; `gAMA`, `cHRM`, `sRGB` and `iCCP` are ignored as ImageIO's
//!   `getRGB` ignores them.

use crate::checksum::crc32;
use crate::zlib::zlib_decompress;
use crate::DecodeError;

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// The smallest 16-bit linear gray value that `getRGB` maps to each 8-bit sRGB value
/// (`round(255 * sRGB(v / 65535))`), recorded from the oracle JDK and checked against the
/// formula by `gray_curve_matches_srgb`.
const GRAY_TO_SRGB_THRESHOLDS: [u16; 256] = [
    0, 10, 30, 50, 70, 90, 110, 130, 150, 170, 189, 209, 230, 253, 276, 301,
    327, 354, 382, 412, 443, 475, 509, 544, 580, 618, 657, 698, 740, 783, 828, 875,
    923, 972, 1023, 1075, 1129, 1185, 1242, 1300, 1360, 1422, 1486, 1551, 1617, 1685, 1755, 1827,
    1900, 1975, 2052, 2130, 2210, 2292, 2376, 2461, 2548, 2637, 2727, 2820, 2914, 3010, 3108, 3208,
    3309, 3412, 3518, 3625, 3734, 3844, 3957, 4072, 4188, 4307, 4427, 4550, 4674, 4800, 4928, 5059,
    5191, 5325, 5461, 5599, 5740, 5882, 6026, 6173, 6321, 6471, 6624, 6778, 6935, 7094, 7255, 7418,
    7583, 7750, 7919, 8091, 8265, 8440, 8618, 8798, 8981, 9165, 9352, 9541, 9732, 9925, 10121, 10318,
    10518, 10720, 10925, 11132, 11341, 11552, 11765, 11981, 12199, 12420, 12643, 12868, 13095, 13325, 13557, 13791,
    14028, 14267, 14508, 14752, 14998, 15247, 15498, 15751, 16007, 16265, 16525, 16788, 17054, 17321, 17592, 17864,
    18139, 18417, 18697, 18980, 19264, 19552, 19842, 20134, 20429, 20727, 21027, 21329, 21634, 21942, 22252, 22564,
    22880, 23197, 23518, 23840, 24166, 24494, 24824, 25158, 25493, 25832, 26173, 26516, 26862, 27211, 27563, 27917,
    28273, 28633, 28995, 29359, 29727, 30097, 30469, 30845, 31223, 31603, 31987, 32373, 32762, 33153, 33547, 33944,
    34344, 34747, 35152, 35560, 35970, 36384, 36800, 37219, 37640, 38065, 38492, 38922, 39355, 39790, 40229, 40670,
    41114, 41561, 42011, 42463, 42918, 43377, 43838, 44301, 44768, 45238, 45710, 46185, 46663, 47144, 47628, 48115,
    48605, 49097, 49593, 50091, 50592, 51096, 51604, 52114, 52627, 53142, 53661, 54183, 54708, 55235, 55766, 56300,
    56836, 57376, 57918, 58464, 59012, 59564, 60118, 60675, 61236, 61799, 62366, 62935, 63508, 64083, 64662, 65244,
];

/// Java's linear gray (16-bit) to sRGB (8-bit).
fn gray16_to_srgb(value: u16) -> u8 {
    (GRAY_TO_SRGB_THRESHOLDS.partition_point(|&threshold| threshold <= value) - 1) as u8
}

pub(crate) fn gray8_to_srgb(value: u8) -> u8 {
    gray16_to_srgb(u16::from(value) * 257)
}

/// `ComponentColorModel`'s 16-to-8-bit rounding.
fn narrow16(value: u16) -> u8 {
    ((u32::from(value) * 255 + 32767) / 65535) as u8
}

#[derive(Debug, Clone, Copy)]
pub struct PngLimits {
    pub max_width: u32,
    pub max_height: u32,
    pub max_compressed_bytes: usize,
}

impl Default for PngLimits {
    fn default() -> Self {
        Self { max_width: 4096, max_height: 4096, max_compressed_bytes: 64 << 20 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PngImage {
    pub width: u32,
    pub height: u32,
    /// Row-major `0xAARRGGBB` pixels.
    pub argb: Vec<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ColorType {
    Gray,
    Truecolor,
    Indexed,
    GrayAlpha,
    TruecolorAlpha,
}

struct Header {
    width: u32,
    height: u32,
    bit_depth: u8,
    color_type: ColorType,
    interlaced: bool,
}

/// Adam7: first column, first row, column step and row step of each pass.
const ADAM7: [(usize, usize, usize, usize); 7] = [(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)];

impl Header {
    fn channels(&self) -> usize {
        match self.color_type {
            ColorType::Gray | ColorType::Indexed => 1,
            ColorType::GrayAlpha => 2,
            ColorType::Truecolor => 3,
            ColorType::TruecolorAlpha => 4,
        }
    }

    fn bits_per_pixel(&self) -> usize {
        self.channels() * usize::from(self.bit_depth)
    }

    fn row_bytes(&self, width: usize) -> usize {
        (width * self.bits_per_pixel()).div_ceil(8)
    }

    fn filter_stride(&self) -> usize {
        self.bits_per_pixel().div_ceil(8)
    }

    /// The sub-images stored one after the other: (first x, first y, x step, y step, width, height).
    fn passes(&self) -> Vec<(usize, usize, usize, usize, usize, usize)> {
        let (width, height) = (self.width as usize, self.height as usize);
        if !self.interlaced {
            return vec![(0, 0, 1, 1, width, height)];
        }
        ADAM7
            .iter()
            .map(|&(x0, y0, dx, dy)| (x0, y0, dx, dy, width.saturating_sub(x0).div_ceil(dx), height.saturating_sub(y0).div_ceil(dy)))
            .filter(|pass| pass.4 > 0 && pass.5 > 0)
            .collect()
    }

    fn raw_size(&self) -> usize {
        self.passes().iter().map(|pass| pass.5 * (self.row_bytes(pass.4) + 1)).sum()
    }
}

fn parse_header(data: &[u8], limits: &PngLimits) -> Result<Header, DecodeError> {
    if data.len() != 13 {
        return Err(DecodeError::InvalidPng("IHDR has the wrong length"));
    }
    let width = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    let height = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    let (bit_depth, color_type, compression, filter, interlace) =
        (data[8], data[9], data[10], data[11], data[12]);
    if width == 0 || height == 0 {
        return Err(DecodeError::InvalidPng("zero image dimension"));
    }
    if width > limits.max_width || height > limits.max_height {
        return Err(DecodeError::ImageTooLarge { width, height });
    }
    if compression != 0 || filter != 0 {
        return Err(DecodeError::InvalidPng("unknown compression or filter method"));
    }
    if interlace > 1 {
        return Err(DecodeError::InvalidPng("unknown interlace method"));
    }
    let color_type = match (color_type, bit_depth) {
        (0, 1 | 2 | 4 | 8 | 16) => ColorType::Gray,
        (2, 8 | 16) => ColorType::Truecolor,
        (3, 1 | 2 | 4 | 8) => ColorType::Indexed,
        (4, 8 | 16) => ColorType::GrayAlpha,
        (6, 8 | 16) => ColorType::TruecolorAlpha,
        _ => return Err(DecodeError::InvalidPng("invalid colour type and bit depth combination")),
    };
    Ok(Header { width, height, bit_depth, color_type, interlaced: interlace == 1 })
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let pa = (p - i16::from(a)).abs();
    let pb = (p - i16::from(b)).abs();
    let pc = (p - i16::from(c)).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Reverses the row filters of one (sub-)image of `rows` rows of `row_bytes` bytes.
fn unfilter(raw: &[u8], rows: usize, row_bytes: usize, stride: usize) -> Result<Vec<u8>, DecodeError> {
    if raw.len() != rows * (row_bytes + 1) {
        return Err(DecodeError::InvalidPng("decompressed image data has the wrong size"));
    }
    let mut pixels = vec![0u8; rows * row_bytes];
    for y in 0..rows {
        let filter = raw[y * (row_bytes + 1)];
        let source = &raw[y * (row_bytes + 1) + 1..(y + 1) * (row_bytes + 1)];
        let (done, rest) = pixels.split_at_mut(y * row_bytes);
        let previous = if y == 0 { None } else { Some(&done[(y - 1) * row_bytes..]) };
        let current = &mut rest[..row_bytes];
        for i in 0..row_bytes {
            let left = if i >= stride { current[i - stride] } else { 0 };
            let up = previous.map_or(0, |row| row[i]);
            let up_left = if i >= stride { previous.map_or(0, |row| row[i - stride]) } else { 0 };
            let predictor = match filter {
                0 => 0,
                1 => left,
                2 => up,
                3 => ((u16::from(left) + u16::from(up)) / 2) as u8,
                4 => paeth(left, up, up_left),
                _ => return Err(DecodeError::InvalidPng("unknown row filter type")),
            };
            current[i] = source[i].wrapping_add(predictor);
        }
    }
    Ok(pixels)
}

/// Sample `index` of an unfiltered row, at any bit depth.
fn sample(row: &[u8], index: usize, depth: u8) -> u16 {
    match depth {
        16 => u16::from_be_bytes([row[2 * index], row[2 * index + 1]]),
        8 => u16::from(row[index]),
        _ => {
            let depth = usize::from(depth);
            let per_byte = 8 / depth;
            let shift = 8 - depth * (index % per_byte + 1);
            (u16::from(row[index / per_byte]) >> shift) & ((1 << depth) - 1)
        }
    }
}

pub fn decode_png(bytes: &[u8], limits: &PngLimits) -> Result<PngImage, DecodeError> {
    if bytes.get(..8) != Some(&SIGNATURE[..]) {
        return Err(DecodeError::InvalidPng("missing PNG signature"));
    }
    let mut offset = 8;
    let mut header: Option<Header> = None;
    let mut palette: Vec<[u8; 3]> = Vec::new();
    let mut transparency: Vec<u8> = Vec::new();
    let mut compressed: Vec<u8> = Vec::new();
    let mut ended = false;
    while !ended {
        let length_bytes = bytes.get(offset..offset + 4).ok_or(DecodeError::Truncated)?;
        let length = u32::from_be_bytes([length_bytes[0], length_bytes[1], length_bytes[2], length_bytes[3]]) as usize;
        if length > 0x7FFF_FFFF {
            return Err(DecodeError::InvalidPng("chunk length exceeds 2^31-1"));
        }
        let kind_and_data = bytes.get(offset + 4..offset + 8 + length).ok_or(DecodeError::Truncated)?;
        let crc_bytes = bytes.get(offset + 8 + length..offset + 12 + length).ok_or(DecodeError::Truncated)?;
        let expected = u32::from_be_bytes([crc_bytes[0], crc_bytes[1], crc_bytes[2], crc_bytes[3]]);
        let actual = crc32(kind_and_data);
        if expected != actual {
            return Err(DecodeError::ChecksumMismatch { kind: "PNG chunk CRC-32", expected, actual });
        }
        let kind = &kind_and_data[..4];
        let data = &kind_and_data[4..];
        match kind {
            b"IHDR" => {
                if header.is_some() {
                    return Err(DecodeError::InvalidPng("duplicate IHDR"));
                }
                header = Some(parse_header(data, limits)?);
            }
            _ if header.is_none() => return Err(DecodeError::InvalidPng("first chunk is not IHDR")),
            b"PLTE" => {
                if data.len() % 3 != 0 || data.len() > 256 * 3 {
                    return Err(DecodeError::InvalidPng("palette length is invalid"));
                }
                palette = data.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
            }
            b"tRNS" => transparency = data.to_vec(),
            b"IDAT" => {
                if compressed.len() + data.len() > limits.max_compressed_bytes {
                    return Err(DecodeError::InvalidPng("compressed image data exceeds the limit"));
                }
                compressed.extend_from_slice(data);
            }
            b"IEND" => ended = true,
            _ if kind[0] & 0x20 == 0 => {
                return Err(DecodeError::UnsupportedPng(format!(
                    "critical chunk {}",
                    String::from_utf8_lossy(kind)
                )));
            }
            _ => {}
        }
        offset += 12 + length;
    }
    let header = header.ok_or(DecodeError::InvalidPng("missing IHDR"))?;
    if header.color_type == ColorType::Indexed && palette.is_empty() {
        return Err(DecodeError::InvalidPng("indexed image without PLTE"));
    }
    let raw = zlib_decompress(&compressed, header.raw_size())?;
    if raw.len() != header.raw_size() {
        return Err(DecodeError::InvalidPng("decompressed image data has the wrong size"));
    }
    // Every sample of the full image, after de-interlacing.
    let (width, height) = (header.width as usize, header.height as usize);
    let channels = header.channels();
    let mut samples = vec![0u16; width * height * channels];
    let mut consumed = 0;
    for (x0, y0, dx, dy, pass_width, pass_height) in header.passes() {
        let row_bytes = header.row_bytes(pass_width);
        let size = pass_height * (row_bytes + 1);
        let pixels = unfilter(&raw[consumed..consumed + size], pass_height, row_bytes, header.filter_stride())?;
        consumed += size;
        for (row_index, row) in pixels.chunks_exact(row_bytes).enumerate() {
            let y = y0 + row_index * dy;
            for column in 0..pass_width {
                let x = x0 + column * dx;
                for channel in 0..channels {
                    samples[(y * width + x) * channels + channel] = sample(row, column * channels + channel, header.bit_depth);
                }
            }
        }
    }
    let key = |index: usize| transparency.get(2 * index..2 * index + 2).map(|pair| u16::from_be_bytes([pair[0], pair[1]]));
    let mut argb = Vec::with_capacity(width * height);
    match header.color_type {
        ColorType::Gray => {
            let gray_key = (transparency.len() == 2).then(|| key(0)).flatten();
            for &value in &samples {
                let pixel = match (header.bit_depth, gray_key) {
                    (16, key) => {
                        let gray = gray16_to_srgb(value);
                        (if key == Some(value) { 0 } else { 0xFF00_0000 }) | u32::from_be_bytes([0, gray, gray, gray])
                    }
                    (8, key) => {
                        let gray = gray8_to_srgb(value as u8);
                        (if key == Some(value) { 0 } else { 0xFF00_0000 }) | u32::from_be_bytes([0, gray, gray, gray])
                    }
                    (depth, None) => {
                        let gray = (u32::from(value) * 255 / ((1 << depth) - 1)) as u8;
                        u32::from_be_bytes([0xFF, gray, gray, gray])
                    }
                    (depth, Some(key)) => {
                        // Expanded to 8 bits, then compared with the raw key, as ImageIO does.
                        let expanded = (u32::from(value) * 255 / ((1 << depth) - 1)) as u16;
                        let gray = gray8_to_srgb(expanded as u8);
                        (if key == expanded { 0 } else { 0xFF00_0000 }) | u32::from_be_bytes([0, gray, gray, gray])
                    }
                };
                argb.push(pixel);
            }
        }
        ColorType::GrayAlpha => {
            for pair in samples.chunks_exact(2) {
                let (gray, alpha) = if header.bit_depth == 16 { (gray16_to_srgb(pair[0]), narrow16(pair[1])) } else { (gray8_to_srgb(pair[0] as u8), pair[1] as u8) };
                argb.push(u32::from_be_bytes([alpha, gray, gray, gray]));
            }
        }
        ColorType::Truecolor => {
            let rgb_key = (transparency.len() == 6).then(|| [key(0), key(1), key(2)]);
            for rgb in samples.chunks_exact(3) {
                let narrow = |value: u16| if header.bit_depth == 16 { narrow16(value) } else { value as u8 };
                let transparent = match rgb_key {
                    Some(key) if header.bit_depth == 16 => key == [Some(rgb[0]), Some(rgb[1]), Some(rgb[2])],
                    // 8-bit images compare the low byte of each key sample.
                    Some(key) => key.map(|k| k.map(|k| k & 0xFF)) == [Some(rgb[0]), Some(rgb[1]), Some(rgb[2])],
                    None => false,
                };
                argb.push(u32::from_be_bytes([if transparent { 0 } else { 0xFF }, narrow(rgb[0]), narrow(rgb[1]), narrow(rgb[2])]));
            }
        }
        ColorType::TruecolorAlpha => {
            for rgba in samples.chunks_exact(4) {
                let narrow = |value: u16| if header.bit_depth == 16 { narrow16(value) } else { value as u8 };
                argb.push(u32::from_be_bytes([narrow(rgba[3]), narrow(rgba[0]), narrow(rgba[1]), narrow(rgba[2])]));
            }
        }
        ColorType::Indexed => {
            for &index in &samples {
                let index = usize::from(index);
                let rgb = palette.get(index).ok_or(DecodeError::InvalidPng("palette index out of range"))?;
                let alpha = transparency.get(index).copied().unwrap_or(0xFF);
                argb.push(u32::from_be_bytes([alpha, rgb[0], rgb[1], rgb[2]]));
            }
        }
    }
    Ok(PngImage { width: header.width, height: header.height, argb })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checksum::adler32;

    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = (data.len() as u32).to_be_bytes().to_vec();
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
        out
    }

    fn stored_zlib(raw: &[u8]) -> Vec<u8> {
        let mut out = vec![0x78, 0x01];
        let blocks: Vec<&[u8]> = raw.chunks(65_535).collect();
        for (i, block) in blocks.iter().enumerate() {
            out.push(u8::from(i + 1 == blocks.len()));
            let length = block.len() as u16;
            out.extend_from_slice(&length.to_le_bytes());
            out.extend_from_slice(&(!length).to_le_bytes());
            out.extend_from_slice(block);
        }
        out.extend_from_slice(&adler32(raw).to_be_bytes());
        out
    }

    fn encode(width: u32, height: u32, color_type: u8, depth: u8, raw: &[u8], extra: &[Vec<u8>]) -> Vec<u8> {
        let mut ihdr = width.to_be_bytes().to_vec();
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[depth, color_type, 0, 0, 0]);
        let mut png = SIGNATURE.to_vec();
        png.extend(chunk(b"IHDR", &ihdr));
        for c in extra {
            png.extend_from_slice(c);
        }
        png.extend(chunk(b"IDAT", &stored_zlib(raw)));
        png.extend(chunk(b"IEND", &[]));
        png
    }

    #[test]
    fn decodes_all_filters_for_rgb() {
        // 2x5 image, one row per filter type, all encoding the same pixels [(10,20,30), (40,50,60)].
        let pixels = [10u8, 20, 30, 40, 50, 60];
        let mut raw = Vec::new();
        let mut previous = [0u8; 6];
        for filter in 0..5u8 {
            raw.push(filter);
            for i in 0..6 {
                let left = if i >= 3 { pixels[i - 3] } else { 0 };
                let up = previous[i];
                let up_left = if i >= 3 { previous[i - 3] } else { 0 };
                let predictor = match filter {
                    0 => 0,
                    1 => left,
                    2 => up,
                    3 => ((u16::from(left) + u16::from(up)) / 2) as u8,
                    _ => paeth(left, up, up_left),
                };
                raw.push(pixels[i].wrapping_sub(predictor));
            }
            previous = pixels;
        }
        let image = decode_png(&encode(2, 5, 2, 8, &raw, &[]), &PngLimits::default()).expect("valid png");
        assert_eq!(image.argb.len(), 10);
        for pair in image.argb.chunks(2) {
            assert_eq!(pair, [0xFF0A_141E, 0xFF28_323C]);
        }
    }

    #[test]
    fn indexed_with_transparency() {
        let plte = chunk(b"PLTE", &[1, 2, 3, 4, 5, 6]);
        let trns = chunk(b"tRNS", &[0x00]);
        let raw = [0u8, 0b0100_0000];
        let image = decode_png(&encode(2, 1, 3, 1, &raw, &[plte, trns]), &PngLimits::default()).expect("valid png");
        assert_eq!(image.argb, vec![0x0001_0203, 0xFF04_0506]);
    }

    #[test]
    fn gray_curve_matches_srgb() {
        let srgb = |linear: f64| if linear <= 0.003_130_8 { 12.92 * linear } else { 1.055 * linear.powf(1.0 / 2.4) - 0.055 };
        for value in 0..=u16::MAX {
            // Away from the rounding edges the formula and the recorded table must agree.
            let exact = srgb(f64::from(value) / 65535.0) * 255.0;
            if (exact.fract() - 0.5).abs() > 1e-6 {
                assert_eq!(gray16_to_srgb(value), (exact + 0.5) as u8, "{value}");
            }
        }
        assert_eq!((gray8_to_srgb(0), gray8_to_srgb(10), gray8_to_srgb(255)), (0, 0x38, 0xFF));
        assert_eq!((narrow16(128), narrow16(255), narrow16(32767), narrow16(32768), narrow16(65407)), (0, 1, 127, 128, 255));
    }

    #[test]
    fn gray_alpha_sixteen_bit_and_interlaced() {
        // 8-bit gray with a tRNS key.
        let image = decode_png(&encode(4, 1, 0, 8, &[0, 0, 5, 6, 255], &[chunk(b"tRNS", &[0, 5])]), &PngLimits::default()).unwrap();
        assert_eq!(image.argb, vec![0xFF00_0000, 0x0026_2626, 0xFF2A_2A2A, 0xFFFF_FFFF]);
        // 2-bit gray: plain scaling, or the curve and an expanded-value key with tRNS.
        assert_eq!(decode_png(&encode(4, 1, 0, 2, &[0, 0b0001_1011], &[]), &PngLimits::default()).unwrap().argb, vec![0xFF00_0000, 0xFF55_5555, 0xFFAA_AAAA, 0xFFFF_FFFF]);
        let keyed = decode_png(&encode(4, 1, 0, 2, &[0, 0b0001_1011], &[chunk(b"tRNS", &[0, 170])]), &PngLimits::default()).unwrap();
        assert_eq!(keyed.argb, vec![0xFF00_0000, 0xFF9C_9C9C, 0x00D5_D5D5, 0xFFFF_FFFF]);
        // 16-bit truecolour with a key, and gray+alpha.
        let rgb16 = [0u8, 0, 1, 0, 2, 0, 3, 0, 1, 0, 2, 0, 4];
        assert_eq!(decode_png(&encode(2, 1, 2, 16, &rgb16, &[chunk(b"tRNS", &[0, 1, 0, 2, 0, 3])]), &PngLimits::default()).unwrap().argb, vec![0, 0xFF00_0000]);
        assert_eq!(decode_png(&encode(2, 1, 4, 8, &[0, 10, 255, 20, 128], &[]), &PngLimits::default()).unwrap().argb, vec![0xFF38_3838, 0x804F_4F4F]);
        // Adam7: a 3x3 gray image stored as passes 1 (0,0), 4 (2,0), 5 (0,2),(2,2), 6 (1,0),(1,2), 7 row 1.
        // 4-bit samples, packed high nibble first.
        let interlaced = [0, 0x00, /* pass 4 */ 0, 0x20, /* pass 5 */ 0, 0x68, /* pass 6 */ 0, 0x10, 0, 0x70, /* pass 7 */ 0, 0x34, 0x50];
        let mut ihdr = 3u32.to_be_bytes().to_vec();
        ihdr.extend_from_slice(&3u32.to_be_bytes());
        ihdr.extend_from_slice(&[4, 0, 0, 0, 1]);
        let mut png = SIGNATURE.to_vec();
        png.extend(chunk(b"IHDR", &ihdr));
        png.extend(chunk(b"IDAT", &stored_zlib(&interlaced)));
        png.extend(chunk(b"IEND", &[]));
        let image = decode_png(&png, &PngLimits::default()).unwrap();
        assert_eq!(image.argb.iter().map(|p| (p & 0xFF) as u8 / 17).collect::<Vec<_>>(), vec![0, 1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn rejects_corruption_and_limits() {
        let raw = [0u8, 1, 2, 3];
        let good = encode(1, 1, 2, 8, &raw, &[]);
        assert!(decode_png(&good, &PngLimits::default()).is_ok());
        let mut bad_crc = good.clone();
        let last = bad_crc.len() - 1;
        bad_crc[last] ^= 1;
        assert!(decode_png(&bad_crc, &PngLimits::default()).is_err());
        let limits = PngLimits { max_width: 0, ..PngLimits::default() };
        assert!(matches!(decode_png(&good, &limits), Err(DecodeError::ImageTooLarge { .. })));
        for cut in [7, 20, good.len() - 13] {
            assert!(decode_png(&good[..cut], &PngLimits::default()).is_err());
        }
        assert!(matches!(
            decode_png(&encode(1, 1, 0, 3, &[0, 7], &[]), &PngLimits::default()),
            Err(DecodeError::InvalidPng(_))
        ));
    }
}
