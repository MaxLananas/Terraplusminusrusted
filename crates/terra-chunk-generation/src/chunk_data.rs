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

/// Surface block requested by a data baker (OSM features); `None` defers to biome rules.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SurfaceBlock(pub String);

#[derive(Debug, Clone)]
pub struct ChunkDataBuilder {
    surface_height: [i32; COLUMNS],
    water_depth: [i8; COLUMNS],
    surface_blocks: Vec<Option<SurfaceBlock>>,
    trees_cleared: [bool; COLUMNS],
}

impl Default for ChunkDataBuilder {
    fn default() -> Self {
        Self {
            surface_height: [BLANK_HEIGHT; COLUMNS],
            water_depth: [WATER_DEPTH_DEFAULT; COLUMNS],
            surface_blocks: vec![None; COLUMNS],
            trees_cleared: [false; COLUMNS],
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
                        ground_height[i] -= depth + WATER_DEPTH_OFFSET;
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
        self.surface_height(x, z) - 1
    }

    pub fn surface_block(&self, x: usize, z: usize) -> Option<&SurfaceBlock> {
        self.surface_blocks[column(x, z)].as_ref()
    }

    /// False where an OpenStreetMap feature drew `no_trees` (roads, buildings, rivers).
    pub fn trees_allowed(&self, x: usize, z: usize) -> bool {
        !self.trees_cleared[column(x, z)]
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
