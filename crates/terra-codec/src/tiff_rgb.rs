//! TIFF to ARGB, as TerraMinusMinus' `ParseTiffISP` gets it: Apache Commons Imaging
//! 1.0.0-alpha5 `TiffImageParser.getBufferedImage` on the first image directory, then
//! `BufferedImage.getRGB`. Commons Imaging's own pixel rules are reproduced, quirks included,
//! because they decide the integers a heights config sees:
//!
//! * samples wider than 8 bits keep their top byte; narrower ones are shifted up and, when their
//!   lowest bit is set, filled with ones (4-bit `0011` becomes `0x3F`);
//! * horizontal differencing (predictor 2) is undone on those narrowed bytes, modulo 256;
//! * floating-point gray samples hand their raw IEEE bits to the gray interpreter;
//! * premultiplied (associated) alpha is divided out the way `DirectColorModel` does it, in
//!   single precision, letting components above the alpha bleed into the next byte;
//! * gray, RGB, palette, CMYK and YCbCr (without subsampling) photometrics, strips or tiles,
//!   chunky or (24-bit RGB strips only) planar, uncompressed, LZW, Deflate or PackBits, either
//!   fill order, classic TIFF or BigTIFF.
//!
//! Commons Imaging can also read CCITT fax, JPEG-in-TIFF (with its own JPEG decoder), CIELab
//! and LogLuv images; those are refused with [`DecodeError::UnsupportedTiff`] rather than
//! decoded approximately. Corrupt data that Commons Imaging would turn into arbitrary pixels
//! (strips shorter than declared) is refused too.

use crate::error::DecodeError;
use crate::png::PngImage;
use crate::tiff::{Directory, Reader, TiffLimits, decompress, invalid, limit_exceeded, read_directories_limited, unsupported};

const TAG_WIDTH: u16 = 256;
const TAG_HEIGHT: u16 = 257;
const TAG_BITS_PER_SAMPLE: u16 = 258;
const TAG_COMPRESSION: u16 = 259;
const TAG_PHOTOMETRIC: u16 = 262;
const TAG_FILL_ORDER: u16 = 266;
const TAG_STRIP_OFFSETS: u16 = 273;
const TAG_SAMPLES_PER_PIXEL: u16 = 277;
const TAG_ROWS_PER_STRIP: u16 = 278;
const TAG_STRIP_BYTE_COUNTS: u16 = 279;
const TAG_PLANAR_CONFIGURATION: u16 = 284;
const TAG_PREDICTOR: u16 = 317;
const TAG_COLOR_MAP: u16 = 320;
const TAG_TILE_WIDTH: u16 = 322;
const TAG_TILE_LENGTH: u16 = 323;
const TAG_TILE_OFFSETS: u16 = 324;
const TAG_TILE_BYTE_COUNTS: u16 = 325;
const TAG_EXTRA_SAMPLES: u16 = 338;
const TAG_SAMPLE_FORMAT: u16 = 339;

/// `PhotometricInterpreter*`: one pixel's samples (already narrowed to bytes) to ARGB.
enum Interpreter {
    /// MinIsWhite (0, inverted) or MinIsBlack (1): the first sample only.
    Gray { invert: bool },
    Rgb,
    Palette { colors: Vec<u32>, mask: i32 },
    Cmyk,
    YCbCr,
}

impl Interpreter {
    /// Samples the interpreter reads; Commons Imaging fails on fewer.
    fn samples_needed(&self) -> usize {
        match self {
            Self::Gray { .. } | Self::Palette { .. } => 1,
            Self::Rgb | Self::YCbCr => 3,
            Self::Cmyk => 4,
        }
    }

