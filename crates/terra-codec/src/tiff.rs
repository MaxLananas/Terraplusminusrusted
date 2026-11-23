//! Single-channel scalar TIFF decoding, as TerraMinusMinus' `ParseTiffDSP` does it through
//! Apache Commons Imaging (`TiffDirectory.getRasterData`): the first grayscale, one-sample
//! directory with an integer or IEEE floating-point sample format is read into `f32` values.
//!
//! Supported: classic TIFF and BigTIFF (as Commons Imaging reads both) in either byte order; strips or tiles; no compression,
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

/// Resource limits for one decode, checked with overflow-safe arithmetic before anything is
/// allocated or decompressed. The file is untrusted: its declared sizes never set an allocation
/// on their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TiffLimits {
    /// Image width times height.
    pub max_pixels: u64,
    /// Decoded bytes of one strip or tile (tiles may overhang the image edge).
    pub max_segment_bytes: u64,
    /// Decoded bytes of all strips and tiles together.
    pub max_decoded_bytes: u64,
    /// Strips or tiles in the image.
    pub max_segments: u64,
    /// Values in one array field (offsets, byte counts, bits per sample).
    pub max_field_values: u64,
}

impl TiffLimits {
    /// Limits for images of at most `max_pixels`: a segment may decode to at most four times the
    /// largest image at 8 bytes per sample (overhanging tiles), with a 1 MiB floor for tiny
    /// images, and all segments together to at most twice that.
    pub fn for_pixels(max_pixels: u64) -> Self {
        let segment = max_pixels.saturating_mul(32).max(1 << 20);
        Self {
            max_pixels,
            max_segment_bytes: segment,
            max_decoded_bytes: segment.saturating_mul(2),
            max_segments: max_pixels.clamp(1, 1 << 20),
            max_field_values: max_pixels.clamp(16, 1 << 20),
        }
    }
}

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
pub(crate) struct Reader<'a> {
    pub(crate) bytes: &'a [u8],
    pub(crate) little: bool,
    /// BigTIFF: 8-byte offsets and counts, 20-byte directory entries.
    pub(crate) big: bool,
}

impl<'a> Reader<'a> {
    /// Reads the header: byte order, version 42 (classic) or 43 (BigTIFF, with its offset size 8
    /// and reserved zero), and the first directory offset.
    pub(crate) fn open(bytes: &'a [u8]) -> Result<(Self, u64), DecodeError> {
        let (little, big) = match bytes.get(..4) {
            Some([b'I', b'I', 42, 0]) => (true, false),
            Some([b'M', b'M', 0, 42]) => (false, false),
            Some([b'I', b'I', 43, 0]) => (true, true),
            Some([b'M', b'M', 0, 43]) => (false, true),
            Some(_) => return Err(invalid("not a TIFF file")),
            None => return Err(DecodeError::Truncated),
        };
        let reader = Self { bytes, little, big };
        let first = if big {
            if reader.u16_at(4)? != 8 || reader.u16_at(6)? != 0 {
                return Err(invalid("malformed BigTIFF header"));
            }
            reader.u64_at(8)?
        } else {
            u64::from(reader.u32_at(4)?)
        };
        Ok((reader, first))
    }

    pub(crate) fn slice(&self, offset: u64, length: u64) -> Result<&[u8], DecodeError> {
        let start = usize::try_from(offset).map_err(|_| invalid("offset out of range"))?;
        let length = usize::try_from(length).map_err(|_| invalid("length out of range"))?;
        let end = start.checked_add(length).ok_or(invalid("offset out of range"))?;
        self.bytes.get(start..end).ok_or(DecodeError::Truncated)
    }

    pub(crate) fn u16_at(&self, offset: u64) -> Result<u16, DecodeError> {
        let bytes = self.slice(offset, 2)?;
        let pair = [bytes[0], bytes[1]];
        Ok(if self.little { u16::from_le_bytes(pair) } else { u16::from_be_bytes(pair) })
    }

    pub(crate) fn u32_at(&self, offset: u64) -> Result<u32, DecodeError> {
        let bytes = self.slice(offset, 4)?;
        let quad = [bytes[0], bytes[1], bytes[2], bytes[3]];
        Ok(if self.little { u32::from_le_bytes(quad) } else { u32::from_be_bytes(quad) })
    }

