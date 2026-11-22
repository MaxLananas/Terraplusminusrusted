//! OpenStreetMap through the whole stack: GeoJSON tiles from a local mirror, projected with the
//! real BTE projection, baked into chunk terrain and written as blocks.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::Path;
use terra_chunk_generation::{ChunkTerrain, GenerationError, WorldHeight};
use terra_osm::OsmError;
use terra_projection::{GeoCoordinate, ProjectedCoordinate};
use terra_server::{BlockPalette, BlockWriter, TerraStack, write_chunk};

const OSM: &str = r#", "openstreetmap": {"servers": ["https://osm.fixture/"]}"#;

/// The Grand Canyon chunk of the elevation fixture.
fn canyon_chunk(stack: &TerraStack) -> (i32, i32) {
    let block = stack.generator().projection().from_geo(GeoCoordinate::new(-113.247, 35.995)).unwrap();
    ((block.x.floor() as i32) >> 4, (block.y.floor() as i32) >> 4)
}

fn position(stack: &TerraStack, x: f64, z: f64) -> String {
    let geo = stack.generator().projection().to_geo(ProjectedCoordinate::new(x, z)).unwrap();
    format!("[{:?},{:?}]", geo.longitude, geo.latitude)
}

/// Writes `lines` to every 1/64-degree tile around the chunk.
fn publish(osm: &Path, stack: &TerraStack, chunk: (i32, i32), lines: &[String]) {
    let geo = stack.generator().projection().to_geo(ProjectedCoordinate::new(f64::from(chunk.0 * 16), f64::from(chunk.1 * 16))).unwrap();
    let (tile_x, tile_z) = ((geo.longitude * 64.0).floor() as i32, (geo.latitude * 64.0).floor() as i32);
    for x in tile_x - 2..=tile_x + 2 {
        for z in tile_z - 2..=tile_z + 2 {
            let directory = osm.join("tile").join(x.to_string());
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join(format!("{z}.json")), lines.join("\n")).unwrap();
        }
    }
}

fn terrain(stack: &TerraStack, chunk: (i32, i32)) -> ChunkTerrain {
    stack.generator().chunk_terrain(chunk.0, chunk.1).unwrap()
}

