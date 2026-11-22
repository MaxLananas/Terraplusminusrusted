//! Tree cover and `TreePopulator` through the whole stack: TIFF tree cover from the synthetic
//! `fixtures/treecover` tiles, chunk data, and tree blocks written over terrain.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeMap;
use terra_chunk_generation::WorldHeight;
use terra_projection::GeoCoordinate;
use terra_server::{BlockPalette, BlockWriter, GenerationControl, TreeTarget, terrain_for_generation, write_chunk, write_trees};

/// Inside the terrarium fixture tile (z13 tile row 3216), away from the no-data band.
const LATITUDE: f64 = 36.0136;
const WORLD: WorldHeight = WorldHeight { min_y: -64, max_y: 2032 };

#[derive(Default)]
struct FakeChunk {
    blocks: BTreeMap<[i32; 3], String>,
}

impl BlockWriter<usize> for FakeChunk {
    fn set_block(&mut self, position: [i32; 3], state: usize) {
        self.blocks.insert(position, NAMES.with(|names| names.borrow()[state].clone()));
    }
}

thread_local! {
    static NAMES: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn palette() -> BlockPalette<usize> {
    BlockPalette::new(|name: &str| {
        NAMES.with(|names| {
            let mut names = names.borrow_mut();
            names.push(name.to_owned());
            names.len() - 1
        })
    })
}

/// The chunk containing a longitude/latitude.
fn chunk_at(stack: &terra_server::TerraStack, longitude: f64, latitude: f64) -> (i32, i32) {
    let block = stack.generator().projection().from_geo(GeoCoordinate::new(longitude, latitude)).unwrap();
    ((block.x.floor() as i32) >> 4, (block.y.floor() as i32) >> 4)
}

#[test]
fn tree_cover_is_baked_from_tiff_tiles() {
    let (stack, _) = common::stack("trees-cover", r#", "generate_trees": true"#);
    let control = GenerationControl::default();
    // fixtures/treecover: 0 % in the west, ramping to 100 % at the terrarium fixture's east edge.
    let west = chunk_at(&stack, -113.26, LATITUDE);
    let east = chunk_at(&stack, -113.21, LATITUDE);
    let west_terrain = terrain_for_generation(&stack, &control, west.0, west.1, &mut |message| panic!("{message}")).unwrap();
    let east_terrain = terrain_for_generation(&stack, &control, east.0, east.1, &mut |message| panic!("{message}")).unwrap();
    let (west_chance, east_chance) = (west_terrain.tree_chance(0, 0), east_terrain.tree_chance(0, 0));
    assert!(west_chance < east_chance, "{west_chance} < {east_chance}");
    // treeChance(cover) = ceil(cover / 4 * 255 * 1.5); 100 % cover is 96.
    assert!(east_chance > 50 && east_chance <= 96, "{east_chance}");
    // GDAL no-data (row 128 of tile 13/1519/3218, south of the terrarium fixture) is NaN, which
    // treeChance maps to 0.
    let no_data = chunk_at(&stack, -113.21, 35.942_434);
    let no_data_terrain = terrain_for_generation(&stack, &control, no_data.0, no_data.1, &mut |message| panic!("{message}")).unwrap();
    assert_eq!(no_data_terrain.tree_chance(8, 8), 0);
    assert!(stack.fingerprint().field("tree_cover_config").is_some());
}

#[test]
fn trees_grow_on_covered_land_and_overhang_into_neighbours() {
    let (stack, _) = common::stack("trees-grow", r#", "generate_trees": true"#);
    let control = GenerationControl::default();
    let (chunk_x, chunk_z) = chunk_at(&stack, -113.21, LATITUDE);
    let terrain = terrain_for_generation(&stack, &control, chunk_x, chunk_z, &mut |message| panic!("{message}")).unwrap();
    let target = TreeTarget { chunk_x, chunk_z, world: WORLD, level_seed: 1234 };
    let mut palette = palette();
    let mut chunk = FakeChunk::default();
    write_chunk(&stack, &terrain, chunk_x, chunk_z, WORLD, &mut palette, &mut chunk);
    let terrain_blocks = chunk.blocks.len();
    let written = write_trees(&stack, &control, &terrain, target, &mut palette, &mut chunk, &mut |message| panic!("{message}")).unwrap();
    assert!(written > 0, "dense cover grows trees");
    let (base_x, base_z) = (chunk_x << 4, chunk_z << 4);
    let tree_blocks: Vec<_> = chunk.blocks.iter().filter(|(_, name)| name.contains("leaves") || name.contains("fence") || name.contains("log") || name.contains("wood")).collect();
    assert!(!tree_blocks.is_empty());
    for (position, _) in &tree_blocks {
        assert!((base_x..base_x + 16).contains(&position[0]) && (base_z..base_z + 16).contains(&position[2]), "clipped to the chunk");
        let ground = terrain.ground_height((position[0] - base_x) as usize, (position[2] - base_z) as usize);
        assert!(position[1] > ground - 40, "near the surface");
    }
    assert!(chunk.blocks.len() > terrain_blocks);
    assert!(tree_blocks.iter().any(|(_, name)| name.ends_with("[persistent=true]")));
    // Deterministic per seed, different across seeds.
    let mut again = FakeChunk::default();
    write_trees(&stack, &control, &terrain, target, &mut palette, &mut again, &mut |message| panic!("{message}")).unwrap();
    let mut other_seed = FakeChunk::default();
    write_trees(&stack, &control, &terrain, TreeTarget { level_seed: 99, ..target }, &mut palette, &mut other_seed, &mut |message| panic!("{message}")).unwrap();
    let mut third = FakeChunk::default();
    write_trees(&stack, &control, &terrain, target, &mut palette, &mut third, &mut |message| panic!("{message}")).unwrap();
    assert!(!again.blocks.is_empty() && again.blocks.len() <= written);
    assert_eq!(again.blocks, third.blocks);
    assert_ne!(again.blocks, other_seed.blocks);
    // The registered palette names every block the trees place.
    let names = stack.surface_block_names();
    for (_, name) in &tree_blocks {
        assert!(names.contains(name), "{name} is registered");
    }
}

#[test]
fn bare_ground_and_disabled_trees_place_nothing() {
    let (stack, _) = common::stack("trees-bare", r#", "generate_trees": true"#);
    let control = GenerationControl::default();
    let (chunk_x, chunk_z) = chunk_at(&stack, -113.2855, LATITUDE);
    let terrain = terrain_for_generation(&stack, &control, chunk_x, chunk_z, &mut |message| panic!("{message}")).unwrap();
    assert_eq!(terrain.tree_chance(0, 0), 0, "0 % cover at the west edge");
    let mut chunk = FakeChunk::default();
    let target = TreeTarget { chunk_x, chunk_z, world: WORLD, level_seed: 1 };
    // Neighbours may still reach in; the chunk's own populator places nothing.
    let own = stack.generator().trees().unwrap().populate(&terrain, chunk_x, chunk_z, WORLD, 1, &|_, _| 8);
    assert!(own.is_empty());
    let _ = write_trees(&stack, &control, &terrain, target, &mut palette(), &mut chunk, &mut |message| panic!("{message}")).unwrap();

    let (disabled, _) = common::stack("trees-disabled", "");
    assert!(disabled.generator().trees().is_none());
    assert_eq!(disabled.fingerprint().field("trees"), None);
    let terrain = terrain_for_generation(&disabled, &control, chunk_x, chunk_z, &mut |message| panic!("{message}")).unwrap();
    assert_eq!(write_trees(&disabled, &control, &terrain, target, &mut palette(), &mut FakeChunk::default(), &mut |message| panic!("{message}")), Ok(0));
}
