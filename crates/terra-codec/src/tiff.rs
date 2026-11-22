//! Single-channel scalar TIFF decoding, as TerraMinusMinus' `ParseTiffDSP` does it through
//! Apache Commons Imaging (`TiffDirectory.getRasterData`): the first grayscale, one-sample
//! directory with an integer or IEEE floating-point sample format is read into `f32` values.
//!
//! Supported: classic TIFF (not BigTIFF) in either byte order; strips or tiles; no compression,
//! LZW, Deflate (8 and 32946) and PackBits; horizontal (2) and floating-point (3) predictors;
//! 8, 16 and 32-bit integers, signed or unsigned, and 32 or 64-bit floats.

use crate::error::DecodeError;
use crate::zlib::zlib_decompress;

/// A decoded raster: `data[y * width + x]`, plus the GDAL no-data tag (42113) when present.
#[derive(Debug, Clone, PartialEq)]
pub struct TiffRaster {
    pub width: u32,
    pub height: u32,
    pub data: Vec<f32>,
    /// `GDAL_NODATA` as written, without the terminating NUL.
    pub gdal_nodata: Option<String>,
}

const MAX_DIRECTORIES: usize = 64;
const MAX_ENTRIES: usize = 4096;

const TAG_WIDTH: u16 = 256;
const TAG_HEIGHT: u16 = 257;
const TAG_BITS_PER_SAMPLE: u16 = 258;
const TAG_COMPRESSION: u16 = 259;
const TAG_PHOTOMETRIC: u16 = 262;
const TAG_STRIP_OFFSETS: u16 = 273;
const TAG_SAMPLES_PER_PIXEL: u16 = 277;
const TAG_ROWS_PER_STRIP: u16 = 278;
const TAG_STRIP_BYTE_COUNTS: u16 = 279;
const TAG_PREDICTOR: u16 = 317;
const TAG_TILE_WIDTH: u16 = 322;
const TAG_TILE_LENGTH: u16 = 323;
const TAG_TILE_OFFSETS: u16 = 324;
const TAG_TILE_BYTE_COUNTS: u16 = 325;
const TAG_SAMPLE_FORMAT: u16 = 339;
const TAG_GDAL_NODATA: u16 = 42113;

fn invalid(reason: &'static str) -> DecodeError {
    DecodeError::InvalidTiff(reason)
}

fn unsupported(reason: impl Into<String>) -> DecodeError {
    DecodeError::UnsupportedTiff(reason.into())
}

#[derive(Clone, Copy)]
struct Reader<'a> {
    bytes: &'a [u8],
    little: bool,
}

impl Reader<'_> {
    fn slice(&self, offset: u64, length: u64) -> Result<&[u8], DecodeError> {
        let start = usize::try_from(offset).map_err(|_| invalid("offset out of range"))?;
        let length = usize::try_from(length).map_err(|_| invalid("length out of range"))?;
        let end = start.checked_add(length).ok_or(invalid("offset out of range"))?;
        self.bytes.get(start..end).ok_or(DecodeError::Truncated)
    }

    fn u16_at(&self, offset: u64) -> Result<u16, DecodeError> {
        let bytes = self.slice(offset, 2)?;
        let pair = [bytes[0], bytes[1]];
        Ok(if self.little { u16::from_le_bytes(pair) } else { u16::from_be_bytes(pair) })
    }

    fn u32_at(&self, offset: u64) -> Result<u32, DecodeError> {
        let bytes = self.slice(offset, 4)?;
        let quad = [bytes[0], bytes[1], bytes[2], bytes[3]];
        Ok(if self.little { u32::from_le_bytes(quad) } else { u32::from_be_bytes(quad) })
    }
}

struct Entry {
    tag: u16,
    kind: u16,
    count: u64,
    /// Absolute offset of the value bytes (inline values point into the entry itself).
    value_offset: u64,
}

fn type_size(kind: u16) -> Option<u64> {
    match kind {
        1 | 2 | 6 | 7 => Some(1),
        3 | 8 => Some(2),
        4 | 9 | 11 => Some(4),
        5 | 10 | 12 => Some(8),
        _ => None,
    }
}

struct Directory {
    entries: Vec<Entry>,
}