#[test]
fn rivers_roads_and_buildings_shape_the_chunk() {
    let (stack, directory) = common::stack("osm-features", OSM);
    let (chunk_x, chunk_z) = canyon_chunk(&stack);
    let (base_x, base_z) = (f64::from(chunk_x * 16), f64::from(chunk_z * 16));
    let line = |id: &str, tags: &str, from: (f64, f64), to: (f64, f64)| {
        format!(
            r#"{{"type":"Feature","id":"{id}","properties":{{{tags}}},"geometry":{{"type":"LineString","coordinates":[{},{}]}}}}"#,
            position(&stack, from.0, from.1),
            position(&stack, to.0, to.1)
        )
    };
    let river = line("way/1", r#""waterway":"river""#, (base_x - 48.0, base_z + 8.0), (base_x + 64.0, base_z + 8.0));
    let road = line("way/2", r#""highway":"residential""#, (base_x - 48.0, base_z - 8.0), (base_x + 64.0, base_z - 8.0));
    let ring: Vec<String> = [(4.5, 4.5), (11.5, 4.5), (11.5, 11.5), (4.5, 11.5), (4.5, 4.5)].iter().map(|&(x, z)| position(&stack, base_x + 16.0 + x, base_z + z)).collect();
    let building = format!(r#"{{"type":"Feature","id":"way/3","properties":{{"building":"yes"}},"geometry":{{"type":"Polygon","coordinates":[[{}]]}}}}"#, ring.join(","));
    std::fs::create_dir_all(directory.join("osm/shared")).unwrap();
    std::fs::write(directory.join("osm/shared/river.json"), &river).unwrap();
    publish(&directory.join("osm"), &stack, (chunk_x, chunk_z), &[r#"{"type":"Reference","location":"shared/river.json"}"#.to_owned(), road, building]);

    let river_chunk = terrain(&stack, (chunk_x, chunk_z));
    for x in 0..16 {
        let lowered = river_chunk.surface_height(x, 8) - river_chunk.ground_height(x, 8);
        assert!((5..=6).contains(&lowered), "the river bed is cut below the water surface at ({x}, 8): {lowered}");
        assert_eq!(river_chunk.surface_height(x, 15), river_chunk.ground_height(x, 15), "no water 7 blocks from the bank");
        assert!(!river_chunk.trees_allowed(x, 8));
    }

    let road_chunk = terrain(&stack, (chunk_x, chunk_z - 1));
    assert_eq!(road_chunk.surface_block(4, 8).map(|block| block.0.as_str()), Some("minecraft:gray_concrete"));
    assert_eq!(road_chunk.surface_block(4, 0), None);
    assert!(road_chunk.trees_allowed(4, 0));

    let building_chunk = terrain(&stack, (chunk_x + 1, chunk_z));
    for (x, z) in [(4, 8), (11, 8), (8, 4), (8, 11)] {
        assert_eq!(building_chunk.surface_block(x, z).map(|block| block.0.as_str()), Some("minecraft:bricks"), "outline ({x}, {z})");
    }
    assert_eq!(building_chunk.surface_block(8, 8), None);
    assert!(!building_chunk.trees_allowed(8, 8));

    struct Columns(std::collections::BTreeMap<[i32; 3], &'static str>);
    impl BlockWriter<&'static str> for Columns {
        fn set_block(&mut self, position: [i32; 3], state: &'static str) {
            self.0.insert(position, state);
        }
    }
    let known = ["minecraft:stone", "minecraft:water", "minecraft:grass_block", "minecraft:dirt", "minecraft:sand", "minecraft:snow_block", "minecraft:gray_concrete", "minecraft:dirt_path", "minecraft:bricks"];
    let mut palette = BlockPalette::new(move |name: &str| *known.iter().find(|known| **known == name).unwrap_or_else(|| panic!("unexpected block {name}")));
    let world = WorldHeight { min_y: -64, max_y: 2032 };
    let mut written = Columns(Default::default());
    write_chunk(&stack, &road_chunk, chunk_x, chunk_z - 1, world, &mut palette, &mut written);
    let road_top = road_chunk.ground_height(4, 8);
    assert_eq!(written.0.get(&[chunk_x * 16 + 4, road_top, (chunk_z - 1) * 16 + 8]), Some(&"minecraft:gray_concrete"));
    let mut written = Columns(Default::default());
    write_chunk(&stack, &river_chunk, chunk_x, chunk_z, world, &mut palette, &mut written);
    let (bed, surface) = (river_chunk.ground_height(3, 8), river_chunk.surface_height(3, 8));
    assert_eq!(written.0.get(&[chunk_x * 16 + 3, bed + 1, chunk_z * 16 + 8]), Some(&"minecraft:water"));
    assert_eq!(written.0.get(&[chunk_x * 16 + 3, surface - 1, chunk_z * 16 + 8]), Some(&"minecraft:water"));

    assert_eq!(stack.surface_block_names(), vec!["minecraft:gray_concrete", "minecraft:dirt_path", "minecraft:bricks"]);
    let fingerprint_differs = terra_server::TerraStack::build(
        terra_server::parse_server_config(r#"{"openstreetmap": {"enabled": false}, "biomes": {"use_dataset": false}}"#).unwrap(),
        &directory,
    )
    .unwrap()
    .fingerprint()
    .differences(stack.fingerprint());
    assert!(fingerprint_differs.iter().any(|(key, _, _)| key == "osm_mapping"));
}

#[test]
fn a_missing_tile_fails_the_chunk_instead_of_dropping_features() {
    let (stack, _directory) = common::stack("osm-missing", OSM);
    let chunk = canyon_chunk(&stack);
    match stack.generator().chunk_terrain(chunk.0, chunk.1) {
        Err(GenerationError::OpenStreetMap(OsmError::Missing { key })) => assert!(key.starts_with("tile/"), "{key}"),
        other => panic!("expected a missing tile, got {other:?}"),
    }
}

#[test]
fn an_edited_mapping_beside_the_config_is_used() {
    let directory = common::scratch("osm-mapping");
    std::fs::write(directory.join("osm.json5"), r#"{"line": {"nothing": {}}, "polygon": {"nothing": {}}}"#).unwrap();
    let config = terra_server::parse_server_config(r#"{"biomes": {"use_dataset": false}, "generate_trees": false}"#).unwrap();
    let stack = TerraStack::build(config, &directory).unwrap();
    assert!(stack.surface_block_names().is_empty(), "no OSM blocks and no tree blocks");
    std::fs::write(directory.join("osm.json5"), r#"{"line": {"nothing": {}}}"#).unwrap();
    let config = terra_server::parse_server_config(r#"{"biomes": {"use_dataset": false}}"#).unwrap();
    let error = TerraStack::build(config, &directory).err().expect("an invalid mapping refuses to start");
    assert!(error.to_string().contains("osm.json5"), "{error}");
    let fresh = common::scratch("osm-extract");
    assert!(terra_server::extract_default_osm_mapping(&fresh).unwrap());
    assert!(!terra_server::extract_default_osm_mapping(&fresh).unwrap(), "an existing file is kept");
    assert_eq!(std::fs::read_to_string(fresh.join("osm.json5")).unwrap(), terra_osm::DEFAULT_OSM_CONFIG);
}