    fn pixel(&self, samples: &[i32]) -> u32 {
        match self {
            Self::Gray { invert } => {
                // Java `int` arithmetic: a sample wrapped by the predictor wraps here too.
                let sample = if *invert { 255i32.wrapping_sub(samples[0]) } else { samples[0] };
                (0xFF_i32 << 24 | sample.wrapping_shl(16) | sample.wrapping_shl(8) | sample) as u32
            }
            Self::Rgb => (0xFF_i32 << 24 | samples[0].wrapping_shl(16) | samples[1].wrapping_shl(8) | samples[2]) as u32,
            Self::Palette { colors, mask } => colors[(samples[0] & mask) as usize],
            Self::Cmyk => {
                // ColorConversions.convertCmykToRgb: CMYK to CMY, then Ghostscript's (1 - C) * 255.
                let k = f64::from(samples[3]) / 255.0;
                let channel = |sample: i32| {
                    let cmy = (f64::from(sample) / 255.0) * (1.0 - k) + k;
                    java_round((1.0 - cmy) * 255.0).clamp(0, 255)
                };
                0xFF00_0000 | (channel(samples[0]) as u32) << 16 | (channel(samples[1]) as u32) << 8 | channel(samples[2]) as u32
            }
            Self::YCbCr => {
                let (y, cb, cr) = (f64::from(samples[0]), f64::from(samples[1]), f64::from(samples[2]));
                // Java's (int) cast truncates toward zero (and saturates), then clamps.
                let limit = |value: f64| (value as i32).clamp(0, 255) as u32;
                let red = limit(y + 1.402 * (cr - 128.0));
                let green = limit(y - 0.34414 * (cb - 128.0) - 0.71414 * (cr - 128.0));
                let blue = limit(y + 1.772 * (cb - 128.0));
                0xFF00_0000 | red << 16 | green << 8 | blue
            }
        }
    }
}

/// `Math.round(double)`: the closest integer, ties toward positive infinity (exact, unlike
/// `floor(x + 0.5)`).
fn java_round(value: f64) -> i64 {
    let floor = value.floor();
    (if value - floor >= 0.5 { floor + 1.0 } else { floor }) as i64
}

/// How `getRGB` reads the builder's pixels back: no alpha (forced opaque), straight alpha, or
/// premultiplied alpha divided out by `DirectColorModel`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AlphaMode {
    Opaque,
    Straight,
    Premultiplied,
}

fn read_back(pixel: u32, mode: AlphaMode) -> u32 {
    match mode {
        AlphaMode::Opaque => 0xFF00_0000 | (pixel & 0x00FF_FFFF),
        AlphaMode::Straight => pixel,
        AlphaMode::Premultiplied => {
            // DirectColorModel.getsRGBComponentFromsRGB with 8-bit scale factors of 1.0f, then
            // getRGB's `a << 24 | r << 16 | g << 8 | b`, overflowing components included.
            let alpha = (pixel >> 24) as i32;
            let component = |shift: u32| -> i32 {
                let value = ((pixel >> shift) & 0xFF) as i32;
                if alpha == 0 { 0 } else { ((value as f32 * 255.0f32) / alpha as f32 + 0.5f32) as i32 }
            };
            (alpha << 24 | component(16) << 16 | component(8) << 8 | component(0)) as u32
        }
    }
}

/// Commons Imaging's `BitInputStream` over one decoded segment: sub-byte fields MSB first and
/// never across a byte, wider ones (16, 24, 32 bits) in the file's byte order.
struct Bits<'a> {
    bytes: &'a [u8],
    position: usize,
    cache: u32,
    cache_bits: u32,
    little: bool,
}

impl Bits<'_> {
    fn byte(&mut self) -> Result<u32, DecodeError> {
        let byte = *self.bytes.get(self.position).ok_or(DecodeError::Truncated)?;
        self.position += 1;
        Ok(u32::from(byte))
    }

    fn read(&mut self, count: u32) -> Result<i32, DecodeError> {
        if count < 8 {
            if self.cache_bits == 0 {
                self.cache = self.byte()?;
                self.cache_bits = 8;
            }
            if count > self.cache_bits {
                return Err(unsupported("bit fields across bytes"));
            }
            self.cache_bits -= count;
            return Ok(((self.cache >> self.cache_bits) & ((1 << count) - 1)) as i32);
        }
        if self.cache_bits > 0 {
            return Err(unsupported("incomplete bit read"));
        }
        let bytes = match count {
            8 | 16 | 24 | 32 => count / 8,
            other => return Err(unsupported(format!("{other}-bit samples"))),
        };
        let mut value = 0u32;
        for index in 0..bytes {
            let byte = self.byte()?;
            value |= if self.little { byte << (8 * index) } else { byte << (8 * (bytes - 1 - index)) };
        }
        Ok(value as i32)
    }

    fn flush(&mut self) {
        self.cache_bits = 0;
    }
}