impl Directory {
    fn find(&self, tag: u16) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.tag == tag)
    }

    /// An unsigned integer field (BYTE, SHORT or LONG) as values.
    fn integers(&self, reader: Reader<'_>, tag: u16) -> Result<Option<Vec<u64>>, DecodeError> {
        let Some(entry) = self.find(tag) else { return Ok(None) };
        let size = match entry.kind {
            1 => 1,
            3 => 2,
            4 => 4,
            _ => return Err(invalid("integer field has a non-integer type")),
        };
        if entry.count > (1 << 24) {
            return Err(invalid("field count too large"));
        }
        let mut values = Vec::with_capacity(entry.count as usize);
        for index in 0..entry.count {
            let offset = entry.value_offset + index * size;
            values.push(match size {
                1 => u64::from(reader.slice(offset, 1)?[0]),
                2 => u64::from(reader.u16_at(offset)?),
                _ => u64::from(reader.u32_at(offset)?),
            });
        }
        Ok(Some(values))
    }

    fn integer(&self, reader: Reader<'_>, tag: u16) -> Result<Option<u64>, DecodeError> {
        Ok(self.integers(reader, tag)?.and_then(|values| values.first().copied()))
    }

    fn ascii(&self, reader: Reader<'_>, tag: u16) -> Result<Option<String>, DecodeError> {
        let Some(entry) = self.find(tag) else { return Ok(None) };
        if entry.kind != 2 {
            return Err(invalid("ASCII field has a non-ASCII type"));
        }
        let bytes = reader.slice(entry.value_offset, entry.count)?;
        let text = bytes.split(|&byte| byte == 0).next().unwrap_or(&[]);
        Ok(Some(String::from_utf8_lossy(text).into_owned()))
    }
}

