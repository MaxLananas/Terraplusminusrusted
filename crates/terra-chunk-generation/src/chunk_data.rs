//! `CachedChunkData` and its builder: per-column surface and ground heights after water baking.

use terra_elevation::numeric::{floor_i, lerp};
use terra_osm::{BlockState, DrawTarget};

pub const BLANK_HEIGHT: i32 = -1;
const WATER_DEPTH_OFFSET: i32 = 1;
const WATER_DEPTH_DEFAULT: i8 = i8::MIN;
const TYPE_MASK: i32 = 0xC0u8 as i8 as i32;
const TYPE_DEFAULT: i32 = 0x80u8 as i8 as i32;
const TYPE_WATER: i32 = 0x00;
const TYPE_OCEAN: i32 = 0x40;
const COLUMNS: usize = 256;

fn column(x: usize, z: usize) -> usize {
    x * 16 + z
}

/// `TreeCoverBaker.FALLBACK_TREE_DENSITY`, read when no tree cover was baked: `treeChance(50.0)`.
pub const FALLBACK_TREE_DENSITY: u8 = 255;

/// `TreeCoverBaker.treeChance`: a tree-cover fraction in `[0, 1]` to a per-cell tree chance out
/// of 256 (a tree covers 2x2 blocks; the result is raised by half and clamped).
pub fn tree_chance(value: f64) -> u8 {
    if value.is_nan() {
        return 0;
    }
    let scaled = value * (1.0 / (2.0 * 2.0)) * 255.0 * 1.50;
    // `(int) Math.ceil` saturates like Rust's float-to-int cast.
    (scaled.ceil() as i32).clamp(0, 255) as u8
}

/// Surface block requested by a data baker (OSM features); `None` defers to biome rules.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SurfaceBlock(pub String);

#[derive(Debug, Clone)]
pub struct ChunkDataBuilder {
    surface_height: [i32; COLUMNS],
    water_depth: [i8; COLUMNS],
    surface_blocks: Vec<Option<SurfaceBlock>>,
    trees_cleared: [bool; COLUMNS],
    tree_cover: Option<[u8; COLUMNS]>,
}

impl Default for ChunkDataBuilder {
    fn default() -> Self {
        Self {
            surface_height: [BLANK_HEIGHT; COLUMNS],
            water_depth: [WATER_DEPTH_DEFAULT; COLUMNS],
            surface_blocks: vec![None; COLUMNS],
            trees_cleared: [false; COLUMNS],
            tree_cover: None,
        }
    }
}

/// `OSMBaker` draws through the same builder methods as upstream's `CachedChunkData.Builder`.
impl DrawTarget for ChunkDataBuilder {
    fn update_water_depth(&mut self, x: usize, z: usize, depth: i32) {
        ChunkDataBuilder::update_water_depth(self, x, z, depth);
    }

    fn update_ocean_depth(&mut self, x: usize, z: usize, depth: i32) {
        ChunkDataBuilder::update_ocean_depth(self, x, z, depth);
    }

    fn set_surface_block(&mut self, x: usize, z: usize, block: &BlockState) {
        ChunkDataBuilder::set_surface_block(self, x, z, Some(SurfaceBlock(block.to_string())));
    }

    fn clear_tree_cover(&mut self, x: usize, z: usize) {
        self.trees_cleared[column(x, z)] = true;
    }
}

impl ChunkDataBuilder {
    pub fn set_surface_height(&mut self, x: usize, z: usize, value: i32) {
        self.surface_height[column(x, z)] = value;
    }

    pub fn surface_height(&self, x: usize, z: usize) -> i32 {
        self.surface_height[column(x, z)]
    }

    pub fn set_surface_block(&mut self, x: usize, z: usize, block: Option<SurfaceBlock>) {
        self.surface_blocks[column(x, z)] = block;
    }

    pub fn surface_block(&self, x: usize, z: usize) -> Option<&SurfaceBlock> {
        self.surface_blocks[column(x, z)].as_ref()
    }

    /// The encoded water depth byte: `WATERDEPTH_TYPE_*` in the top two bits, depth + 32 below.
    pub fn water_depth(&self, x: usize, z: usize) -> i8 {
        self.water_depth[column(x, z)]
    }

    /// The baked tree chance before `no_trees` draws, if tree cover was baked.
    pub fn tree_cover(&self, x: usize, z: usize) -> Option<u8> {
        self.tree_cover.map(|cover| cover[column(x, z)])
    }

    /// `TreeCoverBaker.bake`: `None` (no data, or outside the projection) bakes zero chance.
    pub fn bake_tree_cover(&mut self, cover: Option<&[f64]>) {
        let mut chances = [0u8; COLUMNS];
        if let Some(cover) = cover.filter(|cover| cover.len() == COLUMNS) {
            for (chance, &value) in chances.iter_mut().zip(cover) {
                *chance = tree_chance(value);
            }
        }
        self.tree_cover = Some(chances);
    }

