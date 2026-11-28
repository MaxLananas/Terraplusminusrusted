//! TerraPlusMinus `TreePopulator`: custom tree schematics from `customTrees.json`, placed by the
//! baked tree cover and chosen by Köppen climate class.
//!
//! Upstream runs as a Bukkit `BlockPopulator` that may write into neighbouring chunks. Horizon
//! builds one chunk at a time, so [`TreePopulator::blocks_in_chunk`] replays the populator of
//! every chunk whose trees can reach the chunk being built and keeps the blocks that fall inside
//! it. The result depends only on the world seed, the configuration and the chunk position.
//!
//! Where upstream is not reproducible this port picks a deterministic equivalent:
//! - the thread-local `RNG_CACHE` buffer, whose initial content carries over from whatever chunk
//!   the thread populated before, starts zeroed for every chunk;
//! - the tree variant and rotation, drawn from `new Random()`, come from a generator seeded by
//!   the world seed and chunk position;
//! - the climate class, which upstream reads from the last biome any thread computed
//!   (`CustomBiomeProvider.getBiome()`), is the class at the tree's own column.
//!
//! Tree positions themselves follow Paper's populator random (`setDecorationSeed` with the world
//! seed and chunk coordinates, then `nextBytes` and `nextInt(15)`) exactly.

use crate::chunk_data::ChunkTerrain;
use crate::generator::{Block, WorldHeight};
use crate::java_random::LegacyRandom;
use std::fmt;
use terra_codec::{JsonValue, parse_relaxed_json};

/// The upstream schematics, byte-identical.
pub const UPSTREAM_TREES: &str = include_str!("../resources/customTrees.json");

/// Seeds the stand-in for upstream's unseeded `new Random()` (tree variant and rotation).
const CHOICE_SALT: i32 = 0x7472_6565;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeError(pub String);

impl fmt::Display for TreeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid tree schematics: {}", self.0)
    }
}

impl std::error::Error for TreeError {}

/// One block of a schematic, relative to the block above the ground.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeBlock {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub block: Block,
}

/// `customTrees.json`: families (`oak`, `spruce`, ...) of variants, in file order.
#[derive(Debug, Clone)]
pub struct TreeLibrary {
    families: Vec<(String, Vec<Vec<TreeBlock>>)>,
    reach: i32,
    height: i32,
}

/// The block a Bukkit `Material` name places through `LimitedRegion.setType`: its default state,
/// except that leaves are persistent. Upstream's default leaves (`distance=7`,
/// `persistent=false`) have no log within reach in most schematics (trunks are fences) and would
/// decay on random ticks unless the server disables leaf decay; persistent leaves look the same.
pub fn material_block(material: &str) -> Block {
    let id = format!("minecraft:{}", material.to_ascii_lowercase());
    if id.ends_with("_leaves") { Block::Named(format!("{id}[persistent=true]")) } else { Block::from_material_name(&id) }
}

impl TreeLibrary {
    pub fn upstream() -> Result<Self, TreeError> {
        Self::parse(UPSTREAM_TREES)
    }