/// Everything `getBufferedImage` derives from the directory before reading pixels.
struct Layout {
    width: usize,
    height: usize,
    bits: Vec<u32>,
    bits_per_pixel: u64,
    predictor: i64,
    float: bool,
    interpreter: Interpreter,
    alpha: AlphaMode,
    compression: u64,
    reverse_fill: bool,
    little: bool,
}

impl Layout {
    fn samples(&self) -> usize {
        self.bits.len()
    }

    /// `getSamplesAsBytes`: each sample read and narrowed to (roughly) a byte.
    fn read_samples(&self, bits: &mut Bits<'_>, samples: &mut [i32]) -> Result<(), DecodeError> {
        for (sample, &width) in samples.iter_mut().zip(&self.bits) {
            let mut value = bits.read(width)?;
            if width < 8 {
                let sign = value & 1;
                value <<= 8 - width;
                if sign > 0 {
                    value |= (1 << (8 - width)) - 1;
                }
            } else if width > 8 {
                value >>= width - 8;
            }
            *sample = value;
        }
        Ok(())
    }
}

/// Decodes `bytes` the way `ParseTiffISP` does, into ARGB. Only the first image directory is
/// considered, as in Commons Imaging.
pub fn decode_tiff_argb(bytes: &[u8], limits: &TiffLimits) -> Result<PngImage, DecodeError> {
    let (reader, first) = Reader::open(bytes)?;
    let directories = read_directories_limited(reader, first, 1)?;
    let directory = directories.first().ok_or(invalid("no image directory"))?;
    let layout = layout(reader, directory, limits)?;
    let mut argb = Vec::new();
    argb.try_reserve_exact(layout.width * layout.height).map_err(|_| DecodeError::LimitExceeded("TIFF image allocation failed".into()))?;
    argb.resize(layout.width * layout.height, 0u32);
    let planar = directory.integer(reader, TAG_PLANAR_CONFIGURATION)? == Some(2);
    if directory.find(TAG_TILE_OFFSETS).is_some() && directory.find(TAG_TILE_BYTE_COUNTS).is_some() {
        if planar {
            return Err(unsupported("planar configuration 2 with tiles"));
        }
        read_tiles(reader, directory, &layout, limits, &mut argb)?;
    } else if directory.find(TAG_STRIP_OFFSETS).is_some() && directory.find(TAG_STRIP_BYTE_COUNTS).is_some() {
        read_strips(reader, directory, &layout, limits, planar, &mut argb)?;
    } else {
        return Err(invalid("TIFF does not contain an image"));
    }
    for pixel in &mut argb {
        *pixel = read_back(*pixel, layout.alpha);
    }
    Ok(PngImage { width: layout.width as u32, height: layout.height as u32, argb })
}