fn read_directories(reader: Reader<'_>, first: u64) -> Result<Vec<Directory>, DecodeError> {
    let mut directories = Vec::new();
    let mut seen = Vec::new();
    let mut offset = first;
    while offset != 0 {
        if directories.len() == MAX_DIRECTORIES || seen.contains(&offset) {
            return Err(invalid("too many or cyclic image directories"));
        }
        seen.push(offset);
        let count = u64::from(reader.u16_at(offset)?);
        if count as usize > MAX_ENTRIES {
            return Err(invalid("too many directory entries"));
        }
        let mut entries = Vec::with_capacity(count as usize);
        for index in 0..count {
            let at = offset + 2 + index * 12;
            let tag = reader.u16_at(at)?;
            let kind = reader.u16_at(at + 2)?;
            let count = u64::from(reader.u32_at(at + 4)?);
            let Some(size) = type_size(kind) else { continue };
            let total = size.checked_mul(count).ok_or(invalid("field too large"))?;
            let value_offset = if total <= 4 { at + 8 } else { u64::from(reader.u32_at(at + 8)?) };
            entries.push(Entry { tag, kind, count, value_offset });
        }
        directories.push(Directory { entries });
        offset = u64::from(reader.u32_at(offset + 2 + count * 12)?);
    }
    Ok(directories)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SampleKind {
    Unsigned,
    Signed,
    Float,
}

struct Layout {
    width: u64,
    height: u64,
    bytes_per_sample: u64,
    kind: SampleKind,
    compression: u64,
    predictor: u64,
}

/// `ParseTiffDSP.isSupportedFormat`: grayscale, one sample, integer or IEEE float.
fn supported_layout(reader: Reader<'_>, directory: &Directory) -> Result<Option<Layout>, DecodeError> {
    match directory.integer(reader, TAG_PHOTOMETRIC)? {
        Some(0 | 1) => {}
        _ => return Ok(None),
    }
    let bits = directory.integers(reader, TAG_BITS_PER_SAMPLE)?.unwrap_or_else(|| vec![1]);
    if bits.len() != 1 || directory.integer(reader, TAG_SAMPLES_PER_PIXEL)?.unwrap_or(1) != 1 {
        return Ok(None);
    }
    let kind = match directory.integer(reader, TAG_SAMPLE_FORMAT)?.unwrap_or(1) {
        1 => SampleKind::Unsigned,
        2 => SampleKind::Signed,
        3 => SampleKind::Float,
        _ => return Ok(None),
    };
    let bits = bits[0];
    match (kind, bits) {
        (SampleKind::Unsigned | SampleKind::Signed, 8 | 16 | 32) | (SampleKind::Float, 32 | 64) => {}
        _ => return Err(unsupported(format!("{bits}-bit {kind:?} samples"))),
    }
    let width = directory.integer(reader, TAG_WIDTH)?.ok_or(invalid("missing ImageWidth"))?;
    let height = directory.integer(reader, TAG_HEIGHT)?.ok_or(invalid("missing ImageLength"))?;
    let compression = directory.integer(reader, TAG_COMPRESSION)?.unwrap_or(1);
    let predictor = directory.integer(reader, TAG_PREDICTOR)?.unwrap_or(1);
    Ok(Some(Layout { width, height, bytes_per_sample: bits / 8, kind, compression, predictor }))
}

/// Decodes the first supported directory of `bytes`. Images over `max_pixels` are refused before
/// anything is decompressed.
pub fn decode_tiff_scalar(bytes: &[u8], max_pixels: u64) -> Result<TiffRaster, DecodeError> {
    let little = match bytes.get(..4) {
        Some([b'I', b'I', 42, 0]) => true,
        Some([b'M', b'M', 0, 42]) => false,
        Some([b'I', b'I', 43, 0] | [b'M', b'M', 0, 43]) => return Err(unsupported("BigTIFF")),
        Some(_) => return Err(invalid("not a TIFF file")),
        None => return Err(DecodeError::Truncated),
    };
    let reader = Reader { bytes, little };
    let directories = read_directories(reader, u64::from(reader.u32_at(4)?))?;
    for directory in &directories {
        let Some(layout) = supported_layout(reader, directory)? else { continue };
        let pixels = layout.width.checked_mul(layout.height).ok_or(invalid("image too large"))?;
        if layout.width == 0 || layout.height == 0 || pixels > max_pixels {
            return Err(DecodeError::ImageTooLarge { width: layout.width as u32, height: layout.height as u32 });
        }
        let data = decode_raster(reader, directory, &layout)?;
        return Ok(TiffRaster {
            width: layout.width as u32,
            height: layout.height as u32,
            data,
            gdal_nodata: directory.ascii(reader, TAG_GDAL_NODATA)?,
        });
    }
    Err(unsupported("no supported TIFF directories could be found"))
}

fn decode_raster(reader: Reader<'_>, directory: &Directory, layout: &Layout) -> Result<Vec<f32>, DecodeError> {
    let (segment_width, segment_height, offsets, counts) = if directory.find(TAG_TILE_OFFSETS).is_some() {
        let tile_width = directory.integer(reader, TAG_TILE_WIDTH)?.ok_or(invalid("missing TileWidth"))?;
        let tile_height = directory.integer(reader, TAG_TILE_LENGTH)?.ok_or(invalid("missing TileLength"))?;
        let offsets = directory.integers(reader, TAG_TILE_OFFSETS)?.unwrap_or_default();
        let counts = directory.integers(reader, TAG_TILE_BYTE_COUNTS)?.ok_or(invalid("missing TileByteCounts"))?;
        (tile_width, tile_height, offsets, counts)
    } else {
        let rows = directory.integer(reader, TAG_ROWS_PER_STRIP)?.unwrap_or(layout.height).min(layout.height);
        let offsets = directory.integers(reader, TAG_STRIP_OFFSETS)?.ok_or(invalid("missing StripOffsets"))?;
        let counts = directory.integers(reader, TAG_STRIP_BYTE_COUNTS)?.ok_or(invalid("missing StripByteCounts"))?;
        (layout.width, rows, offsets, counts)
    };
    if segment_width == 0 || segment_height == 0 || segment_width > 1 << 16 || segment_height > 1 << 16 {
        return Err(invalid("invalid tile or strip size"));
    }
    let across = layout.width.div_ceil(segment_width);
    let down = layout.height.div_ceil(segment_height);
    if offsets.len() as u64 != across * down || counts.len() != offsets.len() {
        return Err(invalid("segment offsets do not cover the image"));
    }
    let mut data = vec![0.0f32; (layout.width * layout.height) as usize];
    let row_bytes = segment_width * layout.bytes_per_sample;
    for (index, (&offset, &count)) in offsets.iter().zip(&counts).enumerate() {
        let index = index as u64;
        let (left, top) = ((index % across) * segment_width, (index / across) * segment_height);
        // Strips end at the image; tiles are always full size.
        let rows = if directory.find(TAG_TILE_OFFSETS).is_some() { segment_height } else { segment_height.min(layout.height - top) };
        let expected = (row_bytes * rows) as usize;
        let mut segment = decompress(reader.slice(offset, count)?, layout.compression, expected)?;
        if segment.len() < expected {
            return Err(DecodeError::Truncated);
        }
        segment.truncate(expected);
        for row in segment.chunks_exact_mut(row_bytes as usize) {
            undo_predictor(row, layout, reader.little)?;
        }
        for y in 0..rows {
            let image_y = top + y;
            if image_y >= layout.height {
                break;
            }
            for x in 0..segment_width {
                let image_x = left + x;
                if image_x >= layout.width {
                    break;
                }
                let at = ((y * segment_width + x) * layout.bytes_per_sample) as usize;
                let sample = &segment[at..at + layout.bytes_per_sample as usize];
                // Predictor 3 leaves samples big-endian whatever the file's byte order.
                let little = reader.little && layout.predictor != 3;
                data[(image_y * layout.width + image_x) as usize] = sample_value(sample, layout.kind, little);
            }
        }
    }
    Ok(data)
}

fn sample_value(bytes: &[u8], kind: SampleKind, little: bool) -> f32 {
    let mut buffer = [0u8; 8];
    let width = bytes.len();
    buffer[..width].copy_from_slice(bytes);
    if !little {
        buffer[..width].reverse();
    }
    let raw = u64::from_le_bytes(buffer);
    match (kind, width) {
        (SampleKind::Unsigned, 1) => f32::from(raw as u8),
        (SampleKind::Unsigned, 2) => f32::from(raw as u16),
        (SampleKind::Unsigned, _) => raw as u32 as f32,
        (SampleKind::Signed, 1) => f32::from(raw as u8 as i8),
        (SampleKind::Signed, 2) => f32::from(raw as u16 as i16),
        (SampleKind::Signed, _) => raw as u32 as i32 as f32,
        (SampleKind::Float, 4) => f32::from_bits(raw as u32),
        (SampleKind::Float, _) => f64::from_bits(raw) as f32,
    }
}

fn decompress(input: &[u8], compression: u64, expected: usize) -> Result<Vec<u8>, DecodeError> {
    match compression {
        1 => Ok(input.to_vec()),
        5 => lzw_decode(input, expected),
        8 | 32946 => zlib_decompress(input, expected),
        32773 => packbits_decode(input, expected),
        other => Err(unsupported(format!("compression {other}"))),
    }
}

fn undo_predictor(row: &mut [u8], layout: &Layout, little: bool) -> Result<(), DecodeError> {
    match layout.predictor {
        1 => Ok(()),
        2 if layout.kind != SampleKind::Float => {
            let size = layout.bytes_per_sample as usize;
            let read = |bytes: &[u8]| -> u32 {
                let mut buffer = [0u8; 4];
                buffer[..size].copy_from_slice(bytes);
                if !little {
                    buffer[..size].reverse();
                }
                u32::from_le_bytes(buffer)
            };
            let mut previous = 0u32;
            for sample in row.chunks_exact_mut(size) {
                let value = read(sample).wrapping_add(previous);
                let mut bytes = value.to_le_bytes();
                if !little {
                    bytes[..size].reverse();
                }
                sample.copy_from_slice(&bytes[..size]);
                previous = value;
            }
            Ok(())
        }
        3 if layout.kind == SampleKind::Float => {
            for index in 1..row.len() {
                row[index] = row[index].wrapping_add(row[index - 1]);
            }
            // Bytes are stored plane by plane, most significant plane first.
            let size = layout.bytes_per_sample as usize;
            let count = row.len() / size;
            let planar = row.to_vec();
            for sample in 0..count {
                for plane in 0..size {
                    row[sample * size + plane] = planar[plane * count + sample];
                }
            }
            Ok(())
        }
        other => Err(unsupported(format!("predictor {other} for {:?} samples", layout.kind))),
    }
}

/// TIFF LZW: MSB-first codes from 9 to 12 bits, clear code 256, end code 257, and the code width
/// growing one code early. Entries are slices of the output already produced.
fn lzw_decode(input: &[u8], limit: usize) -> Result<Vec<u8>, DecodeError> {
    const CLEAR: usize = 256;
    const END: usize = 257;
    if input.len() >= 2 && input[0] == 0 && input[1] & 1 != 0 {
        return Err(unsupported("pre-6.0 (LSB-first) LZW"));
    }
    let mut out: Vec<u8> = Vec::with_capacity(limit);
    let mut table = vec![(0usize, 0usize); 4096];
    let mut next = 258usize;
    let mut width = 9u32;
    let mut previous: Option<(usize, usize)> = None;
    let (mut accumulator, mut bits, mut position) = (0u32, 0u32, 0usize);
    loop {
        while bits < width {
            let Some(&byte) = input.get(position) else { return Ok(out) };
            accumulator = (accumulator << 8) | u32::from(byte);
            bits += 8;
            position += 1;
        }
        let code = ((accumulator >> (bits - width)) & ((1 << width) - 1)) as usize;
        bits -= width;
        if code == END {
            return Ok(out);
        }
        if code == CLEAR {
            next = 258;
            width = 9;
            previous = None;
            continue;
        }
        let start = out.len();
        match previous {
            None => {
                if code >= CLEAR {
                    return Err(invalid("LZW stream starts with a non-literal code"));
                }
                out.push(code as u8);
            }
            Some((previous_start, previous_length)) => {
                if code < CLEAR {
                    out.push(code as u8);
                } else if code < next {
                    let (entry_start, entry_length) = table[code];
                    out.extend_from_within(entry_start..entry_start + entry_length);
                } else if code == next {
                    out.extend_from_within(previous_start..previous_start + previous_length);
                    out.push(out[previous_start]);
                } else {
                    return Err(invalid("LZW code beyond the table"));
                }
                if next < 4096 {
                    table[next] = (previous_start, previous_length + 1);
                    next += 1;
                }
            }
        }
        if out.len() > limit {
            return Err(DecodeError::OutputLimitExceeded { limit });
        }
        previous = Some((start, out.len() - start));
        if next + 1 >= (1 << width) && width < 12 {
            width += 1;
        }
    }
}

fn packbits_decode(input: &[u8], limit: usize) -> Result<Vec<u8>, DecodeError> {
    let mut out = Vec::with_capacity(limit);
    let mut position = 0;
    while position < input.len() && out.len() < limit {
        let header = input[position] as i8;
        position += 1;
        match header {
            0..=127 => {
                let count = header as usize + 1;
                let literal = input.get(position..position + count).ok_or(DecodeError::Truncated)?;
                out.extend_from_slice(literal);
                position += count;
            }
            -127..=-1 => {
                let byte = *input.get(position).ok_or(DecodeError::Truncated)?;
                out.resize(out.len() + (1 - i32::from(header)) as usize, byte);
                position += 1;
            }
            -128 => {}
        }
    }
    if out.len() > limit {
        out.truncate(limit);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_tiff_and_bigtiff() {
        assert_eq!(decode_tiff_scalar(b"\x89PNG", 1 << 20), Err(DecodeError::InvalidTiff("not a TIFF file")));
        assert!(matches!(decode_tiff_scalar(b"II\x2b\x00\x08\x00\x00\x00", 1 << 20), Err(DecodeError::UnsupportedTiff(_))));
        assert_eq!(decode_tiff_scalar(b"II", 1 << 20), Err(DecodeError::Truncated));
    }

    #[test]
    fn packbits_matches_the_specification_example() {
        let encoded = [0xFE, 0xAA, 0x02, 0x80, 0x00, 0x2A, 0xFD, 0xAA, 0x03, 0x80, 0x00, 0x2A, 0x22, 0xF7, 0xAA];
        let decoded = packbits_decode(&encoded, 24).unwrap();
        let expected = [
            0xAA, 0xAA, 0xAA, 0x80, 0x00, 0x2A, 0xAA, 0xAA, 0xAA, 0xAA, 0x80, 0x00, 0x2A, 0x22, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA,
        ];
        assert_eq!(decoded, expected);
    }

    #[test]
    fn lzw_rejects_codes_beyond_the_table() {
        // Clear (256), literal 'A' (65), then 300 > next (258).
        let mut bits = Vec::new();
        for code in [256u32, 65, 300] {
            for bit in (0..9).rev() {
                bits.push(((code >> bit) & 1) as u8);
            }
        }
        let bytes: Vec<u8> = bits.chunks(8).map(|chunk| chunk.iter().enumerate().fold(0u8, |byte, (i, bit)| byte | (bit << (7 - i)))).collect();
        assert_eq!(lzw_decode(&bytes, 16), Err(DecodeError::InvalidTiff("LZW code beyond the table")));
    }
}