    /// `TreePopulator` constructor: `trees.<family>.<size>.<variant>.blocks[]`, sizes flattened.
    pub fn parse(text: &str) -> Result<Self, TreeError> {
        let document = parse_relaxed_json(text).map_err(|error| TreeError(error.to_string()))?;
        let families_json = document.get("trees").and_then(JsonValue::as_object).ok_or_else(|| TreeError("missing `trees` object".into()))?;
        let mut families = Vec::new();
        let (mut reach, mut height) = (0, 0);
        for (family, sizes) in families_json {
            let sizes = sizes.as_object().ok_or_else(|| TreeError(format!("family `{family}` is not an object")))?;
            let mut variants = Vec::new();
            for (size, trees) in sizes {
                let trees = trees.as_object().ok_or_else(|| TreeError(format!("`{family}.{size}` is not an object")))?;
                for (name, tree) in trees {
                    let context = |message: &str| TreeError(format!("`{family}.{size}.{name}`: {message}"));
                    let blocks = match tree.get("blocks") {
                        Some(JsonValue::Array(blocks)) => blocks,
                        _ => return Err(context("missing `blocks` array")),
                    };
                    let mut parsed = Vec::with_capacity(blocks.len());
                    for block in blocks {
                        let coordinate = |key: &str| -> Result<i32, TreeError> {
                            let value = block.get(key).and_then(JsonValue::as_f64).ok_or_else(|| context(&format!("block without integer `{key}`")))?;
                            if value.fract() != 0.0 || value.abs() > 64.0 {
                                return Err(context(&format!("`{key}` {value} is not a small integer")));
                            }
                            Ok(value as i32)
                        };
                        let material = block.get("material").and_then(JsonValue::as_str).ok_or_else(|| context("block without `material`"))?;
                        if material.is_empty() || !material.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') {
                            return Err(context(&format!("`{material}` is not a material name")));
                        }
                        let (x, y, z) = (coordinate("x")?, coordinate("y")?, coordinate("z")?);
                        reach = reach.max(x.abs()).max(z.abs());
                        height = height.max(y + 1);
                        parsed.push(TreeBlock { x, y, z, block: material_block(material) });
                    }
                    variants.push(parsed);
                }
            }
            families.push((family.clone(), variants));
        }
        Ok(Self { families, reach, height })
    }

    pub fn family(&self, name: &str) -> &[Vec<TreeBlock>] {
        self.families.iter().find(|(family, _)| family == name).map_or(&[], |(_, variants)| variants.as_slice())
    }

    pub fn families(&self) -> impl Iterator<Item = (&str, usize)> {
        self.families.iter().map(|(name, variants)| (name.as_str(), variants.len()))
    }

    /// The largest horizontal distance of a block from its tree's origin, in any rotation.
    pub fn reach(&self) -> i32 {
        self.reach
    }

    /// The tallest schematic, in blocks above the origin.
    pub fn height(&self) -> i32 {
        self.height
    }

    /// Every distinct block the schematics place.
    pub fn blocks(&self) -> Vec<Block> {
        let mut blocks: Vec<Block> = Vec::new();
        for block in self.families.iter().flat_map(|(_, variants)| variants.iter().flatten()).map(|tree| &tree.block) {
            if !blocks.contains(block) {
                blocks.push(block.clone());
            }
        }
        blocks
    }
}

/// The tree families for a Köppen class, as `TreePopulator.populate`'s switch.
pub fn families_for_climate(class: u8) -> &'static [&'static str] {
    match class {
        4 | 6 | 17 => &["savanna"],
        14 | 15 => &["oak", "birch"],
        27 => &["spruce"],
        28..=30 => &[],
        _ => &["oak", "birch"],
    }
}

/// A tree chosen by the populator: world position of its origin, variant and quarter turns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedTree {
    pub origin: [i32; 3],
    pub family: &'static str,
    pub variant: usize,
    pub quarter_turns: u8,
}

/// `TreePopulator` with its settings: the schematics and the world's Y offset.
#[derive(Debug, Clone)]
pub struct TreePopulator {
    library: std::sync::Arc<TreeLibrary>,
    y_offset: i32,
}

impl TreePopulator {
    pub fn new(library: TreeLibrary, y_offset: i32) -> Self {
        Self { library: std::sync::Arc::new(library), y_offset }
    }

    /// The same populator for a level with another Y offset (linked levels share one library).
    pub fn with_y_offset(&self, y_offset: i32) -> Self {
        Self { library: std::sync::Arc::clone(&self.library), y_offset }
    }

    pub fn library(&self) -> &TreeLibrary {
        &self.library
    }

