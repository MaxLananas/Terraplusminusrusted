//! Generates the offline Grand Canyon area end to end (projection, elevation, Köppen biomes,
//! block columns) and checks seams, determinism and biome data.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{WORLD, centre_chunk, generator};
use std::collections::HashMap;
use std::sync::Arc;
use terra_chunk_generation::{Biome, Block, ColumnSink, KoppenMap};

#[derive(Default)]
struct RecordingSink {
    blocks: HashMap<(usize, i32, usize), Block>,
}

impl ColumnSink for RecordingSink {
    fn fill(&mut self, x: usize, z: usize, from_y: i32, to_y: i32, block: &Block) {
        assert!(from_y <= to_y && from_y >= WORLD.min_y && to_y < WORLD.max_y, "{from_y}..={to_y}");
        for y in from_y..=to_y {
            self.blocks.insert((x, y, z), block.clone());
        }
    }
}

#[test]
fn generates_real_terrain_with_continuous_seams() {
    let generator = generator(false);
    let (cx, cz) = centre_chunk(&generator);
    let mut surfaces = HashMap::new();
    for chunk_x in cx - 2..=cx + 2 {
        for chunk_z in cz - 2..=cz + 2 {
            let terrain = generator.chunk_terrain(chunk_x, chunk_z).unwrap();
            for x in 0..16 {
                for z in 0..16 {
                    surfaces.insert((chunk_x * 16 + x as i32, chunk_z * 16 + z as i32), terrain.surface_height(x, z));
                }
            }
        }
    }
    let (min, max) = surfaces.values().fold((i32::MAX, i32::MIN), |(a, b), &h| (a.min(h), b.max(h)));
    assert!(min > 600 && max < 2200, "heights {min}..{max} are not western Grand Canyon elevations");
    assert!(max - min > 5, "terrain is flat");
    let mut steepest = 0;
    for (&(x, z), &height) in &surfaces {
        for neighbour in [(x + 1, z), (x, z + 1)] {
            if let Some(&other) = surfaces.get(&neighbour) {
                steepest = steepest.max((other - height).abs());
            }
        }
    }
    assert!(steepest <= 8, "discontinuity of {steepest} blocks between adjacent columns");
}

#[test]
fn block_columns_are_stone_capped_by_surface() {
    let generator = generator(false);
    let (cx, cz) = centre_chunk(&generator);
    let terrain = generator.chunk_terrain(cx, cz).unwrap();
    let mut sink = RecordingSink::default();
    generator.fill_chunk(&terrain, cx, cz, WORLD, &mut sink);
    for x in 0..16 {
        for z in 0..16 {
            let ground = terrain.ground_height(x, z);
            assert_eq!(sink.blocks.get(&(x, ground, z)), Some(&Block::GrassBlock));
            assert_eq!(sink.blocks.get(&(x, ground - 1, z)), Some(&Block::Stone));
            assert_eq!(sink.blocks.get(&(x, WORLD.min_y, z)), Some(&Block::Stone));
            assert_eq!(sink.blocks.get(&(x, ground + 1, z)), None);
        }
    }
}

#[test]
fn generation_is_independent_of_order_and_threads() {
    let generator = Arc::new(generator(false));
    let (cx, cz) = centre_chunk(&generator);
    let chunks: Vec<(i32, i32)> = (0..9).map(|i| (cx - 1 + i % 3, cz - 1 + i / 3)).collect();
    let sequential: Vec<_> = chunks.iter().map(|&(x, z)| generator.chunk_terrain(x, z).unwrap()).collect();
    let handles: Vec<_> = chunks
        .iter()
        .rev()
        .map(|&(x, z)| {
            let generator = Arc::clone(&generator);
            std::thread::spawn(move || generator.chunk_terrain(x, z).unwrap())
        })
        .collect();
    let mut parallel: Vec<_> = handles.into_iter().map(|handle| handle.join().unwrap()).collect();
    parallel.reverse();
    assert_eq!(sequential, parallel);
}

#[test]
fn koppen_biomes_cover_the_offline_area() {
    let map = KoppenMap::embedded().unwrap();
    eprintln!("koppen runs: {}", map.runs());
    assert_eq!(map.class_at(0.0, 91.0), None);
    assert_eq!(Biome::from_koppen(map.class_at(-30.0, 0.0).unwrap()), Biome::Ocean);
    let generator = generator(true);
    let (cx, cz) = centre_chunk(&generator);
    let biome = generator.biome_at_block(cx * 16, cz * 16);
    eprintln!("biome at the Grand Canyon fixture: {biome:?}");
    assert_ne!(biome, Biome::Ocean);
}