fn layout(reader: Reader<'_>, directory: &Directory, limits: &TiffLimits) -> Result<Layout, DecodeError> {
    let width = directory.integer(reader, TAG_WIDTH)?.ok_or(invalid("missing ImageWidth"))?;
    let height = directory.integer(reader, TAG_HEIGHT)?.ok_or(invalid("missing ImageLength"))?;
    let pixels = width.checked_mul(height).ok_or(invalid("image too large"))?;
    if width == 0 || height == 0 || pixels > limits.max_pixels || width > i32::MAX as u64 || height > i32::MAX as u64 {
        return Err(DecodeError::ImageTooLarge { width: width.min(u64::from(u32::MAX)) as u32, height: height.min(u64::from(u32::MAX)) as u32 });
    }
    let compression = directory.integer(reader, TAG_COMPRESSION)?.unwrap_or(1);
    match compression {
        1 | 5 | 8 | 32946 | 32773 => {}
        2..=4 => return Err(unsupported(format!("CCITT compression {compression}"))),
        6 | 7 => return Err(unsupported("JPEG-compressed TIFF (Commons Imaging decodes it with its own JPEG decoder)")),
        other => return Err(unsupported(format!("compression {other}"))),
    }
    let samples = directory.integer(reader, TAG_SAMPLES_PER_PIXEL)?.unwrap_or(1);
    let (bits, bits_per_pixel) = match directory.integers_limited(reader, TAG_BITS_PER_SAMPLE, 64)? {
        Some(values) => {
            let sum = values.iter().sum::<u64>();
            (values, sum)
        }
        None => (vec![1], samples),
    };
    if samples != bits.len() as u64 {
        return Err(invalid("SamplesPerPixel does not match BitsPerSample"));
    }
    // 64 bits only make sense for floating-point samples (checked below); the bit reader refuses
    // other widths it cannot read.
    if bits.iter().any(|&width| width == 0 || (width > 32 && width != 64)) {
        return Err(unsupported("sample width outside 1 to 32 bits"));
    }
    let bits: Vec<u32> = bits.iter().map(|&width| width as u32).collect();
    let predictor = directory.integer(reader, TAG_PREDICTOR)?.map_or(-1, |value| value as i64);
    let photometric = directory.integer(reader, TAG_PHOTOMETRIC)?.ok_or(invalid("missing PhotometricInterpretation"))?;
    let alpha = if photometric == 2 && samples == 4 {
        match directory.integer(reader, TAG_EXTRA_SAMPLES)? {
            None | Some(2) => AlphaMode::Straight,
            Some(1) => AlphaMode::Premultiplied,
            Some(_) => AlphaMode::Opaque,
        }
    } else {
        AlphaMode::Opaque
    };
    let interpreter = match photometric {
        0 | 1 => Interpreter::Gray { invert: photometric == 0 },
        2 => Interpreter::Rgb,
        3 => {
            if bits_per_pixel > 16 {
                return Err(unsupported(format!("{bits_per_pixel}-bit palette indices")));
            }
            let entries = 1usize << bits_per_pixel;
            let map = directory.integers_limited(reader, TAG_COLOR_MAP, 3 * 65536)?.ok_or(invalid("palette image without a ColorMap"))?;
            if map.len() != 3 * entries {
                return Err(invalid("ColorMap size does not match the bits per pixel"));
            }
            let index_bits = bits[0];
            let table = 1usize << index_bits;
            if 3 * table > map.len() {
                return Err(invalid("bits per sample too large for the ColorMap"));
            }
            let colors = (0..table)
                .map(|i| {
                    let channel = |at: usize| ((map[at] >> 8) & 0xFF) as u32;
                    0xFF00_0000 | channel(i) << 16 | channel(i + table) << 8 | channel(i + 2 * table)
                })
                .collect();
            Interpreter::Palette { colors, mask: ((1u32 << index_bits) - 1) as i32 }
        }
        5 => Interpreter::Cmyk,
        6 => Interpreter::YCbCr,
        8 => return Err(unsupported("CIELab photometric interpretation")),
        32844 | 32845 => return Err(unsupported("LogLuv photometric interpretation")),
        other => return Err(invalid_photometric(other)),
    };
    if bits.len() < interpreter.samples_needed() {
        return Err(invalid("fewer samples than the photometric interpretation needs"));
    }
    let float = directory.integer(reader, TAG_SAMPLE_FORMAT)? == Some(3);
    if float && (!matches!(interpreter, Interpreter::Gray { .. }) || bits.len() != 1 || !matches!(bits[0], 32 | 64) || (predictor == 3 && bits[0] != 32)) {
        return Err(unsupported("floating-point samples other than 32 or 64-bit gray"));
    }
    let reverse_fill = match directory.integer(reader, TAG_FILL_ORDER)?.unwrap_or(1) {
        1 => false,
        2 => true,
        _ => return Err(invalid("invalid FillOrder")),
    };
    Ok(Layout {
        width: width as usize,
        height: height as usize,
        bits,
        bits_per_pixel,
        predictor,
        float,
        interpreter,
        alpha,
        compression,
        reverse_fill,
        little: reader.little,
    })
}

fn invalid_photometric(value: u64) -> DecodeError {
    DecodeError::UnsupportedTiff(format!("unknown photometric interpretation {value}"))
}

/// `ImageDataReader.decompress`: fill order 2 reverses the bits of every byte first (except for
/// Deflate, which Commons Imaging feeds the original bytes).
fn decode_segment(layout: &Layout, compressed: &[u8], expected: usize) -> Result<Vec<u8>, DecodeError> {
    let reordered: Vec<u8>;
    let input = if layout.reverse_fill && !matches!(layout.compression, 8 | 32946) {
        reordered = compressed.iter().map(|byte| byte.reverse_bits()).collect();
        &reordered[..]
    } else {
        compressed
    };
    let decoded = if layout.compression == 32773 { packbits_strict(input, expected)? } else { decompress(input, layout.compression, expected)? };
    if decoded.len() < expected {
        return Err(DecodeError::Truncated);
    }
    Ok(decoded)
}