    /// `populate` for one chunk, from its baked data. `climate_class` gives the Köppen class at a
    /// block column (8 when the biome dataset is disabled, as upstream's default).
    pub fn populate(&self, terrain: &ChunkTerrain, chunk_x: i32, chunk_z: i32, world: WorldHeight, level_seed: i64, climate_class: &dyn Fn(i32, i32) -> u8) -> Vec<PlacedTree> {
        let mut trees = Vec::new();
        // Upstream compares every cell against the first column's chance (`treeCover[(0)]`).
        let chance = terrain.tree_chance(0, 0);
        let mut random = LegacyRandom::decoration(level_seed, chunk_x, chunk_z);
        let mut choice = LegacyRandom::large_feature_with_salt(level_seed, chunk_x, chunk_z, CHOICE_SALT);
        let mut rng = [0u8; 256];
        for cell in 0..64 {
            if rng[cell] >= chance {
                continue;
            }
            random.next_bytes(&mut rng);
            let local_x = random.next_int_bounded(15) + 1;
            let local_z = random.next_int_bounded(15) + 1;
            let (x, z) = (local_x as usize, local_z as usize);
            let ground = terrain.ground_height(x, z);
            if ground < terrain.water_height(x, z) {
                // Upstream returns from `populate`: no further trees in this chunk.
                return trees;
            }
            let base_y = ground.wrapping_add(self.y_offset);
            if base_y >= world.max_y - 35 || base_y <= world.min_y || terrain.surface_block(x, z).is_some() {
                continue;
            }
            let block_x = chunk_x.wrapping_shl(4).wrapping_add(local_x);
            let block_z = chunk_z.wrapping_shl(4).wrapping_add(local_z);
            let families = families_for_climate(climate_class(block_x, block_z));
            let count: usize = families.iter().map(|family| self.library.family(family).len()).sum();
            if count == 0 {
                continue;
            }
            let mut index = choice.next_int_bounded(count as i32) as usize;
            let quarter_turns = choice.next_int_bounded(4) as u8;
            for &family in families {
                let variants = self.library.family(family).len();
                if index < variants {
                    trees.push(PlacedTree { origin: [block_x, base_y + 1, block_z], family, variant: index, quarter_turns });
                    break;
                }
                index -= variants;
            }
        }
        trees
    }

