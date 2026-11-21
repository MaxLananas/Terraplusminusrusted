//! TerraPlusMinus `RealWorldGenerator` and `CustomBiomeProvider`, independent of any server API.

use crate::biome::{Biome, KoppenMap};
use crate::chunk_data::{ChunkDataBuilder, ChunkTerrain};
use crate::java_random::LegacyRandom;
use std::fmt;
use std::sync::Arc;
use terra_elevation::{ElevationError, LayeredScalarDataset, TileProvider, sample_chunk_heights};
use terra_projection::{GeographicProjection, ProjectedCoordinate, SharedProjection};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Block {
    Stone,
    Water,
    GrassBlock,
    Dirt,
    Sand,
    SnowBlock,
    /// Any other block, by resource name (for example from `surface_material`).
    Named(String),
}

impl Block {
    pub fn resource_name(&self) -> &str {
        match self {
            Self::Stone => "minecraft:stone",
            Self::Water => "minecraft:water",
            Self::GrassBlock => "minecraft:grass_block",
            Self::Dirt => "minecraft:dirt",
            Self::Sand => "minecraft:sand",
            Self::SnowBlock => "minecraft:snow_block",
            Self::Named(name) => name,
        }
    }

    /// Parses a Bukkit material (`GRASS_BLOCK`) or resource name (`minecraft:grass_block`).
    pub fn from_material_name(name: &str) -> Self {
        let lower = name.trim().to_ascii_lowercase();
        let qualified = if lower.contains(':') { lower } else { format!("minecraft:{lower}") };
        match qualified.as_str() {
            "minecraft:stone" => Self::Stone,
            "minecraft:water" => Self::Water,
            "minecraft:grass_block" => Self::GrassBlock,
            "minecraft:dirt" => Self::Dirt,
            "minecraft:sand" => Self::Sand,
            "minecraft:snow_block" => Self::SnowBlock,
            _ => Self::Named(qualified),
        }
    }