/// Commons Imaging's `PackBits.decompress`: runs until `expected` bytes, refusing the -128 code
/// and exhausted input.
fn packbits_strict(input: &[u8], expected: usize) -> Result<Vec<u8>, DecodeError> {
    let mut out = Vec::new();
    out.try_reserve(expected.min(input.len().saturating_mul(128))).map_err(|_| DecodeError::LimitExceeded("PackBits output allocation failed".into()))?;
    let mut position = 0;
    while out.len() < expected {
        let header = *input.get(position).ok_or(DecodeError::Truncated)? as i8;
        position += 1;
        match header {
            0..=127 => {
                let count = header as usize + 1;
                out.extend_from_slice(input.get(position..position + count).ok_or(DecodeError::Truncated)?);
                position += count;
            }
            -127..=-1 => {
                let byte = *input.get(position).ok_or(DecodeError::Truncated)?;
                out.resize(out.len() + (1 - i32::from(header)) as usize, byte);
                position += 1;
            }
            -128 => return Err(invalid("PackBits code -128")),
        }
    }
    Ok(out)
}

/// The segment offsets and byte counts, checked against the expected number of segments.
fn segment_table(reader: Reader<'_>, directory: &Directory, tags: (u16, u16), segments: u64, limits: &TiffLimits) -> Result<Vec<(u64, u64)>, DecodeError> {
    if segments > limits.max_segments {
        return Err(limit_exceeded("segment count", segments, limits.max_segments));
    }
    let field_limit = segments.min(limits.max_field_values);
    let offsets = directory.integers_limited(reader, tags.0, field_limit)?.ok_or(invalid("missing segment offsets"))?;
    let counts = directory.integers_limited(reader, tags.1, field_limit)?.ok_or(invalid("missing segment byte counts"))?;
    if (offsets.len() as u64) < segments || counts.len() < offsets.len() {
        return Err(invalid("segment offsets do not cover the image"));
    }
    Ok(offsets.into_iter().zip(counts).collect())
}

fn check_segment_size(bytes: u64, limits: &TiffLimits) -> Result<usize, DecodeError> {
    if bytes > limits.max_segment_bytes {
        return Err(limit_exceeded("decoded segment size", bytes, limits.max_segment_bytes));
    }
    Ok(bytes as usize)
}

fn read_strips(reader: Reader<'_>, directory: &Directory, layout: &Layout, limits: &TiffLimits, planar: bool, argb: &mut [u32]) -> Result<(), DecodeError> {
    let (width, height) = (layout.width as u64, layout.height as u64);
    let rows_per_strip = directory.integer(reader, TAG_ROWS_PER_STRIP)?.unwrap_or(height).clamp(1, height);
    let strips = height.div_ceil(rows_per_strip);
    let bytes_per_row = (layout.bits_per_pixel * width).div_ceil(8);
    if planar {
        if !matches!(layout.interpreter, Interpreter::Rgb) || layout.bits_per_pixel != 24 {
            return Err(unsupported("planar configuration 2 other than 24-bit RGB"));
        }
        let table = segment_table(reader, directory, (TAG_STRIP_OFFSETS, TAG_STRIP_BYTE_COUNTS), strips * 3, limits)?;
        let per_plane = table.len() / 3;
        for strip in 0..strips {
            let top = strip * rows_per_strip;
            let rows = rows_per_strip.min(height - top);
            let size = check_segment_size(rows * bytes_per_row, limits)?;
            let mut interleaved = vec![0u8; size];
            for plane in 0..3 {
                let (offset, count) = table[plane * per_plane + strip as usize];
                // Each plane holds a third of the strip's bytes (Commons Imaging asks for the whole
                // strip and accepts the shorter result).
                let decoded = decode_segment(layout, reader.slice(offset, count)?, size / 3)?;
                for (index, &byte) in decoded.iter().take(size / 3).enumerate() {
                    interleaved[plane + 3 * index] = byte;
                }
            }
            interpret_block(layout, &interleaved, 0, top as usize, layout.width, rows as usize, argb)?;
        }
        return Ok(());
    }
    let table = segment_table(reader, directory, (TAG_STRIP_OFFSETS, TAG_STRIP_BYTE_COUNTS), strips, limits)?;
    for strip in 0..strips {
        let top = strip * rows_per_strip;
        let rows = rows_per_strip.min(height - top);
        let size = check_segment_size(rows * bytes_per_row, limits)?;
        let (offset, count) = table[strip as usize];
        let decoded = decode_segment(layout, reader.slice(offset, count)?, size)?;
        interpret_block(layout, &decoded, 0, top as usize, layout.width, rows as usize, argb)?;
    }
    Ok(())
}