    /// Whether a `no_trees` draw zeroed this column's tree-cover chance.
    pub fn trees_cleared(&self, x: usize, z: usize) -> bool {
        self.trees_cleared[column(x, z)]
    }

    pub fn update_water_depth(&mut self, x: usize, z: usize, depth: i32) {
        self.update_depth(column(x, z), depth, TYPE_WATER);
    }

    pub fn update_ocean_depth(&mut self, x: usize, z: usize, depth: i32) {
        self.update_depth(column(x, z), depth, TYPE_OCEAN);
    }

    fn update_depth(&mut self, index: usize, depth: i32, kind: i32) {
        let encoded = depth.wrapping_add(32).clamp(0, 0x3F) | kind;
        if encoded > i32::from(self.water_depth[index]) {
            self.water_depth[index] = encoded as i8;
        }
    }

    /// `OSMBaker.bake` without data: the chunk is outside the projection, so all of it is ocean.
    pub fn bake_outside_projection(&mut self) {
        self.water_depth.fill((TYPE_OCEAN | !TYPE_MASK) as i8);
    }

    /// `HeightsBaker.bake`.
    pub fn bake_heights(&mut self, heights: Option<&[f64]>) {
        let Some(heights) = heights.filter(|heights| heights.len() == COLUMNS) else {
            self.water_depth.fill((TYPE_OCEAN | !TYPE_MASK) as i8);
            return;
        };
        for x in 0..16 {
            for z in 0..16 {
                let height = heights[column(x, z)];
                if height.is_nan() {
                    self.update_ocean_depth(x, z, 0);
                } else {
                    self.set_surface_height(x, z, floor_i(height));
                }
            }
        }
    }

    /// `NullIslandBaker.bake`.
    pub fn bake_null_island(&mut self, chunk_x: i32, chunk_z: i32) {
        if is_null_island(chunk_x, chunk_z) {
            self.surface_height.fill(-1);
            self.trees_cleared.fill(true);
            self.water_depth.fill(WATER_DEPTH_DEFAULT);
        }
    }

    pub fn build(&self) -> ChunkTerrain {
        let mut surface_height = self.surface_height;
        let mut ground_height = self.surface_height;
        for i in 0..COLUMNS {
            let water_depth = i32::from(self.water_depth[i]);
            let depth = (((water_depth & 0x3F) - 32) << 26) >> 26;
            match water_depth & TYPE_MASK {
                TYPE_WATER => {
                    if depth + WATER_DEPTH_OFFSET >= 0 {
                        ground_height[i] = ground_height[i].wrapping_sub(depth + WATER_DEPTH_OFFSET);
                    }
                }
                TYPE_OCEAN => {
                    if depth < 0 {
                        let t = f64::from(!depth) / 8.0;
                        surface_height[i] = floor_i(lerp(0.0, f64::from(surface_height[i]), t));
                        ground_height[i] = floor_i(lerp(-1.0, f64::from(ground_height[i]), t));
                    } else {
                        surface_height[i] = 0;
                        ground_height[i] = ground_height[i].min(-2);
                    }
                }
                _ => {}
            }
        }
        debug_assert!(self.water_depth.iter().all(|&d| matches!(i32::from(d) & TYPE_MASK, TYPE_DEFAULT | TYPE_WATER | TYPE_OCEAN)));
        let extremes = surface_height.iter().chain(ground_height.iter()).copied();
        let min = extremes.clone().min().unwrap_or(0);
        let max = extremes.max().unwrap_or(0);
        ChunkTerrain {
            surface_height,
            ground_height,
            surface_blocks: self.surface_blocks.clone(),
            trees_cleared: self.trees_cleared,
            tree_cover: self.tree_cover,
            surface_min_cube: (min >> 4) - 1,
            surface_max_cube: (max >> 4) + 1,
        }
    }
}

pub fn is_null_island(chunk_x: i32, chunk_z: i32) -> bool {
    (chunk_x ^ (chunk_x >> 31)).max(chunk_z ^ (chunk_z >> 31)) < 3
}

/// Immutable baked chunk data (`CachedChunkData`).
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkTerrain {
    surface_height: [i32; COLUMNS],
    ground_height: [i32; COLUMNS],
    surface_blocks: Vec<Option<SurfaceBlock>>,
    trees_cleared: [bool; COLUMNS],
    tree_cover: Option<[u8; COLUMNS]>,
    surface_min_cube: i32,
    surface_max_cube: i32,
}

impl ChunkTerrain {
    pub fn surface_height(&self, x: usize, z: usize) -> i32 {
        self.surface_height[column(x, z)]
    }

    pub fn ground_height(&self, x: usize, z: usize) -> i32 {
        self.ground_height[column(x, z)]
    }

    pub fn water_height(&self, x: usize, z: usize) -> i32 {
        self.surface_height(x, z).wrapping_sub(1)
    }

