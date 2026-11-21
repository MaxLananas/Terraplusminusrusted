//! The adapter-facing pieces against the offline stack: chunk writing through a palette into a
//! fake engine chunk, and cached biome lookups.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeMap;
use terra_chunk_generation::WorldHeight;
use terra_server::{BiomeLookup, BlockPalette, BlockWriter, GenerationCounters, terrain_for_generation, write_chunk};

#[derive(Default)]
/// Last write wins, as the engine's `set_block` replaces: upstream fills stone up to the ground
/// and then replaces the ground block with the surface block.
struct FakeChunk {
    blocks: BTreeMap<[i32; 3], u8>,
}

impl BlockWriter<u8> for FakeChunk {
    fn set_block(&mut self, position: [i32; 3], state: u8) {
        self.blocks.insert(position, state);
    }
}

fn palette() -> BlockPalette<u8> {
    BlockPalette::new(|name: &str| match name {
        "minecraft:stone" => 1,
        "minecraft:water" => 2,
        "minecraft:grass_block" => 3,
        "minecraft:dirt" => 4,
        "minecraft:sand" => 5,
        "minecraft:snow_block" => 6,
        other => panic!("unexpected block {other}"),
    })
}

#[test]
fn writes_real_columns_up_to_the_surface() {
    let (stack, _) = common::stack("bridge-columns", "");
    let block = stack.generator().projection().from_geo(terra_projection::GeoCoordinate::new(-113.247, 35.995)).unwrap();
    let (chunk_x, chunk_z) = ((block.x.floor() as i32) >> 4, (block.y.floor() as i32) >> 4);
    let counters = GenerationCounters::default();
    let terrain = terrain_for_generation(&stack, &counters, chunk_x, chunk_z, &mut |message| panic!("{message}"));
    let world = WorldHeight { min_y: -64, max_y: 2032 };
    let mut chunk = FakeChunk::default();
    write_chunk(&stack, &terrain, chunk_x, chunk_z, world, &mut palette(), &mut chunk);
    for x in 0..16 {
        for z in 0..16 {
            let (world_x, world_z) = ((chunk_x << 4) + x as i32, (chunk_z << 4) + z as i32);
            let column: Vec<(i32, u8)> = chunk.blocks.iter().filter(|(p, _)| p[0] == world_x && p[2] == world_z).map(|(p, s)| (p[1], *s)).collect();
            let ground = terrain.ground_height(x, z);
            assert_eq!(column.len() as i32, ground - world.min_y + 1, "solid from the bottom to the ground, no gaps");
            assert_eq!(column.last().unwrap().0, ground);
            assert!(column[..column.len() - 1].iter().all(|(_, state)| *state == 1));
            assert!(ground > 500, "real canyon elevation, not a flat fallback");
        }
    }
    let mut again = FakeChunk::default();
    write_chunk(&stack, &terrain, chunk_x, chunk_z, world, &mut palette(), &mut again);
    assert_eq!(chunk.blocks, again.blocks);
}

#[test]
fn biome_lookup_matches_the_generator_and_ignores_height() {
    let (stack, _) = common::stack("bridge-biomes", r#", "biomes": {"use_dataset": true}"#);
    let lookup = BiomeLookup::default();
    let names = BiomeLookup::possible_biomes();
    for (quart_x, quart_z) in [(-2_993_933, -1_349_260), (0, 0), (-1, -1), (879_354, -1_322_059)] {
        let expected = stack.generator().biome_at_block(quart_x * 4, quart_z * 4);
        for _ in 0..3 {
            assert_eq!(names[lookup.biome_index(&stack, quart_x, quart_z) as usize], expected.resource_name());
        }
    }
}
