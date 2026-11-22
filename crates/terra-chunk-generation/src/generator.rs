//! TerraPlusMinus `RealWorldGenerator` and `CustomBiomeProvider`, independent of any server API.

use crate::biome::{Biome, KoppenMap};
use crate::chunk_data::{ChunkDataBuilder, ChunkTerrain};
use crate::java_random::LegacyRandom;
use crate::trees::TreePopulator;
use std::fmt;
use std::sync::Arc;
use terra_elevation::{ElevationError, LayeredScalarDataset, TileProvider, sample_chunk_heights};
use terra_osm::{ChunkElements, OsmDataset, OsmError, apply_elements};
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
    /// `building_outlines_material`, `road_material` and `path_material` (deprecated upstream in
    /// favour of `osm.json5`): surface blocks drawn by OpenStreetMap features, by block id,
    /// replaced with another block.
    pub surface_replacements: Vec<(String, Block)>,
}

impl Default for GeneratorSettings {
    fn default() -> Self {
        Self { x_offset: 0, y_offset: 0, z_offset: 0, use_biome_dataset: true, default_biome: Biome::Plains, surface_material: Block::GrassBlock, surface_replacements: Vec::new() }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum GenerationError {
    Elevation(ElevationError),
    OpenStreetMap(OsmError),
    TreeCover(ElevationError),
    /// Generation of this chunk panicked (a bug, or data the decoders do not handle). The
    /// message is the panic payload. Permanent: the same inputs would panic again.
    Internal(String),
}

impl GenerationError {
    /// Whether retrying the chunk cannot succeed (see [`ElevationError::is_permanent`] and
    /// [`OsmError::is_permanent`]).
    pub fn is_permanent(&self) -> bool {
        match self {
            Self::Elevation(error) | Self::TreeCover(error) => error.is_permanent(),
            Self::OpenStreetMap(error) => error.is_permanent(),
            Self::Internal(_) => true,
        }
    }
}

impl fmt::Display for GenerationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Elevation(error) => write!(f, "elevation data unavailable: {error}"),
            Self::OpenStreetMap(error) => write!(f, "OpenStreetMap data unavailable: {error}"),
            Self::TreeCover(error) => write!(f, "tree cover data unavailable: {error}"),
            Self::Internal(message) => write!(f, "chunk generation failed internally: {message}"),
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
    osm: Option<Arc<OsmDataset>>,
    trees: Option<(Arc<TreePopulator>, Arc<LayeredScalarDataset>)>,
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
        Self { projection, heights, provider, koppen, osm: None, trees: None, settings }
    }

    /// Draws OpenStreetMap features (water, coastlines, roads, buildings) into chunk data.
    #[must_use]
    pub fn with_openstreetmap(mut self, osm: Arc<OsmDataset>) -> Self {
        self.osm = Some(osm);
        self
    }

    /// Bakes tree cover from `tree_cover` (fractions in `[0, 1]`) and places trees with
    /// `populator`.
    #[must_use]
    pub fn with_trees(mut self, populator: Arc<TreePopulator>, tree_cover: Arc<LayeredScalarDataset>) -> Self {
        self.trees = Some((populator, tree_cover));
        self
    }

    pub fn trees(&self) -> Option<&Arc<TreePopulator>> {
        self.trees.as_ref().map(|(populator, _)| populator)
    }

    pub fn openstreetmap(&self) -> Option<&Arc<OsmDataset>> {
        self.osm.as_ref()
    }

    pub fn settings(&self) -> &GeneratorSettings {
        &self.settings
    }

    pub fn projection(&self) -> &SharedProjection {
        &self.projection
    }

