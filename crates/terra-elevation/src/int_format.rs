//! `IntScalarParser` chains (`GlobalParseRegistries.SCALAR_PARSERS_INT`), reached from a heights
//! or tree-cover config through the `from_int` double parser.
//!
//! Decoders hand back the ARGB pixels Java's `BufferedImage.getRGB` returns: `parse_png` and
//! `parse_jpg` as `ImageIO.read` decodes PNG and JPEG (both autodetect the format), `parse_tiff`
//! as Commons Imaging's `TiffImageParser` does. The operators are bit-exact ports of the
//! `*ISP` classes; `Integer.MIN_VALUE` is the "no data" marker that `from_int` turns into `NaN`.

use crate::error::ElevationError;
use terra_codec::{DecodeError, JpegLimits, PngImage, PngLimits, TiffLimits, decode_jpeg, decode_png, decode_tiff_argb};

/// `Integer.MIN_VALUE`: the int parsers' missing-value marker.
pub const NO_DATA: i32 = i32::MIN;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntFormat {
    /// `parse_png` and `parse_jpg`: `ImageIO.read`, which sniffs PNG or JPEG whatever the name.
    Image,
    /// `parse_tiff`: Commons Imaging's TIFF reader, first directory, as ARGB.
    Tiff,
    /// `add`: wrapping addition, applied even to [`NO_DATA`].
    Add(Box<IntFormat>, i32),
    /// `and`: bitwise and, [`NO_DATA`] left alone.
    And(Box<IntFormat>, i32),
    /// `flip_x`.
    FlipX(Box<IntFormat>),
    /// `flip_z`.
    FlipZ(Box<IntFormat>),
    /// `swap_axes`.
    SwapAxes(Box<IntFormat>),
    /// `grayscale_extract`: the blue byte of opaque pixels, [`NO_DATA`] otherwise.
    GrayscaleExtract(Box<IntFormat>),
    /// `rgb_extract`: the RGB bits of opaque pixels, [`NO_DATA`] otherwise.
    RgbExtract(Box<IntFormat>),
    /// `require_opaque`: pixels that are not fully opaque become [`NO_DATA`].
    RequireOpaque(Box<IntFormat>),
}

impl IntFormat {
    /// `IntScalarParser.parse`: a square `resolution` grid, `z * resolution + x`.
    pub fn parse(&self, bytes: &[u8], resolution: u32) -> Result<Vec<i32>, ElevationError> {
        let size = resolution as usize;
        let map = |inner: &IntFormat, f: &dyn Fn(i32) -> i32| -> Result<Vec<i32>, ElevationError> { Ok(inner.parse(bytes, resolution)?.into_iter().map(f).collect()) };
        Ok(match self {
            Self::Image => square(decode_image(bytes, resolution)?, resolution)?,
            Self::Tiff => {
                let pixels = u64::from(resolution) * u64::from(resolution);
                square(decode_tiff_argb(bytes, &TiffLimits::for_pixels(pixels))?, resolution)?
            }
            Self::Add(inner, value) => map(inner, &|v| v.wrapping_add(*value))?,
            Self::And(inner, mask) => map(inner, &|v| if v != NO_DATA { v & mask } else { v })?,
            Self::GrayscaleExtract(inner) => map(inner, &|v| if (v as u32) >> 24 == 0xFF { v & 0xFF } else { NO_DATA })?,
            Self::RgbExtract(inner) => map(inner, &|v| if (v as u32) >> 24 == 0xFF { v & 0x00FF_FFFF } else { NO_DATA })?,
            Self::RequireOpaque(inner) => map(inner, &|v| if (v as u32) >> 24 == 0xFF { v } else { NO_DATA })?,
            Self::FlipX(inner) => flip_x(inner.parse(bytes, resolution)?, size),
            Self::FlipZ(inner) => flip_z(inner.parse(bytes, resolution)?, size),
            Self::SwapAxes(inner) => swap_axes(inner.parse(bytes, resolution)?, size),
        })
    }
}

/// `ImageIO.read`: the format comes from the bytes. Of the JDK's readers, PNG and JPEG are
/// ported; GIF, BMP, WBMP and TIFF are refused explicitly, anything else is unreadable (Java
/// fails with a `NullPointerException` there).
fn decode_image(bytes: &[u8], resolution: u32) -> Result<PngImage, DecodeError> {
    // Images larger than the tile are refused before decoding; Java would decode, then fail.
    let side = resolution.max(1);
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        decode_png(bytes, &PngLimits { max_width: side, max_height: side, ..PngLimits::default() })
    } else if bytes.starts_with(&[0xFF, 0xD8]) {
        decode_jpeg(bytes, &JpegLimits { max_width: side, max_height: side })
    } else if bytes.starts_with(b"GIF8") || bytes.starts_with(b"BM") || bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
        Err(DecodeError::UnsupportedPng("ImageIO would read this as GIF, BMP or TIFF; only PNG and JPEG are ported for parse_png/parse_jpg (use parse_tiff for TIFF)".into()))
    } else {
        Err(DecodeError::UnsupportedPng("no ImageIO reader recognises this image".into()))
    }
}

/// `checkArg(w == resolution && h == resolution, "invalid image resolution: ...")`, then
/// `getRGB` as signed ints.
fn square(image: PngImage, resolution: u32) -> Result<Vec<i32>, ElevationError> {
    if image.width != resolution || image.height != resolution {
        return Err(ElevationError::TileDecode(DecodeError::UnsupportedPng(format!(
            "invalid image resolution: {}x{} (expected: {resolution}x{resolution})",
            image.width, image.height
        ))));
    }
    Ok(image.argb.into_iter().map(|pixel| pixel as i32).collect())
}

/// `FlipX*SP`: mirrors every row.
pub(crate) fn flip_x<T>(mut values: Vec<T>, size: usize) -> Vec<T> {
    values.chunks_exact_mut(size.max(1)).for_each(<[T]>::reverse);
    values
}

/// `FlipZ*SP`: reverses the row order.
pub(crate) fn flip_z<T: Copy>(values: Vec<T>, size: usize) -> Vec<T> {
    values.chunks_exact(size.max(1)).rev().flatten().copied().collect()
}

/// `SwapAxes*SP`: transposes the square grid.
pub(crate) fn swap_axes<T>(mut values: Vec<T>, size: usize) -> Vec<T> {
    for i in 1..size {
        for j in 0..i {
            values.swap(i * size + j, j * size + i);
        }
    }
    values
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn operators_match_the_upstream_classes() {
        // A 2x2 "image" through a stand-in: flips and transposes on known data.
        assert_eq!(flip_x(vec![1, 2, 3, 4], 2), vec![2, 1, 4, 3]);
        assert_eq!(flip_z(vec![1, 2, 3, 4], 2), vec![3, 4, 1, 2]);
        assert_eq!(swap_axes(vec![1, 2, 3, 4], 2), vec![1, 3, 2, 4]);
        assert_eq!(NO_DATA.wrapping_add(-1), i32::MAX);
    }

    #[test]
    fn non_square_and_unknown_images_are_refused() {
        let error = IntFormat::Image.parse(b"GIF89a....", 16).unwrap_err();
        assert!(error.to_string().contains("GIF"), "{error}");
        assert!(IntFormat::Image.parse(b"nothing", 16).is_err());
        assert!(IntFormat::Tiff.parse(b"\x89PNG\r\n\x1a\n", 16).is_err());
    }
}
