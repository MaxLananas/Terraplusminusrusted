//! The offline stack shared by the integration tests: the repository's terrarium fixture tile
//! served from a local mirror, with a disk tile cache in a scratch directory.

#![allow(dead_code, clippy::unwrap_used)]

use std::path::PathBuf;
use terra_server::{TerraStack, parse_server_config};

pub fn scratch(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!("terra-tpll-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

pub fn stack(name: &str, extra: &str) -> (TerraStack, PathBuf) {
    let directory = scratch(name);
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/elevation/terrarium");
    std::fs::write(
        directory.join("heights.json5"),
        r#"[{"dataset": {"urls": ["mirror://terrarium/12/${x}/${z}.png"], "projection": {"web_mercator": {"zoom": 12}},
            "resolution": 256, "blend": "CUBIC", "parse": {"parse_png_terrarium": {}}},
            "bounds": {"minX": -180, "maxX": 180, "minZ": -85, "maxZ": 85}, "zooms": {"min": 0, "max": 3}}]"#,
    )
    .unwrap();
    let biomes = if extra.contains("\"biomes\"") { "" } else { r#""biomes": {"use_dataset": false},"# };
    let generation = if extra.contains("\"generation\"") { r#""config_version": 1"# } else { r#""generation": {"workers": 2}"# };
    let config = parse_server_config(&format!(
        r#"{{"elevation": {{"heights_config": "heights.json5"}}, {biomes}
            "network": {{"mirrors": {{"mirror://terrarium/": "{}"}}, "cache_directory": "tile-cache"}},
            "world": {{"min_y": -64, "height": 2096}}, {generation} {extra}}}"#,
        fixtures.display()
    ))
    .unwrap();
    (TerraStack::build(config, &directory).unwrap(), directory)
}

