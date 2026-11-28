//! Image decoding against what upstream's runtime produces, pixel for pixel (`tools/oracle`,
//! `ImageGoldenDump`): PNG and JPEG through `ImageIO.read`, TIFF through Commons Imaging's
//! `TiffImageParser` (`parse_tiff` for ints), then `BufferedImage.getRGB` in the oracle JDK.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use terra_codec::{DecodeError, JpegLimits, PngImage, PngLimits, TiffLimits, decode_jpeg, decode_png, decode_tiff_argb};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/images")
}

fn decode(name: &str) -> Result<PngImage, DecodeError> {
    let bytes = std::fs::read(root().join("input").join(name)).unwrap();
    if name.ends_with(".tif") {
        decode_tiff_argb(&bytes, &TiffLimits::for_pixels(1 << 22))
    } else if bytes.starts_with(&[0xFF, 0xD8]) {
        decode_jpeg(&bytes, &JpegLimits::default())
    } else {
        decode_png(&bytes, &PngLimits::default())
    }
}

#[test]
fn every_fixture_image_decodes_exactly_as_imageio() {
    let compressed = std::fs::read(root().join("golden/argb.tsv.gz")).unwrap();
    let text = String::from_utf8(terra_codec::gunzip(&compressed, 64 << 20).unwrap()).unwrap();
    let mut failures = Vec::new();
    let mut checked = 0;
    for line in text.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        let name = fields[0];
        // Known upstream bugs and formats refused on purpose, checked separately below.
        if name.starts_with("tif_k_") || name == "tif_jpeg.tif" {
            continue;
        }
        let decoded = decode(name);
        if fields[1] == "UNREADABLE" {
            if decoded.is_ok() {
                failures.push(format!("{name}: ImageIO cannot read it, the port can"));
            }
            continue;
        }
        let image = match decoded {
            Ok(image) => image,
            Err(error) => {
                failures.push(format!("{name}: {error}"));
                continue;
            }
        };
        let (width, height): (u32, u32) = (fields[1].parse().unwrap(), fields[2].parse().unwrap());
        let expected: Vec<u32> = fields[3..].iter().map(|hex| u32::from_str_radix(hex, 16).unwrap()).collect();
        if (image.width, image.height) != (width, height) {
            failures.push(format!("{name}: {}x{} instead of {width}x{height}", image.width, image.height));
            continue;
        }
        let differing: Vec<usize> = (0..expected.len()).filter(|&i| image.argb[i] != expected[i]).collect();
        if let Some(&first) = differing.first() {
            let worst = differing
                .iter()
                .map(|&i| (0..4).map(|byte| ((image.argb[i] >> (8 * byte)) & 0xFF).abs_diff((expected[i] >> (8 * byte)) & 0xFF)).max().unwrap_or(0))
                .max()
                .unwrap_or(0);
            failures.push(format!(
                "{name}: {} of {} pixels differ (largest channel difference {worst}); first at ({}, {}): {:08x} instead of {:08x}",
                differing.len(),
                expected.len(),
                first % width as usize,
                first / width as usize,
                image.argb[first],
                expected[first]
            ));
        }
        checked += 1;
    }
    assert!(checked >= 90, "only {checked} images compared");
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Commons Imaging reads a single-valued LONG8 field through `toInt`, i.e. its first four bytes:
/// in a big-endian BigTIFF with one strip the strip offset becomes 0 and upstream decodes the
/// header as pixels. The port reads the offset correctly, giving the pixels of the same raster
/// stored in several strips (where upstream is right).
#[test]
fn big_endian_bigtiff_with_one_strip_reads_its_real_offset() {
    let one_strip = decode("tif_k_i16_be_bigtiff_one_strip.tif").unwrap();
    let several = decode("tif_r_i16_be_bigtiff.tif").unwrap();
    assert_eq!(one_strip, several);
}

/// Formats Commons Imaging reads but this port refuses are refused explicitly, never decoded
/// approximately.
#[test]
fn jpeg_compressed_tiff_is_refused_explicitly() {
    match decode("tif_jpeg.tif") {
        Err(DecodeError::UnsupportedTiff(reason)) => assert!(reason.contains("JPEG"), "{reason}"),
        other => panic!("{other:?}"),
    }
}