    /// `GRASS_LIKE_MATERIALS`: surfaces replaced by dirt under water.
    fn is_grass_like(&self) -> bool {
        matches!(self.resource_name(), "minecraft:grass_block" | "minecraft:dirt_path" | "minecraft:farmland" | "minecraft:mycelium" | "minecraft:snow")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GeneratorSettings {
    pub x_offset: i32,
    pub y_offset: i32,
    pub z_offset: i32,
    /// `biomes.use_dataset`.
    pub use_biome_dataset: bool,
    /// `biomes.biome`.
    pub default_biome: Biome,
    /// `surface_material`.
    pub surface_material: Block,
}

impl Default for GeneratorSettings {
    fn default() -> Self {
        Self { x_offset: 0, y_offset: 0, z_offset: 0, use_biome_dataset: true, default_biome: Biome::Plains, surface_material: Block::GrassBlock }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum GenerationError {
    Elevation(ElevationError),
}

impl fmt::Display for GenerationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Elevation(error) => write!(f, "elevation data unavailable: {error}"),
        }
    }
}

impl std::error::Error for GenerationError {}

/// Writes block columns into a chunk; coordinates are chunk-local x/z and absolute y.
pub trait ColumnSink {
    /// Fills `from_y..=to_y` (already clipped to the world height) with `block`.
    fn fill(&mut self, x: usize, z: usize, from_y: i32, to_y: i32, block: &Block);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldHeight {
    pub min_y: i32,
    /// Exclusive.
    pub max_y: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeightmapKind {
    OceanFloor,
    Surface,
}

pub struct TerrainGenerator {
    projection: SharedProjection,
    heights: Arc<LayeredScalarDataset>,
    provider: Arc<dyn TileProvider>,
    koppen: Option<Arc<KoppenMap>>,
    settings: GeneratorSettings,
}

impl TerrainGenerator {
    /// `koppen` is required when `settings.use_biome_dataset` is set.
    pub fn new(
        projection: SharedProjection,
        heights: Arc<LayeredScalarDataset>,
        provider: Arc<dyn TileProvider>,
        koppen: Option<Arc<KoppenMap>>,
        settings: GeneratorSettings,
    ) -> Self {
        Self { projection, heights, provider, koppen, settings }
    }

    pub fn settings(&self) -> &GeneratorSettings {
        &self.settings
    }

    pub fn projection(&self) -> &SharedProjection {
        &self.projection
    }

    /// `ChunkDataLoader.load` restricted to the bakers that shape terrain: heights and null island.
    pub fn chunk_terrain(&self, chunk_x: i32, chunk_z: i32) -> Result<ChunkTerrain, GenerationError> {
        let heights = sample_chunk_heights(self.projection.as_ref(), &self.heights, self.provider.as_ref(), chunk_x, chunk_z)
            .map_err(GenerationError::Elevation)?;
        let mut builder = ChunkDataBuilder::default();
        builder.bake_heights(heights.as_deref());
        builder.bake_null_island(chunk_x, chunk_z);
        Ok(builder.build())
    }

    /// `CustomBiomeProvider.getBiome` at block coordinates.
    pub fn biome_at_block(&self, x: i32, z: i32) -> Biome {
        if !self.settings.use_biome_dataset {
            return self.settings.default_biome;
        }
        let Ok(geo) = self.projection.to_geo(ProjectedCoordinate::new(f64::from(x), f64::from(z))) else {
            return Biome::Plains;
        };
        match self.koppen.as_ref().and_then(|map| map.class_at(geo.longitude, geo.latitude)) {
            Some(class) => Biome::from_koppen(class),
            None => self.settings.default_biome,
        }
    }

    /// `generateNoise` followed by `generateSurface` for one chunk.
    pub fn fill_chunk(&self, terrain: &ChunkTerrain, chunk_x: i32, chunk_z: i32, world: WorldHeight, sink: &mut dyn ColumnSink) {
        let y_offset = self.settings.y_offset;
        let min_surface_cube = world.min_y.wrapping_sub(y_offset) >> 4;
        let fill_noise = !terrain.above_surface(min_surface_cube);
        let top = world.max_y - 1;
        let mut random = LegacyRandom::large_feature_with_salt(0, chunk_x, chunk_z, 0);
        let base_x = chunk_x.wrapping_shl(4);
        let base_z = chunk_z.wrapping_shl(4);
        for x in 0..16 {
            for z in 0..16 {
                let ground = (terrain.ground_height(x, z) + y_offset).min(top);
                let water = (terrain.water_height(x, z) + y_offset).min(top);
                if fill_noise {
                    if ground >= world.min_y {
                        sink.fill(x, z, world.min_y, ground, &Block::Stone);
                    }
                    let water_bottom = (ground + 1).max(world.min_y);
                    if water >= water_bottom {
                        sink.fill(x, z, water_bottom, water, &Block::Water);
                    }
                }
                let ground_y = terrain.ground_height(x, z) + y_offset;
                let start_mountain_height = random.next_int_between(7500, 7520);
                if ground_y < world.min_y || ground_y >= world.max_y {
                    continue;
                }
                let mut surface = match terrain.surface_block(x, z) {
                    Some(block) => Block::from_material_name(&block.0),
                    None if ground_y >= start_mountain_height => Block::Stone,
                    None => {
                        let quart_x = base_x.wrapping_add((x as i32) & !3);
                        let quart_z = base_z.wrapping_add((z as i32) & !3);
                        self.default_surface(self.biome_at_block(quart_x, quart_z))
                    }
                };
                let under_water = ground_y + 1 >= world.max_y || (fill_noise && ground_y < water);
                if under_water && surface.is_grass_like() {
                    surface = Block::Dirt;
                }
                sink.fill(x, z, ground_y, ground_y, &surface);
            }
        }
    }

    fn default_surface(&self, biome: Biome) -> Block {
        match biome {
            Biome::Desert => Block::Sand,
            Biome::SnowySlopes | Biome::SnowyPlains | Biome::FrozenPeaks => Block::SnowBlock,
            _ => self.settings.surface_material.clone(),
        }
    }

    /// `getBaseHeight` for chunk-local columns of `terrain`.
    pub fn base_height(&self, terrain: &ChunkTerrain, x: usize, z: usize, kind: HeightmapKind) -> i32 {
        match kind {
            HeightmapKind::OceanFloor => terrain.ground_height(x, z) + self.settings.y_offset,
            HeightmapKind::Surface => terrain.surface_height(x, z) + self.settings.y_offset,
        }
    }
}

/// `getFixedSpawnLocation`.
pub const FIXED_SPAWN: (i32, i32, i32) = (3_517_417, 58, -5_288_234);

pub fn project_block(projection: &dyn GeographicProjection, x: f64, z: f64) -> Option<(f64, f64)> {
    projection.to_geo(ProjectedCoordinate::new(x, z)).ok().map(|geo| (geo.longitude, geo.latitude))
}
