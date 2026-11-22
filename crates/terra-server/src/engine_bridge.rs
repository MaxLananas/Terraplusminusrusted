//! Engine-independent pieces of the game adapter: the generation gate, chunk writing through a
//! block palette, cached quart-cell biome lookups and the wait for real chunk data.
//!
//! The engine's terrain callback has no failure channel, so a generation worker waits until the
//! chunk's real elevation data is available rather than writing placeholder terrain.

use crate::stack::TerraStack;
use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use terra_chunk_generation::{Biome, Block, ChunkTerrain, ColumnSink, FingerprintError, FingerprintStatus, LoaderError, WorldHeight};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateState {
    /// The world's generation fingerprint has not been checked yet.
    AwaitingFingerprint,
    Open,
    /// The world was generated with different settings; new chunks are never generated.
    Refused(String),
}

pub struct GenerationGate {
    state: Mutex<GateState>,
    changed: Condvar,
}

impl Default for GenerationGate {
    fn default() -> Self {
        Self { state: Mutex::new(GateState::AwaitingFingerprint), changed: Condvar::new() }
    }
}

impl GenerationGate {
    pub fn state(&self) -> GateState {
        self.state.lock().map_or_else(|_| GateState::Refused("gate lock poisoned".into()), |state| state.clone())
    }

    pub fn apply_fingerprint(&self, status: &Result<FingerprintStatus, FingerprintError>) -> GateState {
        let next = match status {
            Ok(_) => GateState::Open,
            Err(error) => GateState::Refused(format!("{error}; new chunks will not be generated until the original settings are restored")),
        };
        self.set(next.clone());
        next
    }

    pub fn set(&self, next: GateState) {
        if let Ok(mut state) = self.state.lock() {
            *state = next;
            self.changed.notify_all();
        }
    }

    /// Blocks until the gate opens. Returns the refusal reason each `report_every` while it is
    /// refused or still waiting, so the caller can log it, and keeps waiting.
    pub fn wait_open(&self, report_every: Duration, report: &mut dyn FnMut(&GateState)) {
        let Ok(mut state) = self.state.lock() else { return };
        while *state != GateState::Open {
            report(&state);
            match self.changed.wait_timeout(state, report_every) {
                Ok((next, _)) => state = next,
                Err(_) => return,
            }
        }
    }
}

/// Generation statistics shown by the diagnostics command.
#[derive(Default)]
pub struct GenerationCounters {
    pub chunks_written: AtomicU64,
    pub data_retries: AtomicU64,
    pub direct_generations: AtomicU64,
}

impl GenerationCounters {
    pub fn summary(&self) -> String {
        format!(
            "chunks written: {}, chunk-data retries: {}, generated without the loader: {}",
            self.chunks_written.load(Ordering::Relaxed),
            self.data_retries.load(Ordering::Relaxed),
            self.direct_generations.load(Ordering::Relaxed)
        )
    }
}

/// The real data for a chunk being generated. Retries with backoff for as long as it takes;
/// a stopped loader falls back to generating on the calling thread.
pub fn terrain_for_generation(stack: &TerraStack, counters: &GenerationCounters, chunk_x: i32, chunk_z: i32, log: &mut dyn FnMut(String)) -> Arc<ChunkTerrain> {
    let mut backoff = Duration::from_millis(250);
    loop {
        let attempt = match stack.loader().load_blocking(chunk_x, chunk_z, stack.config().generation_wait) {
            Err(LoaderError::ShutDown) => {
                counters.direct_generations.fetch_add(1, Ordering::Relaxed);
                stack.generator().chunk_terrain(chunk_x, chunk_z).map(Arc::new).map_err(|error| error.to_string())
            }
            other => other.map_err(|error| error.to_string()),
        };
        match attempt {
            Ok(terrain) => return terrain,
            Err(reason) => {
                let retries = counters.data_retries.fetch_add(1, Ordering::Relaxed) + 1;
                log(format!("chunk ({chunk_x}, {chunk_z}) waits for map data: {reason} (retry {retries})"));
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        }
    }
}

/// Engine block states for generator blocks, resolved once per palette.
pub struct BlockPalette<S> {
    resolved: Vec<(Block, S)>,
    resolve: Box<dyn Fn(&str) -> S + Send + Sync>,
}

impl<S: Copy> BlockPalette<S> {
    pub fn new(resolve: impl Fn(&str) -> S + Send + Sync + 'static) -> Self {
        Self { resolved: Vec::new(), resolve: Box::new(resolve) }
    }

    pub fn state(&mut self, block: &Block) -> S {
        if let Some((_, state)) = self.resolved.iter().find(|(known, _)| known == block) {
            return *state;
        }
        let state = (self.resolve)(block.resource_name());
        self.resolved.push((block.clone(), state));
        state
    }
}

pub trait BlockWriter<S> {
    fn set_block(&mut self, position: [i32; 3], state: S);
}

struct PaletteSink<'a, S, W> {
    palette: &'a mut BlockPalette<S>,
    writer: &'a mut W,
    base_x: i32,
    base_z: i32,
}

