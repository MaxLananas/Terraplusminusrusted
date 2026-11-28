//! Chat-visible number formatting against `Double.toString` and `String.format` from the JDK.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use terra_server::java_text::{java_double_to_string, java_format_fixed};

#[test]
fn matches_java_double_text() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/commands/golden/double_text.tsv.gz");
    let text = String::from_utf8(terra_codec::gunzip(&std::fs::read(path).unwrap(), 16 << 20).unwrap()).unwrap();
    let mut mismatches = Vec::new();
    let mut compared = 0;
    for line in text.lines().skip(1) {
        let fields: Vec<&str> = line.split('\t').collect();
        let value = f64::from_bits(u64::from_str_radix(fields[0], 16).unwrap());
        let actual = [java_double_to_string(value), java_format_fixed(value, 6), java_format_fixed(value, 4)];
        for (actual, expected) in actual.iter().zip(&fields[1..4]) {
            if actual != expected {
                mismatches.push(format!("{value:e}: {actual} != {expected}"));
            }
        }
        compared += 1;
    }
    assert_eq!(compared, 735);
    assert!(mismatches.is_empty(), "{} mismatches:\n{}", mismatches.len(), mismatches.join("\n"));
}