    pub fn surface_block(&self, x: usize, z: usize) -> Option<&SurfaceBlock> {
        self.surface_blocks[column(x, z)].as_ref()
    }

    /// False where an OpenStreetMap feature drew `no_trees` (roads, buildings, rivers).
    pub fn trees_allowed(&self, x: usize, z: usize) -> bool {
        !self.trees_cleared[column(x, z)]
    }

    /// The tree chance out of 256 that `TreePopulator` reads: the baked tree cover, zero where
    /// `no_trees` was drawn or on null island, and the fallback density when nothing was baked.
    /// Upstream only clears baked cover; cover is always baked when trees are generated.
    pub fn tree_chance(&self, x: usize, z: usize) -> u8 {
        match self.tree_cover {
            Some(_) if self.trees_cleared[column(x, z)] => 0,
            Some(cover) => cover[column(x, z)],
            None => FALLBACK_TREE_DENSITY,
        }
    }

    pub fn above_surface(&self, cube_y: i32) -> bool {
        cube_y > self.surface_max_cube
    }

    pub fn surface_cube_range(&self) -> (i32, i32) {
        (self.surface_min_cube, self.surface_max_cube)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_chance_matches_tree_cover_baker() {
        assert_eq!(tree_chance(50.0), FALLBACK_TREE_DENSITY);
        assert_eq!(tree_chance(f64::NAN), 0);
        assert_eq!(tree_chance(0.0), 0);
        assert_eq!(tree_chance(-0.5), 0);
        assert_eq!(tree_chance(f64::INFINITY), 255);
        // 0.94 * 0.25 * 255 * 1.5 = 89.8875, rounded up.
        assert_eq!(tree_chance(0.94), 90);
        assert_eq!(tree_chance(0.01), 1);
        assert_eq!(tree_chance(1.0), 96);
    }

    #[test]
    fn tree_cover_is_cleared_by_no_trees_and_null_island() {
        let cover = [0.5; COLUMNS];
        let mut builder = ChunkDataBuilder::default();
        builder.bake_heights(Some(&[10.0; COLUMNS]));
        assert_eq!(builder.build().tree_chance(0, 0), FALLBACK_TREE_DENSITY);
        builder.bake_tree_cover(Some(&cover));
        DrawTarget::clear_tree_cover(&mut builder, 3, 4);
        let terrain = builder.build();
        assert_eq!((terrain.tree_chance(0, 0), terrain.tree_chance(3, 4)), (tree_chance(0.5), 0));
        let mut outside = ChunkDataBuilder::default();
        outside.bake_tree_cover(None);
        assert_eq!(outside.build().tree_chance(7, 7), 0);
        let mut island = ChunkDataBuilder::default();
        island.bake_tree_cover(Some(&cover));
        island.bake_null_island(1, -2);
        assert_eq!(island.build().tree_chance(9, 9), 0);
    }

    #[test]
    fn missing_heights_become_shallow_ocean() {
        let mut builder = ChunkDataBuilder::default();
        builder.bake_heights(None);
        let terrain = builder.build();
        assert_eq!((terrain.surface_height(3, 4), terrain.ground_height(3, 4)), (0, -2));
    }

    #[test]
    fn nan_heights_are_sea_level_columns() {
        let mut heights = vec![12.7; 256];
        heights[column(1, 2)] = f64::NAN;
        let mut builder = ChunkDataBuilder::default();
        builder.bake_heights(Some(&heights));
        let terrain = builder.build();
        assert_eq!(terrain.surface_height(0, 0), 12);
        assert_eq!(terrain.ground_height(0, 0), 12);
        assert_eq!((terrain.surface_height(1, 2), terrain.ground_height(1, 2)), (0, -2));
    }

    #[test]
    fn negative_heights_floor_toward_negative_infinity() {
        let mut builder = ChunkDataBuilder::default();
        builder.bake_heights(Some(&[-0.25; 256]));
        let terrain = builder.build();
        assert_eq!(terrain.surface_height(15, 15), -1);
        assert_eq!(terrain.surface_cube_range(), (-2, 0));
    }

    #[test]
    fn inland_water_lowers_ground_only() {
        let mut builder = ChunkDataBuilder::default();
        builder.bake_heights(Some(&[50.0; 256]));
        builder.update_water_depth(2, 3, 4);
        let terrain = builder.build();
        assert_eq!((terrain.surface_height(2, 3), terrain.ground_height(2, 3)), (50, 45));
    }

    #[test]
    fn null_island_spans_six_chunks_per_axis() {
        assert!(is_null_island(-3, 2));
        assert!(!is_null_island(3, 0));
        assert!(!is_null_island(0, -4));
        let mut builder = ChunkDataBuilder::default();
        builder.bake_heights(None);
        builder.bake_null_island(0, 0);
        let terrain = builder.build();
        assert_eq!((terrain.surface_height(0, 0), terrain.ground_height(0, 0)), (-1, -1));
    }
}
