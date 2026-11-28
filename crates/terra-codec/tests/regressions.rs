//! Every input that once crashed a decoder (`tests/regressions/`), fed to every decoder: each
//! call must return, a value or an error, without panicking.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use terra_codec::{JpegLimits, PngLimits, TiffLimits, decode_jpeg, decode_png, decode_tiff_argb, decode_tiff_scalar_limited, gunzip, inflate, parse_relaxed_json, zlib_decompress};

#[test]
fn past_crashes_stay_fixed() {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/regressions");
    let mut checked = 0;
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|extension| extension == "md") {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        let tiff = TiffLimits::for_pixels(1 << 20);
        let _ = decode_tiff_argb(&bytes, &tiff);
        let _ = decode_tiff_scalar_limited(&bytes, &tiff);
        let _ = decode_png(&bytes, &PngLimits::default());
        let _ = decode_jpeg(&bytes, &JpegLimits::default());
        let _ = gunzip(&bytes, 1 << 24);
        let _ = inflate(&bytes, 1 << 24);
        let _ = zlib_decompress(&bytes, 1 << 24);
        let _ = parse_relaxed_json(&String::from_utf8_lossy(&bytes));
        checked += 1;
    }
    assert!(checked >= 1);
}