impl<S: Copy, W: BlockWriter<S>> ColumnSink for PaletteSink<'_, S, W> {
    fn fill(&mut self, x: usize, z: usize, from_y: i32, to_y: i32, block: &Block) {
        let state = self.palette.state(block);
        let (world_x, world_z) = (self.base_x + x as i32, self.base_z + z as i32);
        for y in from_y..=to_y {
            self.writer.set_block([world_x, y, world_z], state);
        }
    }
}

pub fn write_chunk<S: Copy, W: BlockWriter<S>>(stack: &TerraStack, terrain: &ChunkTerrain, chunk_x: i32, chunk_z: i32, world: WorldHeight, palette: &mut BlockPalette<S>, writer: &mut W) {
    let mut sink = PaletteSink { palette, writer, base_x: chunk_x.wrapping_shl(4), base_z: chunk_z.wrapping_shl(4) };
    stack.generator().fill_chunk(terrain, chunk_x, chunk_z, world, &mut sink);
}

/// The chunk an engine asks for, with what tree placement needs to know about the world.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeTarget {
    pub chunk_x: i32,
    pub chunk_z: i32,
    pub world: WorldHeight,
    /// The world seed, which Paper feeds to every Bukkit `BlockPopulator`.
    pub level_seed: i64,
}

/// `TreePopulator` for a chunk whose terrain is already written: the trees of the chunk and the
/// parts of its neighbours' trees that reach into it, overwriting terrain as upstream's
/// `LimitedRegion.setType` does. Neighbour data comes from the loader, waiting like the chunk's
/// own data. Returns the number of blocks written; nothing when trees are disabled.
pub fn write_trees<S: Copy, W: BlockWriter<S>>(
    stack: &TerraStack,
    counters: &GenerationCounters,
    terrain: &Arc<ChunkTerrain>,
    target: TreeTarget,
    palette: &mut BlockPalette<S>,
    writer: &mut W,
    log: &mut dyn FnMut(String),
) -> usize {
    if stack.generator().trees().is_none() {
        return 0;
    }
    let TreeTarget { chunk_x, chunk_z, world, level_seed } = target;
    let mut neighbour = |x: i32, z: i32| -> Option<Arc<ChunkTerrain>> {
        if (x, z) == (chunk_x, chunk_z) { Some(Arc::clone(terrain)) } else { Some(terrain_for_generation(stack, counters, x, z, log)) }
    };
    let blocks = stack.generator().tree_blocks(chunk_x, chunk_z, world, level_seed, &mut neighbour);
    let (base_x, base_z) = (chunk_x.wrapping_shl(4), chunk_z.wrapping_shl(4));
    for (x, y, z, block) in &blocks {
        let state = palette.state(block);
        writer.set_block([base_x + *x as i32, *y, base_z + *z as i32], state);
    }
    blocks.len()
}

const BIOME_CACHE_SLOTS: usize = 1024;

type CachedBiome = Option<(u64, (i32, i32), u16)>;

thread_local! {
    static BIOME_CACHE: RefCell<Vec<CachedBiome>> = RefCell::new(vec![None; BIOME_CACHE_SLOTS]);
}

static NEXT_LOOKUP_ID: AtomicU64 = AtomicU64::new(1);

/// `noise_biome` for the engine: the biome index of a quart cell, independent of `y`. Each
/// worker thread keeps a direct-mapped cache, as one chunk asks for every cell height.
pub struct BiomeLookup {
    id: u64,
}

