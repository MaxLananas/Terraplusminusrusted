//! TerraMinusMinus' scalar parsers on images, against upstream's own classes run in the oracle
//! (`tools/oracle`, `ImageGoldenDump`):
//!
//! * `parsers.tsv`: every integer parser (`parse_png`, `parse_jpg`, `parse_tiff`, `add`, `and`,
//!   `flip_x`, `flip_z`, `swap_axes`, `grayscale_extract`, `rgb_extract`, `require_opaque`) and
//!   `from_int` on top, loaded from the same JSON a heights config would hold;
//! * `raster.tsv`: `parse_tiff` for doubles (`ParseTiffDSP`), GDAL no-data included.
//!
//! Values must match bit for bit (any `NaN` matching any `NaN`), failures must coincide, except
//! that the port's `parse_tiff` deliberately reads a superset of what Commons Imaging's raster
//! reader accepts (unsigned and 8-bit integers, a missing `SampleFormat`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use terra_elevation::{IntFormat, TileFormat, UnsupportedLayerPolicy, load_heights_config};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/images")
}

fn golden(name: &str) -> String {
    let compressed = std::fs::read(root().join("golden").join(name)).unwrap();
    String::from_utf8(terra_codec::gunzip(&compressed, 64 << 20).unwrap()).unwrap()
}

/// The `parse` member of a one-layer heights config, as the config loader builds it.
fn load_format(parse: &str) -> TileFormat {
    let config = format!(
        r#"[{{"dataset": {{"urls": ["https://example.invalid/${{x}}/${{z}}.png"], "projection": {{"equirectangular": {{}}}},
        "resolution": 256, "blend": "NEAR", "parse": {parse}}}, "bounds": [{{"minX": -1, "maxX": 1, "minZ": -1, "maxZ": 1}}], "zooms": {{"min": 0, "max": 3}}}}]"#
    );
    let loaded = load_heights_config(&config, UnsupportedLayerPolicy::Reject).unwrap_or_else(|error| panic!("{parse}: {error}"));
    loaded.dataset.layers()[0].dataset.format.clone()
}

fn same_double(actual: f64, expected_bits: &str) -> bool {
    let expected = f64::from_bits(u64::from_str_radix(expected_bits, 16).unwrap());
    actual.to_bits() == expected.to_bits() || (actual.is_nan() && expected.is_nan())
}

#[test]
fn integer_parser_chains_match_upstream() {
    let text = golden("parsers.tsv.gz");
    let (mut values_checked, mut errors_checked, mut failures) = (0, 0, Vec::new());
    for line in text.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        let (file, spec, resolution, kind) = (fields[0], fields[1], fields[2].parse::<u32>().unwrap(), fields[3]);
        // A known upstream bug (see terra-codec's image_oracle): upstream decodes garbage there.
        if file.starts_with("tif_k_") {
            continue;
        }
        let bytes = std::fs::read(root().join("input").join(file)).unwrap();
        let is_double = spec.starts_with("{\"from_int\"");
        let format = load_format(&if is_double { spec.to_owned() } else { format!("{{\"from_int\": {{\"delegate\": {spec}}}}}") });
        let label = format!("{file} {spec}");
        if is_double {
            match (format.parse(&bytes, resolution), kind) {
                (Ok(values), "DOUBLE") => {
                    let expected = &fields[4..];
                    if values.len() != expected.len() {
                        failures.push(format!("{label}: {} values instead of {}", values.len(), expected.len()));
                    } else if let Some(index) = (0..values.len()).find(|&i| !same_double(values[i], expected[i])) {
                        failures.push(format!("{label}: value {index} is {} instead of bits {}", values[index], expected[index]));
                    }
                    values_checked += 1;
                }
                (Err(_), "ERROR") => errors_checked += 1,
                (Ok(_), _) => failures.push(format!("{label}: upstream fails ({}), the port does not", fields[4])),
                (Err(error), _) => failures.push(format!("{label}: {error}")),
            }
        } else {
            let TileFormat::FromInt(int_format) = format else { panic!("{label}: not an integer chain") };
            let int_format: IntFormat = *int_format;
            match (int_format.parse(&bytes, resolution), kind) {
                (Ok(values), "INT") => {
                    let expected: Vec<i32> = fields[4..].iter().map(|hex| u32::from_str_radix(hex, 16).unwrap() as i32).collect();
                    if values != expected {
                        let index = (0..values.len().min(expected.len())).find(|&i| values[i] != expected[i]).unwrap_or(0);
                        failures.push(format!("{label}: value {index} is {:08x} instead of {:08x}", values.get(index).copied().unwrap_or(0), expected.get(index).copied().unwrap_or(0)));
                    }
                    values_checked += 1;
                }
                (Err(_), "ERROR") => errors_checked += 1,
                (Ok(_), _) => failures.push(format!("{label}: upstream fails ({}), the port does not", fields[4])),
                (Err(error), _) => failures.push(format!("{label}: {error}")),
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
    assert!(values_checked >= 300, "only {values_checked} parser outputs compared");
    assert!(errors_checked >= 20, "only {errors_checked} upstream failures compared");
}

#[test]
fn double_parse_tiff_matches_upstream_where_upstream_reads_the_file() {
    let text = golden("raster.tsv.gz");
    let format = load_format(r#"{"parse_tiff": {}}"#);
    let (mut checked, mut superset, mut failures) = (0, 0, Vec::new());
    for line in text.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        let (file, resolution, kind) = (fields[0], fields[2].parse::<u32>().unwrap(), fields[3]);
        if file.starts_with("tif_k_") {
            continue;
        }
        let bytes = std::fs::read(root().join("input").join(file)).unwrap();
        match (format.parse(&bytes, resolution), kind) {
            (Ok(values), "DOUBLE") => {
                let expected = &fields[4..];
                if let Some(index) = (0..expected.len()).find(|&i| values.get(i).is_none_or(|&value| !same_double(value, expected[i]))) {
                    failures.push(format!("{file}: value {index} is {:?} instead of bits {}", values.get(index), expected[index]));
                }
                checked += 1;
            }
            (Err(error), "DOUBLE") => failures.push(format!("{file}: {error}")),
            // Upstream refuses: the port may refuse too, or read it as a documented superset.
            (Ok(_), _) => superset += 1,
            (Err(_), _) => {}
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
    assert!(checked >= 8, "only {checked} rasters compared");
    assert!(superset >= 1, "the superset cases are no longer exercised");
}