    /// `ChunkDataLoader.load` with upstream's bakers in order: tree cover (only when trees are
    /// placed; upstream fetches it regardless), heights, OpenStreetMap (when enabled) and null
    /// island.
    pub fn chunk_terrain(&self, chunk_x: i32, chunk_z: i32) -> Result<ChunkTerrain, GenerationError> {
        let tree_cover = match &self.trees {
            Some((_, dataset)) => Some(
                sample_chunk_heights(self.projection.as_ref(), dataset, self.provider.as_ref(), chunk_x, chunk_z).map_err(GenerationError::TreeCover)?,
            ),
            None => None,
        };
        let heights = sample_chunk_heights(self.projection.as_ref(), &self.heights, self.provider.as_ref(), chunk_x, chunk_z)
            .map_err(GenerationError::Elevation)?;
        let elements = match &self.osm {
            Some(osm) => Some(osm.chunk_elements(chunk_x, chunk_z).map_err(GenerationError::OpenStreetMap)?),
            None => None,
        };
        let mut builder = ChunkDataBuilder::default();
        if let Some(cover) = &tree_cover {
            builder.bake_tree_cover(cover.as_deref());
        }
        builder.bake_heights(heights.as_deref());
        match elements {
            Some(ChunkElements::Elements(elements)) => apply_elements(&elements, &mut builder, chunk_x, chunk_z),
            Some(ChunkElements::OutsideProjection) => builder.bake_outside_projection(),
            None => {}
        }
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

    /// The Köppen class `TreePopulator` switches on at a block column: 8 when the biome dataset is
    /// disabled (upstream's default), 0 where the projection or the raster has no value.
    pub fn koppen_class_at_block(&self, x: i32, z: i32) -> u8 {
        if !self.settings.use_biome_dataset {
            return 8;
        }
        let Ok(geo) = self.projection.to_geo(ProjectedCoordinate::new(f64::from(x), f64::from(z))) else {
            return 0;
        };
        self.koppen.as_ref().and_then(|map| map.class_at(geo.longitude, geo.latitude)).unwrap_or(0)
    }

    /// Tree blocks for a chunk (chunk-local `x`, world `y`, chunk-local `z`), from its own
    /// populator and its neighbours'. Empty when trees are disabled. `neighbour` supplies the
    /// baked data of chunks within [`TreePopulator::chunk_radius`].
    pub fn tree_blocks(
        &self,
        chunk_x: i32,
        chunk_z: i32,
        world: WorldHeight,
        level_seed: i64,
        neighbour: &mut dyn FnMut(i32, i32) -> Option<Arc<ChunkTerrain>>,
    ) -> Vec<(usize, i32, usize, Block)> {
        self.tree_blocks_at(chunk_x, chunk_z, world, level_seed, self.settings.y_offset, neighbour)
    }

    /// [`TerrainGenerator::tree_blocks`] for a level whose terrain sits at `y_offset` (a linked
    /// level) instead of the configured `terrain_offset.y`.
    pub fn tree_blocks_at(
        &self,
        chunk_x: i32,
        chunk_z: i32,
        world: WorldHeight,
        level_seed: i64,
        y_offset: i32,
        neighbour: &mut dyn FnMut(i32, i32) -> Option<Arc<ChunkTerrain>>,
    ) -> Vec<(usize, i32, usize, Block)> {
        let Some((populator, _)) = &self.trees else { return Vec::new() };
        let climate = |x: i32, z: i32| self.koppen_class_at_block(x, z);
        if y_offset == self.settings.y_offset {
            return populator.blocks_in_chunk(chunk_x, chunk_z, world, level_seed, neighbour, &climate);
        }
        populator.with_y_offset(y_offset).blocks_in_chunk(chunk_x, chunk_z, world, level_seed, neighbour, &climate)
    }

    /// `generateNoise` followed by `generateSurface` for one chunk. Height arithmetic is Java's
    /// wrapping `int` arithmetic: saturated heights (a ±inf layer) must not panic in builds with
    /// overflow checks, and must behave as upstream does in all builds.
    pub fn fill_chunk(&self, terrain: &ChunkTerrain, chunk_x: i32, chunk_z: i32, world: WorldHeight, sink: &mut dyn ColumnSink) {
        self.fill_chunk_at(terrain, chunk_x, chunk_z, world, self.settings.y_offset, sink);
    }

    /// [`TerrainGenerator::fill_chunk`] with the terrain at `y_offset`: a linked level's offset.
    pub fn fill_chunk_at(&self, terrain: &ChunkTerrain, chunk_x: i32, chunk_z: i32, world: WorldHeight, y_offset: i32, sink: &mut dyn ColumnSink) {
        let min_surface_cube = world.min_y.wrapping_sub(y_offset) >> 4;
        let fill_noise = !terrain.above_surface(min_surface_cube);
        let top = world.max_y - 1;
        let mut random = LegacyRandom::large_feature_with_salt(0, chunk_x, chunk_z, 0);
        let base_x = chunk_x.wrapping_shl(4);
        let base_z = chunk_z.wrapping_shl(4);
        for x in 0..16 {
            for z in 0..16 {
                let ground = terrain.ground_height(x, z).wrapping_add(y_offset).min(top);
                let water = terrain.water_height(x, z).wrapping_add(y_offset).min(top);
                if fill_noise {
                    if ground >= world.min_y {
                        sink.fill(x, z, world.min_y, ground, &Block::Stone);
                    }
                    let water_bottom = ground.wrapping_add(1).max(world.min_y);
                    if water >= water_bottom {
                        sink.fill(x, z, water_bottom, water, &Block::Water);
                    }
                }
                let ground_y = terrain.ground_height(x, z).wrapping_add(y_offset);
                let start_mountain_height = random.next_int_between(7500, 7520);
                if ground_y < world.min_y || ground_y >= world.max_y {
                    continue;
                }
                let mut surface = match terrain.surface_block(x, z) {
                    Some(block) => self.map_surface_block(&block.0),
                    None if ground_y >= start_mountain_height => Block::Stone,
                    None => {
                        let quart_x = base_x.wrapping_add((x as i32) & !3);
                        let quart_z = base_z.wrapping_add((z as i32) & !3);
                        self.default_surface(self.biome_at_block(quart_x, quart_z))
                    }
                };
                let under_water = ground_y.wrapping_add(1) >= world.max_y || (fill_noise && ground_y < water);
                if under_water && surface.is_grass_like() {
                    surface = Block::Dirt;
                }
                sink.fill(x, z, ground_y, ground_y, &surface);
            }
        }
    }

    /// `BlockMapper.map`: a configured replacement for the block id, else the block state itself.
    fn map_surface_block(&self, state: &str) -> Block {
        let id = state.split_once('[').map_or(state, |(id, _)| id);
        match self.settings.surface_replacements.iter().find(|(replaced, _)| replaced == id) {
            Some((_, replacement)) => replacement.clone(),
            None => Block::from_material_name(state),
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
            HeightmapKind::OceanFloor => terrain.ground_height(x, z).wrapping_add(self.settings.y_offset),
            HeightmapKind::Surface => terrain.surface_height(x, z).wrapping_add(self.settings.y_offset),
        }
    }
}

/// `getFixedSpawnLocation`.
pub const FIXED_SPAWN: (i32, i32, i32) = (3_517_417, 58, -5_288_234);

pub fn project_block(projection: &dyn GeographicProjection, x: f64, z: f64) -> Option<(f64, f64)> {
    projection.to_geo(ProjectedCoordinate::new(x, z)).ok().map(|geo| (geo.longitude, geo.latitude))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permanence_follows_the_cause() {
        let transient = ElevationError::Fetch { url: "u".into(), reason: "reset".into() };
        let permanent = ElevationError::RequestTooLarge { tiles: 9 };
        assert!(!GenerationError::Elevation(transient.clone()).is_permanent());
        assert!(GenerationError::Elevation(permanent.clone()).is_permanent());
        assert!(!GenerationError::TreeCover(transient).is_permanent());
        assert!(GenerationError::TreeCover(permanent).is_permanent());
        assert!(!GenerationError::OpenStreetMap(OsmError::Missing { key: "k".into() }).is_permanent());
        assert!(GenerationError::OpenStreetMap(OsmError::ReferenceCycle { location: "l".into() }).is_permanent());
        assert!(GenerationError::Internal("panic".into()).is_permanent());
    }
}