impl Default for BiomeLookup {
    fn default() -> Self {
        Self { id: NEXT_LOOKUP_ID.fetch_add(1, Ordering::Relaxed) }
    }
}

impl BiomeLookup {
    pub fn possible_biomes() -> Vec<String> {
        Biome::ALL.iter().map(|biome| biome.resource_name().to_owned()).collect()
    }

    pub fn index_of(biome: Biome) -> u16 {
        Biome::ALL.iter().position(|candidate| *candidate == biome).unwrap_or(0) as u16
    }

    pub fn biome_index(&self, stack: &TerraStack, quart_x: i32, quart_z: i32) -> u16 {
        let slot = ((quart_x as u32).wrapping_mul(0x9E37_79B9) ^ (quart_z as u32).wrapping_mul(0x85EB_CA6B)) as usize % BIOME_CACHE_SLOTS;
        let cached = BIOME_CACHE.with(|cache| cache.borrow()[slot]);
        if let Some((id, cell, index)) = cached
            && id == self.id
            && cell == (quart_x, quart_z)
        {
            return index;
        }
        let index = Self::index_of(stack.generator().biome_at_block(quart_x.wrapping_shl(2), quart_z.wrapping_shl(2)));
        BIOME_CACHE.with(|cache| cache.borrow_mut()[slot] = Some((self.id, (quart_x, quart_z), index)));
        index
    }
}

/// Splits a block state string (`minecraft:oak_slab[type=top,waterlogged=false]`) into the block
/// id and its properties, for engines that resolve the two separately. `None` when malformed.
pub fn split_block_state(state: &str) -> Option<(&str, Vec<(&str, &str)>)> {
    let Some((id, rest)) = state.split_once('[') else {
        return (!state.is_empty()).then_some((state, Vec::new()));
    };
    let properties = rest.strip_suffix(']')?;
    if id.is_empty() {
        return None;
    }
    if properties.is_empty() {
        return Some((id, Vec::new()));
    }
    let pairs = properties.split(',').map(|pair| pair.split_once('=').filter(|(key, value)| !key.is_empty() && !value.is_empty())).collect::<Option<Vec<_>>>()?;
    Some((id, pairs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_states_split_into_id_and_properties() {
        assert_eq!(split_block_state("minecraft:bricks"), Some(("minecraft:bricks", vec![])));
        assert_eq!(split_block_state("minecraft:oak_slab[type=top,waterlogged=false]"), Some(("minecraft:oak_slab", vec![("type", "top"), ("waterlogged", "false")])));
        for malformed in ["", "[a=b]", "minecraft:x[a=b", "minecraft:x[a]", "minecraft:x[=b]"] {
            assert_eq!(split_block_state(malformed), None, "{malformed}");
        }
    }

    #[test]
    fn gate_follows_fingerprint_outcomes() {
        let gate = GenerationGate::default();
        assert_eq!(gate.state(), GateState::AwaitingFingerprint);
        assert_eq!(gate.apply_fingerprint(&Ok(FingerprintStatus::Created)), GateState::Open);
        let mismatch = FingerprintError::Mismatch(vec![("y_offset".into(), "0".into(), "5".into())]);
        let GateState::Refused(reason) = gate.apply_fingerprint(&Err(mismatch)) else { panic!() };
        assert!(reason.contains("y_offset"), "{reason}");
    }

    #[test]
    fn waiting_generation_resumes_when_the_gate_opens() {
        let gate = Arc::new(GenerationGate::default());
        let waiter = {
            let gate = Arc::clone(&gate);
            std::thread::spawn(move || {
                let mut reports = 0;
                gate.wait_open(Duration::from_millis(5), &mut |_| reports += 1);
                reports
            })
        };
        std::thread::sleep(Duration::from_millis(30));
        gate.apply_fingerprint(&Ok(FingerprintStatus::Matched));
        assert!(waiter.join().unwrap() >= 1);
    }

    #[test]
    fn palette_resolves_each_block_once() {
        let calls = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&calls);
        let mut palette = BlockPalette::new(move |name: &str| {
            counter.fetch_add(1, Ordering::Relaxed);
            name.len()
        });
        assert_eq!(palette.state(&Block::Stone), "minecraft:stone".len());
        palette.state(&Block::Stone);
        palette.state(&Block::Named("minecraft:moss_block".into()));
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }
}
