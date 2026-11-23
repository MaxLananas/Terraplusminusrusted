//! The TIFF decoder against rasters decoded by tifffile + imagecodecs, an independent
//! implementation (`fixtures/tiff/make-fixtures.py`): every sample must match bit for bit.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use terra_codec::{DecodeError, decode_tiff_scalar, gunzip};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/tiff")
}

#[test]
fn decodes_every_case_like_the_reference() {
    let cases = std::fs::read_to_string(fixtures().join("cases.tsv")).unwrap();
    let mut checked = 0;
    for line in cases.lines().skip(1) {
        let fields: Vec<&str> = line.split('\t').collect();
        let name = fields[0];
        let bytes = std::fs::read(fixtures().join("cases").join(format!("{name}.tif"))).unwrap();
        let raster = decode_tiff_scalar(&bytes, 1 << 20).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!((raster.width.to_string(), raster.height.to_string()), (fields[1].to_owned(), fields[2].to_owned()), "{name}");
        let expected = gunzip(&std::fs::read(fixtures().join("cases").join(format!("{name}.f32.gz"))).unwrap(), 1 << 24).unwrap();
        let expected: Vec<u32> = expected.chunks_exact(4).map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])).collect();
        // Signed 16-bit differencing is summed in Java `int`s by upstream (Commons Imaging), the
        // reference wraps as the TIFF specification says: compare those modulo 2^16 here, the
        // exact upstream values being checked by terra-elevation's `image_parsers_oracle`.
        let wraps_like_the_specification = name.starts_with("i16_") && name.contains("pred2");
        let actual: Vec<u32> = raster
            .data
            .iter()
            .map(|&value| if wraps_like_the_specification { f32::from(value as i32 as i16) } else { value })
            .map(f32::to_bits)
            .collect();
        assert_eq!(actual.len(), expected.len(), "{name}");
        if let Some(index) = actual.iter().zip(&expected).position(|(a, e)| a != e) {
            panic!("{name}: sample {index} is {} but the reference says {}", f32::from_bits(actual[index]), f32::from_bits(expected[index]));
        }
        assert_eq!(raster.gdal_nodata.as_deref().unwrap_or("-"), fields[3], "{name}");
        checked += 1;
    }
    assert_eq!(checked, 16);
}

#[test]
fn refuses_images_over_the_pixel_limit_and_truncated_data() {
    let bytes = std::fs::read(fixtures().join("cases/u8_lzw_large.tif")).unwrap();
    assert!(matches!(decode_tiff_scalar(&bytes, 255 * 256), Err(DecodeError::ImageTooLarge { width: 256, height: 256 })));
    let bytes = std::fs::read(fixtures().join("cases/f32_none_be.tif")).unwrap();
    // Cut inside the pixel data: the strips no longer fit.
    assert!(decode_tiff_scalar(&bytes[..bytes.len() / 2], 1 << 20).is_err());
}

#[test]
fn every_truncation_fails_cleanly() {
    let bytes = std::fs::read(fixtures().join("cases/u16_lzw_pred2.tif")).unwrap();
    for length in 0..bytes.len() {
        let _ = decode_tiff_scalar(&bytes[..length], 1 << 20);
    }
}