fn read_tiles(reader: Reader<'_>, directory: &Directory, layout: &Layout, limits: &TiffLimits, argb: &mut [u32]) -> Result<(), DecodeError> {
    let tile_width = directory.integer(reader, TAG_TILE_WIDTH)?.ok_or(invalid("missing TileWidth"))?;
    let tile_length = directory.integer(reader, TAG_TILE_LENGTH)?.ok_or(invalid("missing TileLength"))?;
    if tile_width == 0 || tile_length == 0 || tile_width > 1 << 16 || tile_length > 1 << 16 {
        return Err(invalid("invalid tile size"));
    }
    let across = (layout.width as u64).div_ceil(tile_width);
    let down = (layout.height as u64).div_ceil(tile_length);
    let size = check_segment_size((tile_width * layout.bits_per_pixel).div_ceil(8) * tile_length, limits)?;
    let total = size as u64 * across * down;
    if total > limits.max_decoded_bytes {
        return Err(limit_exceeded("total decoded size", total, limits.max_decoded_bytes));
    }
    let table = segment_table(reader, directory, (TAG_TILE_OFFSETS, TAG_TILE_BYTE_COUNTS), across * down, limits)?;
    for row in 0..down {
        for column in 0..across {
            let (offset, count) = table[(row * across + column) as usize];
            let decoded = decode_segment(layout, reader.slice(offset, count)?, size)?;
            let (left, top) = ((column * tile_width) as usize, (row * tile_length) as usize);
            interpret_block(layout, &decoded, left, top, tile_width as usize, tile_length as usize, argb)?;
        }
    }
    Ok(())
}

/// `interpretStrip` / `interpretTile`: a decoded block of `block_width` by `block_rows` pixels
/// whose top left is at (`left`, `top`); pixels outside the image are skipped.
fn interpret_block(layout: &Layout, bytes: &[u8], left: usize, top: usize, block_width: usize, block_rows: usize, argb: &mut [u32]) -> Result<(), DecodeError> {
    let visible_rows = block_rows.min(layout.height - top);
    let visible_columns = block_width.min(layout.width - left);
    let mut put = |x: usize, y: usize, pixel: u32| argb[(top + y) * layout.width + left + x] = pixel;
    if layout.float {
        let size = (layout.bits[0] / 8) as usize;
        let mut block = bytes.to_vec();
        if layout.predictor == 3 {
            undo_float_predictor(&mut block, block_width, visible_rows)?;
        }
        // After predictor 3, samples are big-endian whatever the file's byte order.
        let little = layout.little && layout.predictor != 3;
        for y in 0..visible_rows {
            for x in 0..visible_columns {
                let at = (y * block_width + x) * size;
                let raw = block.get(at..at + size).ok_or(DecodeError::Truncated)?;
                let mut buffer = [0u8; 8];
                buffer[..size].copy_from_slice(raw);
                if !little {
                    buffer[..size].reverse();
                }
                let bits = if size == 4 { u32::from_le_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) } else { (f64::from_bits(u64::from_le_bytes(buffer)) as f32).to_bits() };
                put(x, y, layout.interpreter.pixel(&[bits as i32]));
            }
        }
        return Ok(());
    }
    let one_byte = layout.bits.iter().all(|&width| width == 8);
    if matches!(layout.interpreter, Interpreter::Rgb) && one_byte && matches!(layout.bits_per_pixel, 24 | 32) {
        // The RGB fast path: predictor 2 undone on whole bytes, a fourth sample used as alpha.
        let stride = layout.samples();
        let mut block = bytes[..(block_width * stride * visible_rows).min(bytes.len())].to_vec();
        if layout.predictor == 2 {
            for row in block.chunks_mut(block_width * stride) {
                for index in stride..row.len() {
                    row[index] = row[index].wrapping_add(row[index - stride]);
                }
            }
        }
        for y in 0..visible_rows {
            for x in 0..visible_columns {
                let at = (y * block_width + x) * stride;
                let pixel = block.get(at..at + stride).ok_or(DecodeError::Truncated)?;
                let rgb = u32::from(pixel[0]) << 16 | u32::from(pixel[1]) << 8 | u32::from(pixel[2]);
                put(x, y, if stride == 4 { rgb | u32::from(pixel[3]) << 24 } else { 0xFF00_0000 | rgb });
            }
        }
        return Ok(());
    }
    let mut bits = Bits { bytes, position: 0, cache: 0, cache_bits: 0, little: layout.little };
    let mut samples = vec![0i32; layout.samples()];
    let mut last = vec![0i32; layout.samples()];
    for y in 0..visible_rows {
        last.fill(0);
        for x in 0..block_width {
            layout.read_samples(&mut bits, &mut samples)?;
            if x < visible_columns {
                if layout.predictor == 2 {
                    for (sample, previous) in samples.iter_mut().zip(last.iter_mut()) {
                        *sample = 0xFF & sample.wrapping_add(*previous);
                        *previous = *sample;
                    }
                }
                put(x, y, layout.interpreter.pixel(&samples));
            }
        }
        bits.flush();
    }
    Ok(())
}

