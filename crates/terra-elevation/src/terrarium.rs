//! `ParseTerrariumPngDSP`: Terrarium RGB-encoded elevation tiles.

use terra_codec::{DecodeError, PngLimits, decode_png};

/// Decodes a square Terrarium PNG into row-major metres (`z * resolution + x`).
/// Pixels whose alpha is not fully opaque are no-data (`NaN`).
pub fn decode_terrarium_png(bytes: &[u8], resolution: u32) -> Result<Vec<f64>, DecodeError> {
    let limits = PngLimits { max_width: resolution, max_height: resolution, ..PngLimits::default() };
    let image = decode_png(bytes, &limits)?;
    if image.width != resolution || image.height != resolution {
        return Err(DecodeError::UnsupportedPng(format!(
            "invalid image resolution: {}x{} (expected: {resolution}x{resolution})",
            image.width, image.height
        )));
    }
    Ok(image.argb.iter().map(|&argb| terrarium_value(argb)).collect())
}

pub fn terrarium_value(argb: u32) -> f64 {
    if argb >> 24 != 0xFF {
        return f64::NAN;
    }
    let rgb = (argb & 0x00FF_FFFF) as i32;
    f64::from(rgb - 0x0080_0000) * (1.0 / 256.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_sea_level_and_extremes() {
        assert_eq!(terrarium_value(0xFF80_0000), 0.0);
        assert_eq!(terrarium_value(0xFF80_0180), 1.5);
        assert_eq!(terrarium_value(0xFF00_0000), -32768.0);
        assert!(terrarium_value(0x0080_0000).is_nan());
        assert!(terrarium_value(0xFE80_0000).is_nan());
    }
}