    /// The blocks of `tree` in world coordinates, in schematic order, rotated as
    /// `generateCustomTree` rotates them.
    pub fn tree_blocks<'a>(&'a self, tree: &'a PlacedTree) -> impl Iterator<Item = ([i32; 3], &'a Block)> + 'a {
        let [origin_x, origin_y, origin_z] = tree.origin;
        self.library.family(tree.family).get(tree.variant).into_iter().flatten().map(move |block| {
            let (x, z) = match tree.quarter_turns {
                1 => (-block.z, block.x),
                2 => (-block.x, -block.z),
                3 => (block.z, -block.x),
                _ => (block.x, block.z),
            };
            ([origin_x + x, origin_y + block.y, origin_z + z], &block.block)
        })
    }

    /// How many chunks away a populated chunk can still place blocks.
    pub fn chunk_radius(&self) -> i32 {
        // Origins lie 1..=15 blocks into their chunk.
        (self.library.reach() + 15) / 16
    }

    /// Every tree block landing in the chunk, from the populators of it and its neighbours, as
    /// chunk-local `(x, y, z, block)`. Later trees overwrite earlier ones; neighbours are visited
    /// in a fixed order. `neighbour` supplies baked data for any chunk in range.
    pub fn blocks_in_chunk(
        &self,
        chunk_x: i32,
        chunk_z: i32,
        world: WorldHeight,
        level_seed: i64,
        neighbour: &mut dyn FnMut(i32, i32) -> Option<std::sync::Arc<ChunkTerrain>>,
        climate_class: &dyn Fn(i32, i32) -> u8,
    ) -> Vec<(usize, i32, usize, Block)> {
        let radius = self.chunk_radius();
        let (base_x, base_z) = (chunk_x.wrapping_shl(4), chunk_z.wrapping_shl(4));
        let mut out = Vec::new();
        for source_x in chunk_x.saturating_sub(radius)..=chunk_x.saturating_add(radius) {
            for source_z in chunk_z.saturating_sub(radius)..=chunk_z.saturating_add(radius) {
                let Some(terrain) = neighbour(source_x, source_z) else { continue };
                for tree in self.populate(&terrain, source_x, source_z, world, level_seed, climate_class) {
                    for ([x, y, z], block) in self.tree_blocks(&tree) {
                        let (local_x, local_z) = (x.wrapping_sub(base_x), z.wrapping_sub(base_z));
                        if (0..16).contains(&local_x) && (0..16).contains(&local_z) && (world.min_y..world.max_y).contains(&y) {
                            out.push((local_x as usize, y, local_z as usize, block.clone()));
                        }
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk_data::ChunkDataBuilder;
    use std::sync::Arc;

    const WORLD: WorldHeight = WorldHeight { min_y: -64, max_y: 320 };

    fn populator() -> TreePopulator {
        TreePopulator::new(TreeLibrary::upstream().unwrap(), 0)
    }

    fn terrain(height: f64, cover: f64) -> ChunkTerrain {
        let mut builder = ChunkDataBuilder::default();
        builder.bake_tree_cover(Some(&[cover; 256]));
        builder.bake_heights(Some(&[height; 256]));
        builder.build()
    }

    #[test]
    fn upstream_schematics_load_in_file_order() {
        let library = TreeLibrary::upstream().unwrap();
        let families: Vec<(&str, usize)> = library.families().collect();
        assert_eq!(families, vec![("birch", 3), ("snow", 0), ("savanna", 2), ("spruce", 6), ("oak", 5)]);
        assert_eq!((library.reach(), library.height()), (5, 31));
        assert_eq!(library.family("birch")[0][1], TreeBlock { x: 0, y: 2, z: 0, block: Block::Named("minecraft:birch_fence".into()) });
        assert!(library.blocks().contains(&Block::Named("minecraft:oak_leaves[persistent=true]".into())));
        assert_eq!(library.blocks().len(), 18);
        assert!(TreeLibrary::parse(r#"{"trees": {"oak": {"s": {"t": {"blocks": [{"x": 0.5, "y": 0, "z": 0, "material": "OAK_LOG"}]}}}}}"#).is_err());
        assert!(TreeLibrary::parse(r#"{"trees": {"oak": {"s": {"t": {"blocks": [{"x": 0, "y": 0, "z": 0, "material": "oak log]"}]}}}}}"#).is_err());
    }

    #[test]
    fn climate_classes_choose_families_as_upstream() {
        assert_eq!(families_for_climate(4), ["savanna"]);
        assert_eq!(families_for_climate(17), ["savanna"]);
        assert_eq!(families_for_climate(27), ["spruce"]);
        assert!(families_for_climate(29).is_empty());
        assert_eq!(families_for_climate(8), ["oak", "birch"]);
        assert_eq!(families_for_climate(5), ["oak", "birch"]);
    }

    #[test]
    fn positions_follow_the_populator_random() {
        let populator = populator();
        let trees = populator.populate(&terrain(70.0, 1.0), 3, -7, WORLD, 42, &|_, _| 8);
        // Full cover: the first cell always draws (the buffer starts zeroed), and every draw
        // consumes nextBytes(256) then nextInt(15) twice.
        let mut random = LegacyRandom::decoration(42, 3, -7);
        let mut rng = [0u8; 256];
        random.next_bytes(&mut rng);
        let first = (random.next_int_bounded(15) + 1, random.next_int_bounded(15) + 1);
        assert_eq!((trees[0].origin[0], trees[0].origin[2]), (3 * 16 + first.0, -7 * 16 + first.1));
        assert_eq!(trees[0].origin[1], 71);
        assert!(trees.len() > 10 && trees.len() <= 64, "{}", trees.len());
        assert!(trees.iter().all(|tree| tree.family == "oak" || tree.family == "birch"));
        assert_eq!(trees, populator.populate(&terrain(70.0, 1.0), 3, -7, WORLD, 42, &|_, _| 8));
        assert_ne!(trees, populator.populate(&terrain(70.0, 1.0), 3, -7, WORLD, 43, &|_, _| 8));
    }

    #[test]
    fn cover_water_surface_blocks_height_and_climate_stop_trees() {
        let populator = populator();
        assert!(populator.populate(&terrain(70.0, 0.0), 0, 5, WORLD, 1, &|_, _| 8).is_empty());
        assert!(populator.populate(&terrain(70.0, 1.0), 0, 5, WORLD, 1, &|_, _| 29).is_empty());
        // 320 - 35 = 285: ground at 285 is too high, 284 is not.
        assert!(populator.populate(&terrain(285.0, 1.0), 0, 5, WORLD, 1, &|_, _| 8).is_empty());
        assert!(!populator.populate(&terrain(284.0, 1.0), 0, 5, WORLD, 1, &|_, _| 8).is_empty());
        assert!(populator.populate(&terrain(-64.0, 1.0), 0, 5, WORLD, 1, &|_, _| 8).is_empty());
        let mut water = ChunkDataBuilder::default();
        water.bake_tree_cover(Some(&[1.0; 256]));
        water.bake_heights(Some(&[10.0; 256]));
        water.update_water_depth(0, 0, 1);
        for x in 0..16 {
            for z in 0..16 {
                water.update_water_depth(x, z, 3);
            }
        }
        assert!(populator.populate(&water.build(), 0, 5, WORLD, 1, &|_, _| 8).is_empty());
        let mut road = ChunkDataBuilder::default();
        road.bake_tree_cover(Some(&[1.0; 256]));
        road.bake_heights(Some(&[10.0; 256]));
        for x in 0..16 {
            for z in 0..16 {
                road.set_surface_block(x, z, Some(crate::chunk_data::SurfaceBlock("minecraft:gray_concrete".into())));
            }
        }
        assert!(populator.populate(&road.build(), 0, 5, WORLD, 1, &|_, _| 8).is_empty());
    }

    #[test]
    fn the_first_drowned_draw_ends_the_chunk() {
        let populator = populator();
        let land = terrain(70.0, 1.0);
        let all = populator.populate(&land, 4, 4, WORLD, 77, &|_, _| 8);
        assert!(all.len() > 2);
        // Flood the column of the second tree: upstream returns there, keeping only the first.
        let second = &all[1];
        let (x, z) = ((second.origin[0] - 64) as usize, (second.origin[2] - 64) as usize);
        let mut builder = ChunkDataBuilder::default();
        builder.bake_tree_cover(Some(&[1.0; 256]));
        builder.bake_heights(Some(&[70.0; 256]));
        builder.update_water_depth(x, z, 3);
        let flooded = builder.build();
        assert!(flooded.ground_height(x, z) < flooded.water_height(x, z));
        assert_eq!(populator.populate(&flooded, 4, 4, WORLD, 77, &|_, _| 8), all[..1]);
    }

    #[test]
    fn only_the_first_column_chance_matters() {
        let populator = populator();
        let mut builder = ChunkDataBuilder::default();
        builder.bake_heights(Some(&[70.0; 256]));
        builder.bake_tree_cover(Some(&[1.0; 256]));
        terra_osm::DrawTarget::clear_tree_cover(&mut builder, 0, 0);
        assert!(populator.populate(&builder.build(), 2, 2, WORLD, 9, &|_, _| 8).is_empty());
    }

    #[test]
    fn rotations_turn_schematics_counterclockwise_in_quarter_steps() {
        let populator = populator();
        let tree = PlacedTree { origin: [100, 64, 200], family: "birch", variant: 0, quarter_turns: 0 };
        let first = |turns: u8| populator.tree_blocks(&PlacedTree { quarter_turns: turns, ..tree.clone() }).next().unwrap().0;
        // Birch block 0 sits at (-1, 5, 0).
        assert_eq!(first(0), [99, 69, 200]);
        assert_eq!(first(1), [100, 69, 199]);
        assert_eq!(first(2), [101, 69, 200]);
        assert_eq!(first(3), [100, 69, 201]);
    }

    #[test]
    fn chunks_see_the_parts_of_neighbouring_trees_that_reach_them() {
        let populator = populator();
        assert_eq!(populator.chunk_radius(), 1);
        let land = Arc::new(terrain(70.0, 1.0));
        let mut all = |_: i32, _: i32| Some(Arc::clone(&land));
        let center = populator.blocks_in_chunk(0, 0, WORLD, 5, &mut all, &|_, _| 8);
        assert!(center.iter().all(|&(x, y, z, _)| x < 16 && z < 16 && y > 70));
        // Without neighbours, only the chunk's own trees remain; with them, overhangs appear.
        let mut alone = |x: i32, z: i32| (x == 0 && z == 0).then(|| Arc::clone(&land));
        let own = populator.blocks_in_chunk(0, 0, WORLD, 5, &mut alone, &|_, _| 8);
        assert!(own.len() < center.len());
        // Every block a chunk receives is a block its source populator placed at that position.
        let mut expected = Vec::new();
        for source_x in -1..=1 {
            for source_z in -1..=1 {
                for tree in populator.populate(&land, source_x, source_z, WORLD, 5, &|_, _| 8) {
                    for ([x, y, z], block) in populator.tree_blocks(&tree) {
                        if (0..16).contains(&x) && (0..16).contains(&z) {
                            expected.push((x as usize, y, z as usize, block.clone()));
                        }
                    }
                }
            }
        }
        assert_eq!(center, expected);
    }
}