/// Floating-point predictor 3 on 32-bit samples (`unpackFloatingPointSamples`): bytes summed
/// along each row, then regrouped from four byte planes into big-endian samples.
fn undo_float_predictor(block: &mut [u8], block_width: usize, rows: usize) -> Result<(), DecodeError> {
    let row_bytes = block_width * 4;
    for row in block.chunks_exact_mut(row_bytes).take(rows) {
        for index in 1..row.len() {
            row[index] = row[index].wrapping_add(row[index - 1]);
        }
        let planes = row.to_vec();
        for sample in 0..block_width {
            for plane in 0..4 {
                row[sample * 4 + plane] = planes[plane * block_width + sample];
            }
        }
    }
    if block.len() < row_bytes * rows {
        return Err(DecodeError::Truncated);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrow_samples_fill_their_low_bits_when_odd() {
        let layout = |bits: Vec<u32>| Layout {
            width: 1,
            height: 1,
            bits_per_pixel: bits.iter().map(|&b| u64::from(b)).sum(),
            bits,
            predictor: -1,
            float: false,
            interpreter: Interpreter::Gray { invert: false },
            alpha: AlphaMode::Opaque,
            compression: 1,
            reverse_fill: false,
            little: true,
        };
        let mut samples = [0i32; 2];
        let four = layout(vec![4, 4]);
        four.read_samples(&mut Bits { bytes: &[0x32], position: 0, cache: 0, cache_bits: 0, little: true }, &mut samples).unwrap();
        assert_eq!(samples, [0x3F, 0x20]);
        let sixteen = layout(vec![16, 16]);
        sixteen.read_samples(&mut Bits { bytes: &[0x34, 0x12, 0xFF, 0xAB], position: 0, cache: 0, cache_bits: 0, little: true }, &mut samples).unwrap();
        assert_eq!(samples, [0x12, 0xAB]);
    }

    #[test]
    fn premultiplied_alpha_is_divided_like_direct_color_model() {
        assert_eq!(read_back(0x0000_0000, AlphaMode::Premultiplied), 0);
        assert_eq!(read_back(0x8040_2010, AlphaMode::Premultiplied), 0x8080_4020);
        // A component above its alpha overflows into the next byte, as in Java.
        assert_eq!(read_back(0x0102_0000, AlphaMode::Premultiplied), 0x0100_0000 | (510 << 16));
        assert_eq!(read_back(0x1234_5678, AlphaMode::Opaque), 0xFF34_5678);
    }

    #[test]
    fn java_round_breaks_ties_upward() {
        assert_eq!(java_round(2.5), 3);
        assert_eq!(java_round(-2.5), -2);
        assert_eq!(java_round(0.499_999_999_999_999_94), 0);
    }
}
