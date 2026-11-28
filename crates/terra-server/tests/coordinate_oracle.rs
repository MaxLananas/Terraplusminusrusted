//! Differential test of the `/tpll` coordinate parser against TerraMinusMinus'
//! `CoordinateParseUtils` outputs recorded by `tools/oracle/build-oracle.sh`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use terra_server::{UpstreamParseOutcome, parse_coordinates_upstream};

fn fixture(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/coordinates").join(relative)
}

#[test]
fn matches_upstream_on_every_recorded_input() {
    let inputs = std::fs::read_to_string(fixture("parse_inputs.txt")).unwrap();
    let inputs: Vec<&str> = inputs.lines().collect();
    let compressed = std::fs::read(fixture("golden/coordinate_parse.tsv.gz")).unwrap();
    let golden = String::from_utf8(terra_codec::gunzip(&compressed, 1 << 20).unwrap()).unwrap();
    let mut mismatches = Vec::new();
    let mut rows = 0;
    for line in golden.lines().skip(1) {
        let fields: Vec<&str> = line.split('\t').collect();
        let index: usize = fields[0].parse().unwrap();
        let input = inputs[index];
        let expected = match fields[1] {
            "ok" => {
                let bits = |text: &str| f64::from_bits(u64::from_str_radix(text, 16).unwrap());
                UpstreamParseOutcome::Parsed(terra_server::LatLng { latitude: bits(fields[2]), longitude: bits(fields[3]) })
            }
            "null" => UpstreamParseOutcome::Unparseable,
            "throws:NullPointerException" => UpstreamParseOutcome::NullGroup,
            other => panic!("unexpected oracle outcome {other}"),
        };
        let actual = parse_coordinates_upstream(input);
        let same = match (actual, expected) {
            (UpstreamParseOutcome::Parsed(a), UpstreamParseOutcome::Parsed(b)) => {
                a.latitude.to_bits() == b.latitude.to_bits() && a.longitude.to_bits() == b.longitude.to_bits()
            }
            (a, b) => a == b,
        };
        if !same {
            mismatches.push(format!("#{index} {input:?}: expected {expected:?}, got {actual:?}"));
        }
        rows += 1;
    }
    assert_eq!(rows, inputs.len());
    assert!(mismatches.is_empty(), "{} of {rows} inputs differ:\n{}", mismatches.len(), mismatches.join("\n"));
}
