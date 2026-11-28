//! The example configurations the README points operators at keep working.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::process::Command;

fn example(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples").join(name).join("terraplusminus.json5")
}

fn run(arguments: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_terraplusminus")).args(arguments).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(output.status.success(), "{arguments:?} failed:\n{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
    stdout
}

#[test]
fn every_example_configuration_is_valid() {
    for name in ["offline-grand-canyon", "aws-terrain-tiles"] {
        let path = example(name);
        run(&["check-config", path.to_str().unwrap()]);
    }
}

/// The release smoke test: the offline example answers without any network access.
#[test]
fn offline_example_answers_a_column_query() {
    let path = example("offline-grand-canyon");
    let output = run(&["column", path.to_str().unwrap(), "35.995", "-113.247"]);
    assert!(!output.trim().is_empty());
}
