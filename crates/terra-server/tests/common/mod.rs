//! The offline stack shared by the integration tests: the repository's terrarium fixture tile
//! served from a local mirror, with a disk tile cache in a scratch directory. OpenStreetMap is
//! off unless `extra` configures it; `https://osm.fixture/` mirrors `<scratch>/osm`. Trees are
//! off unless `extra` sets `generate_trees`; tree cover then comes from the synthetic tiles in
//! `fixtures/treecover` through the upstream `tree_cover.json5` schema.

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
    let (config, directory) = config(name, extra);
    (TerraStack::build(config, &directory).unwrap(), directory)
}

/// The configuration [`stack`] builds, and its scratch directory.
pub fn config(name: &str, extra: &str) -> (terra_server::ServerConfig, PathBuf) {
    let directory = scratch(name);
    std::fs::create_dir_all(directory.join("osm")).unwrap();
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/elevation/terrarium");
    let tree_cover = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/treecover");
    // Upstream's tree_cover.json5 with the URL pointed at the mirror.
    std::fs::write(
        directory.join("tree_cover.json5"),
        terra_elevation::DEFAULT_TREE_COVER.replace("https://cloud.daporkchop.net/gis/treecover2000/", "mirror://treecover/"),
    )
    .unwrap();
    std::fs::write(
        directory.join("heights.json5"),
        r#"[{"dataset": {"urls": ["mirror://terrarium/12/${x}/${z}.png"], "projection": {"web_mercator": {"zoom": 12}},
            "resolution": 256, "blend": "CUBIC", "parse": {"parse_png_terrarium": {}}},
            "bounds": {"minX": -180, "maxX": 180, "minZ": -85, "maxZ": 85}, "zooms": {"min": 0, "max": 3}}]"#,
    )
    .unwrap();
    let biomes = if extra.contains("\"biomes\"") { "" } else { r#""biomes": {"use_dataset": false},"# };
    let osm = if extra.contains("\"openstreetmap\"") { "" } else { r#""openstreetmap": {"enabled": false},"# };
    let trees = if extra.contains("\"generate_trees\"") { "" } else { r#""generate_trees": false,"# };
    let generation = if extra.contains("\"generation\"") { r#""config_version": 1"# } else { r#""generation": {"workers": 2}"# };
    let config = parse_server_config(&format!(
        r#"{{"elevation": {{"heights_config": "heights.json5", "tree_cover_config": "tree_cover.json5"}}, {biomes} {osm} {trees}
            "network": {{"mirrors": {{"mirror://terrarium/": "{}", "mirror://treecover/": "{}", "https://osm.fixture/": "osm"}}, "cache_directory": "tile-cache"}},
            "world": {{"min_y": -64, "height": 2096}}, {generation} {extra}}}"#,
        fixtures.display(),
        tree_cover.display()
    ))
    .unwrap();
    (config, directory)
}