    pub(crate) fn u64_at(&self, offset: u64) -> Result<u64, DecodeError> {
        let bytes = self.slice(offset, 8)?;
        let mut octet = [0u8; 8];
        octet.copy_from_slice(bytes);
        Ok(if self.little { u64::from_le_bytes(octet) } else { u64::from_be_bytes(octet) })
    }
}

pub(crate) struct Entry {
    pub(crate) tag: u16,
    pub(crate) kind: u16,
    pub(crate) count: u64,
    /// Absolute offset of the value bytes (inline values point into the entry itself).
    pub(crate) value_offset: u64,
}

fn type_size(kind: u16) -> Option<u64> {
    match kind {
        1 | 2 | 6 | 7 => Some(1),
        3 | 8 => Some(2),
        4 | 9 | 11 | 13 => Some(4),
        5 | 10 | 12 | 16 | 17 | 18 => Some(8),
        _ => None,
    }
}

pub(crate) struct Directory {
    pub(crate) entries: Vec<Entry>,
}

impl Directory {
    pub(crate) fn find(&self, tag: u16) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.tag == tag)
    }

    /// An unsigned integer field (BYTE, SHORT, LONG, IFD, or BigTIFF's LONG8 and IFD8) as values.
    /// The values must lie inside the file and number at most `max_values` before anything is
    /// allocated.
    pub(crate) fn integers_limited(&self, reader: Reader<'_>, tag: u16, max_values: u64) -> Result<Option<Vec<u64>>, DecodeError> {
        let Some(entry) = self.find(tag) else { return Ok(None) };
        let size = match entry.kind {
            1 => 1,
            3 => 2,
            4 | 13 => 4,
            16 | 18 => 8,
            _ => return Err(invalid("integer field has a non-integer type")),
        };
        if entry.count > max_values {
            return Err(DecodeError::LimitExceeded(format!("TIFF field {tag} has {} values (limit {max_values})", entry.count)));
        }
        // Proves the whole array is inside the file, so the allocation below is bounded by it.
        reader.slice(entry.value_offset, entry.count.checked_mul(size).ok_or(invalid("field too large"))?)?;
        let mut values = Vec::new();
        values.try_reserve_exact(entry.count as usize).map_err(|_| DecodeError::LimitExceeded("TIFF field allocation failed".into()))?;
        for index in 0..entry.count {
            let offset = entry.value_offset + index * size;
            values.push(match size {
                1 => u64::from(reader.slice(offset, 1)?[0]),
                2 => u64::from(reader.u16_at(offset)?),
                4 => u64::from(reader.u32_at(offset)?),
                _ => reader.u64_at(offset)?,
            });
        }
        Ok(Some(values))
    }

    /// A single-valued integer field: its first value, from a field of at most 16 values.
    pub(crate) fn integer(&self, reader: Reader<'_>, tag: u16) -> Result<Option<u64>, DecodeError> {
        Ok(self.integers_limited(reader, tag, 16)?.and_then(|values| values.first().copied()))
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

/// Reads the chain of image directories from `first`, at most `max` of them.
pub(crate) fn read_directories_limited(reader: Reader<'_>, first: u64, max: usize) -> Result<Vec<Directory>, DecodeError> {
    // Classic: 2-byte entry count, 12-byte entries with a 4-byte count and value, 4-byte link.
    // BigTIFF: 8-byte entry count, 20-byte entries with an 8-byte count and value, 8-byte link.
    let (count_size, entry_size, field_size) = if reader.big { (8, 20, 8) } else { (2, 12, 4) };
    let read_field = |at: u64| -> Result<u64, DecodeError> { if reader.big { reader.u64_at(at) } else { reader.u32_at(at).map(u64::from) } };
    let mut directories = Vec::new();
    let mut seen = Vec::new();
    let mut offset = first;
    while offset != 0 && directories.len() < max {
        if directories.len() == MAX_DIRECTORIES || seen.contains(&offset) {
            return Err(invalid("too many or cyclic image directories"));
        }
        seen.push(offset);
        let count = if reader.big { reader.u64_at(offset)? } else { u64::from(reader.u16_at(offset)?) };
        if count > MAX_ENTRIES as u64 {
            return Err(invalid("too many directory entries"));
        }
        let mut entries = Vec::with_capacity(count as usize);
        for index in 0..count {
            let at = offset + count_size + index * entry_size;
            let tag = reader.u16_at(at)?;
            let kind = reader.u16_at(at + 2)?;
            let count = read_field(at + 4)?;
            let Some(size) = type_size(kind) else { continue };
            let total = size.checked_mul(count).ok_or(invalid("field too large"))?;
            let value_at = at + 4 + field_size;
            let value_offset = if total <= field_size { value_at } else { read_field(value_at)? };
            entries.push(Entry { tag, kind, count, value_offset });
        }
        directories.push(Directory { entries });
        offset = read_field(offset + count_size + count * entry_size)?;
    }
    Ok(directories)
}

fn read_directories(reader: Reader<'_>, first: u64) -> Result<Vec<Directory>, DecodeError> {
    read_directories_limited(reader, first, MAX_DIRECTORIES)
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
    let bits = directory.integers_limited(reader, TAG_BITS_PER_SAMPLE, 16)?.unwrap_or_else(|| vec![1]);
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

/// Decodes the first supported directory of `bytes` with [`TiffLimits::for_pixels`]. Images over
/// `max_pixels` are refused before anything is decompressed.
pub fn decode_tiff_scalar(bytes: &[u8], max_pixels: u64) -> Result<TiffRaster, DecodeError> {
    decode_tiff_scalar_limited(bytes, &TiffLimits::for_pixels(max_pixels))
}

/// Decodes the first supported directory of `bytes`, refusing anything over `limits` with
/// [`DecodeError::ImageTooLarge`] or [`DecodeError::LimitExceeded`] before allocating for it.
pub fn decode_tiff_scalar_limited(bytes: &[u8], limits: &TiffLimits) -> Result<TiffRaster, DecodeError> {
    let max_pixels = limits.max_pixels;
    let (reader, first) = Reader::open(bytes)?;
    let directories = read_directories(reader, first)?;
    for directory in &directories {
        let Some(layout) = supported_layout(reader, directory)? else { continue };
        let pixels = layout.width.checked_mul(layout.height).ok_or(invalid("image too large"))?;
        if layout.width == 0 || layout.height == 0 || pixels > max_pixels {
            return Err(DecodeError::ImageTooLarge { width: layout.width as u32, height: layout.height as u32 });
        }
        let data = decode_raster(reader, directory, &layout, limits)?;
        return Ok(TiffRaster {
            width: layout.width as u32,
            height: layout.height as u32,
            data,
            gdal_nodata: directory.ascii(reader, TAG_GDAL_NODATA)?,
        });
    }
    Err(unsupported("no supported TIFF directories could be found"))
}

pub(crate) fn limit_exceeded(what: &str, value: u64, limit: u64) -> DecodeError {
    DecodeError::LimitExceeded(format!("TIFF {what} of {value} exceeds the limit of {limit}"))
}

fn decode_raster(reader: Reader<'_>, directory: &Directory, layout: &Layout, limits: &TiffLimits) -> Result<Vec<f32>, DecodeError> {
    let tiled = directory.find(TAG_TILE_OFFSETS).is_some();
    let (segment_width, segment_height) = if tiled {
        let tile_width = directory.integer(reader, TAG_TILE_WIDTH)?.ok_or(invalid("missing TileWidth"))?;
        let tile_height = directory.integer(reader, TAG_TILE_LENGTH)?.ok_or(invalid("missing TileLength"))?;
        (tile_width, tile_height)
    } else {
        (layout.width, directory.integer(reader, TAG_ROWS_PER_STRIP)?.unwrap_or(layout.height).min(layout.height))
    };
    if segment_width == 0 || segment_height == 0 || segment_width > 1 << 16 || segment_height > 1 << 16 {
        return Err(invalid("invalid tile or strip size"));
    }
    // Every size below is checked before the segment arrays are read or anything is decoded.
    let row_bytes = segment_width.checked_mul(layout.bytes_per_sample).ok_or(invalid("segment too large"))?;
    let full_segment_bytes = row_bytes.checked_mul(segment_height).ok_or(invalid("segment too large"))?;
    if full_segment_bytes > limits.max_segment_bytes {
        return Err(limit_exceeded("decoded segment size", full_segment_bytes, limits.max_segment_bytes));
    }
    let across = layout.width.div_ceil(segment_width);
    let down = layout.height.div_ceil(segment_height);
    let segments = across.checked_mul(down).ok_or(invalid("too many segments"))?;
    if segments > limits.max_segments {
        return Err(limit_exceeded("segment count", segments, limits.max_segments));
    }
    // Tiles are always full size; strips end at the image.
    let decoded_total = if tiled { segments.checked_mul(full_segment_bytes) } else { layout.height.checked_mul(row_bytes) }.ok_or(invalid("image too large"))?;
    if decoded_total > limits.max_decoded_bytes {
        return Err(limit_exceeded("total decoded size", decoded_total, limits.max_decoded_bytes));
    }
    let field_limit = segments.min(limits.max_field_values);
    let (offset_tag, count_tag) = if tiled { (TAG_TILE_OFFSETS, TAG_TILE_BYTE_COUNTS) } else { (TAG_STRIP_OFFSETS, TAG_STRIP_BYTE_COUNTS) };
    let offsets = directory.integers_limited(reader, offset_tag, field_limit)?.ok_or(invalid("missing segment offsets"))?;
    let counts = directory.integers_limited(reader, count_tag, field_limit)?.ok_or(invalid("missing segment byte counts"))?;
    if offsets.len() as u64 != segments || counts.len() != offsets.len() {
        return Err(invalid("segment offsets do not cover the image"));
    }
    let pixels = layout.width.checked_mul(layout.height).ok_or(invalid("image too large"))?;
    let mut data = Vec::new();
    data.try_reserve_exact(pixels as usize).map_err(|_| DecodeError::LimitExceeded("TIFF raster allocation failed".into()))?;
    data.resize(pixels as usize, 0.0f32);
    for (index, (&offset, &count)) in offsets.iter().zip(&counts).enumerate() {
        let index = index as u64;
        let (left, top) = ((index % across) * segment_width, (index / across) * segment_height);
        let rows = if tiled { segment_height } else { segment_height.min(layout.height - top) };
        let expected = (row_bytes * rows) as usize;
        let mut segment = decompress(reader.slice(offset, count)?, layout.compression, expected)?;
        if segment.len() < expected {
            return Err(DecodeError::Truncated);
        }
        segment.truncate(expected);
        // Commons Imaging (`unpackIntSamples`) undoes differencing on signed 16-bit samples by
        // summing them sign-extended in Java `int`s, so a row's running sum can leave the 16-bit
        // range where the TIFF specification would wrap. That is what upstream's `parse_tiff`
        // produces, so it is reproduced here; other layouts follow the specification.
        let java_int_differencing = layout.predictor == 2 && layout.kind == SampleKind::Signed && layout.bytes_per_sample == 2;
        if !java_int_differencing {
            for row in segment.chunks_exact_mut(row_bytes as usize) {
                undo_predictor(row, layout, reader.little)?;
            }
        }
        for y in 0..rows {
            let image_y = top + y;
            if image_y >= layout.height {
                break;
            }
            let mut running = 0i32;
            for x in 0..segment_width {
                let image_x = left + x;
                if image_x >= layout.width {
                    break;
                }
                let at = ((y * segment_width + x) * layout.bytes_per_sample) as usize;
                let sample = &segment[at..at + layout.bytes_per_sample as usize];
                // Predictor 3 leaves samples big-endian whatever the file's byte order.
                let little = reader.little && layout.predictor != 3;
                data[(image_y * layout.width + image_x) as usize] = if java_int_differencing {
                    let raw = if little { i16::from_le_bytes([sample[0], sample[1]]) } else { i16::from_be_bytes([sample[0], sample[1]]) };
                    running = running.wrapping_add(i32::from(raw));
                    running as f32
                } else {
                    sample_value(sample, layout.kind, little)
                };
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

pub(crate) fn decompress(input: &[u8], compression: u64, expected: usize) -> Result<Vec<u8>, DecodeError> {
    match compression {
        // Never copies more than the segment needs, whatever the declared byte count.
        1 => Ok(input[..input.len().min(expected)].to_vec()),
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
    let mut out: Vec<u8> = Vec::new();
    out.try_reserve(limit.min(input.len().saturating_mul(16))).map_err(|_| DecodeError::LimitExceeded("LZW output allocation failed".into()))?;
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
        // Commons Imaging's decompressor stops once the segment is complete, ignoring the rest.
        if out.len() >= limit {
            out.truncate(limit);
            return Ok(out);
        }
        previous = Some((start, out.len() - start));
        if next + 1 >= (1 << width) && width < 12 {
            width += 1;
        }
    }
}

fn packbits_decode(input: &[u8], limit: usize) -> Result<Vec<u8>, DecodeError> {
    // PackBits expands at most 128 times (a two-byte run of 128).
    let mut out = Vec::new();
    out.try_reserve(limit.min(input.len().saturating_mul(64))).map_err(|_| DecodeError::LimitExceeded("PackBits output allocation failed".into()))?;
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
    fn rejects_non_tiff_and_malformed_bigtiff() {
        assert_eq!(decode_tiff_scalar(b"\x89PNG", 1 << 20), Err(DecodeError::InvalidTiff("not a TIFF file")));
        assert_eq!(decode_tiff_scalar(b"II\x2b\x00\x04\x00\x00\x00\x10\0\0\0\0\0\0\0", 1 << 20), Err(DecodeError::InvalidTiff("malformed BigTIFF header")));
        assert_eq!(decode_tiff_scalar(b"II\x2b\x00\x08\x00\x00\x00", 1 << 20), Err(DecodeError::Truncated));
        assert_eq!(decode_tiff_scalar(b"II", 1 << 20), Err(DecodeError::Truncated));
    }

    /// A BigTIFF and the same raster as classic TIFF decode identically.
    #[test]
    fn bigtiff_reads_like_classic_tiff() {
        fn build(big: bool) -> Vec<u8> {
            // 3x2 little-endian 16-bit unsigned grayscale, one uncompressed strip.
            let pixels: [u16; 6] = [1, 2, 3, 400, 500, 65535];
            let data: Vec<u8> = pixels.iter().flat_map(|value| value.to_le_bytes()).collect();
            let entries: [(u16, u16, u64); 8] = [(256, 3, 3), (257, 3, 2), (258, 3, 16), (259, 3, 1), (262, 3, 1), (273, if big { 16 } else { 4 }, 0), (277, 3, 1), (279, 4, 12)];
            let (header, count_size, entry_size, field_size) = if big { (16u64, 8u64, 20u64, 8usize) } else { (8, 2, 12, 4) };
            let directory_size = count_size + entries.len() as u64 * entry_size + field_size as u64;
            let data_offset = header + directory_size;
            let mut out = Vec::new();
            if big {
                out.extend_from_slice(b"II\x2b\x00\x08\x00\x00\x00");
                out.extend_from_slice(&header.to_le_bytes());
                out.extend_from_slice(&(entries.len() as u64).to_le_bytes());
            } else {
                out.extend_from_slice(b"II\x2a\x00");
                out.extend_from_slice(&(header as u32).to_le_bytes());
                out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
            }
            for (tag, kind, value) in entries {
                let value = if tag == 273 { data_offset } else { value };
                out.extend_from_slice(&tag.to_le_bytes());
                out.extend_from_slice(&kind.to_le_bytes());
                let count = 1u64;
                out.extend_from_slice(&count.to_le_bytes()[..field_size]);
                out.extend_from_slice(&value.to_le_bytes()[..field_size]);
            }
            out.extend_from_slice(&[0u8; 8][..field_size]);
            out.extend_from_slice(&data);
            out
        }
        let classic = decode_tiff_scalar(&build(false), 1 << 20).unwrap();
        let big = decode_tiff_scalar(&build(true), 1 << 20).unwrap();
        assert_eq!(classic.data, vec![1.0, 2.0, 3.0, 400.0, 500.0, 65535.0]);
        assert_eq!(big, classic);
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
