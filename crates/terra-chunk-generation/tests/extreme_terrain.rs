//! Heights at the edge of `int` (what a Float32 GeoTIFF layer holding ±inf or 1e30 bakes to,
//! since `floor` saturates like Java's `(int)`), combined with water depths and the widest
//! `y_offset` the configuration accepts. Upstream's `int` arithmetic wraps; so must the port,
//! whatever the build profile: with overflow checks (debug and test builds) it used to panic.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;
use terra_chunk_generation::{Block, ChunkDataBuilder, ColumnSink, GeneratorSettings, HeightmapKind, TerrainGenerator, TreeLibrary, TreePopulator, WorldHeight};
use terra_projection::terraplusminus_projection;

struct Clipped {
    world: WorldHeight,
    fills: usize,
}

impl ColumnSink for Clipped {
    fn fill(&mut self, x: usize, z: usize, from_y: i32, to_y: i32, _block: &Block) {
        assert!(x < 16 && z < 16);
        assert!(from_y <= to_y && from_y >= self.world.min_y && to_y < self.world.max_y, "fill {from_y}..={to_y} outside {:?}", self.world);
        self.fills += 1;
    }
}

const HEIGHTS: [f64; 8] = [f64::INFINITY, f64::NEG_INFINITY, 1e30, -1e30, 2_147_483_647.0, -2_147_483_648.0, 2_147_483_000.5, -2_147_483_000.5];

#[test]
fn extreme_heights_neither_panic_nor_escape_the_world() {
    let world = common::WORLD;
    let tree_text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/resources/customTrees.json")).unwrap();
    let mut checked = 0;
    for y_offset in [-4064, 0, 4064] {
        let settings = GeneratorSettings { y_offset, use_biome_dataset: false, ..GeneratorSettings::default() };
        let generator = TerrainGenerator::new(terraplusminus_projection(0, 0).unwrap(), Arc::new(terra_elevation::LayeredScalarDataset::new(Vec::new())), common::offline_provider(), None, settings);
        let populator = Arc::new(TreePopulator::new(TreeLibrary::parse(&tree_text).unwrap(), y_offset));
        for (variant, &height) in HEIGHTS.iter().enumerate() {
            for depth in [None, Some((true, 31)), Some((true, -32)), Some((false, 31)), Some((false, -32)), Some((false, 0))] {
                let mut builder = ChunkDataBuilder::default();
                let mut heights = [height; 256];
                heights[variant] = HEIGHTS[(variant + 1) % HEIGHTS.len()];
                builder.bake_tree_cover(Some(&[1.0; 256]));
                builder.bake_heights(Some(&heights));
                for x in 0..16 {
                    for z in 0..16 {
                        match depth {
                            Some((true, d)) => builder.update_water_depth(x, z, d),
                            Some((false, d)) => builder.update_ocean_depth(x, z, d),
                            None => {}
                        }
                    }
                }
                let terrain = Arc::new(builder.build());
                let mut sink = Clipped { world, fills: 0 };
                generator.fill_chunk(&terrain, 100, 100, world, &mut sink);
                for kind in [HeightmapKind::OceanFloor, HeightmapKind::Surface] {
                    let _ = generator.base_height(&terrain, 3, 4, kind);
                }
                let neighbour = Arc::clone(&terrain);
                let blocks = populator.blocks_in_chunk(100, 100, world, 42, &mut |_, _| Some(Arc::clone(&neighbour)), &|_, _| 8);
                for (x, y, z, _) in blocks {
                    assert!(x < 16 && z < 16 && y >= world.min_y && y < world.max_y, "tree block at ({x}, {y}, {z})");
                }
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 3 * HEIGHTS.len() * 6);
}
