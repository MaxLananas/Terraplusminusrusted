//! PNG decoding to packed ARGB, matching `BufferedImage.getRGB` for the colour types it accepts.
//!
//! Supported: non-interlaced truecolour (8-bit), truecolour+alpha (8-bit) and indexed (1/2/4/8-bit)
//! images, with `tRNS` transparency. Grayscale and 16-bit images are rejected explicitly because
//! `ImageIO` applies colour-space conversions to them that have not been verified against the oracle.

use crate::checksum::crc32;
use crate::zlib::zlib_decompress;
use crate::DecodeError;

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

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
    Truecolor,
    Indexed,
    TruecolorAlpha,
}

struct Header {
    width: u32,
    height: u32,
    bit_depth: u8,
    color_type: ColorType,
}

impl Header {
    fn channels(&self) -> usize {
        match self.color_type {
            ColorType::Truecolor => 3,
            ColorType::Indexed => 1,
            ColorType::TruecolorAlpha => 4,
        }
    }

    fn bits_per_pixel(&self) -> usize {
        self.channels() * usize::from(self.bit_depth)
    }

    fn row_bytes(&self) -> usize {
        (self.width as usize * self.bits_per_pixel()).div_ceil(8)
    }

    fn filter_stride(&self) -> usize {
        self.bits_per_pixel().div_ceil(8)
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
    if interlace != 0 {
        return Err(DecodeError::UnsupportedPng("Adam7 interlacing".into()));
    }
    let color_type = match (color_type, bit_depth) {
        (2, 8) => ColorType::Truecolor,
        (6, 8) => ColorType::TruecolorAlpha,
        (3, 1 | 2 | 4 | 8) => ColorType::Indexed,
        (0 | 4, _) => {
            return Err(DecodeError::UnsupportedPng(format!(
                "grayscale colour type {color_type} (ImageIO converts it through a gray colour space)"
            )));
        }
        (2 | 6, 16) => return Err(DecodeError::UnsupportedPng("16-bit samples".into())),
        _ => return Err(DecodeError::InvalidPng("invalid colour type and bit depth combination")),
    };
    Ok(Header { width, height, bit_depth, color_type })
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

fn unfilter(header: &Header, raw: &[u8]) -> Result<Vec<u8>, DecodeError> {
    let row_bytes = header.row_bytes();
    let stride = header.filter_stride();
    let height = header.height as usize;
    if raw.len() != height * (row_bytes + 1) {
        return Err(DecodeError::InvalidPng("decompressed image data has the wrong size"));
    }
    let mut pixels = vec![0u8; height * row_bytes];
    for y in 0..height {
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
    let expected_raw = header.height as usize * (header.row_bytes() + 1);
    let raw = zlib_decompress(&compressed, expected_raw)?;
    let pixels = unfilter(&header, &raw)?;
    let row_bytes = header.row_bytes();
    let (width, height) = (header.width as usize, header.height as usize);
    let mut argb = Vec::with_capacity(width * height);
    match header.color_type {
        ColorType::Truecolor => {
            let transparent = (transparency.len() == 6)
                .then(|| [transparency[1], transparency[3], transparency[5]]);
            for row in pixels.chunks_exact(row_bytes) {
                for rgb in row.chunks_exact(3) {
                    let alpha = if Some([rgb[0], rgb[1], rgb[2]]) == transparent { 0 } else { 0xFF };
                    argb.push(u32::from_be_bytes([alpha, rgb[0], rgb[1], rgb[2]]));
                }
            }
        }
        ColorType::TruecolorAlpha => {
            for row in pixels.chunks_exact(row_bytes) {
                for rgba in row.chunks_exact(4) {
                    argb.push(u32::from_be_bytes([rgba[3], rgba[0], rgba[1], rgba[2]]));
                }
            }
        }
        ColorType::Indexed => {
            if palette.is_empty() {
                return Err(DecodeError::InvalidPng("indexed image without PLTE"));
            }
            let depth = usize::from(header.bit_depth);
            let per_byte = 8 / depth;
            let mask = (1u16 << depth) - 1;
            for row in pixels.chunks_exact(row_bytes) {
                for x in 0..width {
                    let byte = row[x / per_byte];
                    let shift = 8 - depth * (x % per_byte + 1);
                    let index = usize::from((u16::from(byte) >> shift) & mask);
                    let rgb = palette.get(index).ok_or(DecodeError::InvalidPng("palette index out of range"))?;
                    let alpha = transparency.get(index).copied().unwrap_or(0xFF);
                    argb.push(u32::from_be_bytes([alpha, rgb[0], rgb[1], rgb[2]]));
                }
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
            decode_png(&encode(1, 1, 0, 8, &[0, 7], &[]), &PngLimits::default()),
            Err(DecodeError::UnsupportedPng(_))
        ));
    }
}
