//! Engine-independent pieces of the game adapter: the generation gate, chunk writing through a
//! block palette, cached quart-cell biome lookups and the wait for real chunk data.
//!
//! The engine's terrain callback (`ChunkGenerator::build_terrain`) has no failure channel, so
//! these functions report a terminal [`GenerationFailure`] and leave it to the adapter to fail
//! the chunk without completing it. Nothing here ever substitutes placeholder terrain.

use crate::stack::TerraStack;
use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use terra_chunk_generation::{Biome, Block, ChunkTerrain, ColumnSink, FingerprintError, FingerprintStatus, LoaderError, WorldHeight};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateState {
    /// The world's generation fingerprint has not been checked yet.
    AwaitingFingerprint,
    Open,
    /// The world was generated with different settings; new chunks are never generated.
    Refused(String),
    /// The server is stopping: waiting generation ends and no new generation starts.
    ShuttingDown,
    /// An internal error (a poisoned lock); generation never resumes in this process.
    Failed(String),
}

impl GateState {
    fn is_terminal(&self) -> bool {
        matches!(self, Self::Refused(_) | Self::ShuttingDown | Self::Failed(_))
    }
}

/// Why a chunk could not be generated. Every variant is terminal for that chunk: the adapter
/// must not publish it as generated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GenerationFailure {
    /// The world's fingerprint does not match the configuration.
    Refused(String),
    /// The server is stopping.
    Cancelled,
    /// The data can never load with this configuration (unreachable URL, invalid or malformed
    /// data).
    Permanent(String),
    /// Transient failures outlasted `generation.retry_attempts`.
    RetriesExhausted { attempts: u32, last: String },
    /// An internal error, such as a poisoned lock.
    Internal(String),
}

impl std::fmt::Display for GenerationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(reason) => write!(f, "generation refused: {reason}"),
            Self::Cancelled => f.write_str("generation cancelled: the server is stopping"),
            Self::Permanent(reason) => write!(f, "map data cannot be loaded with this configuration: {reason}"),
            Self::RetriesExhausted { attempts, last } => write!(f, "map data still unavailable after {attempts} attempts: {last}"),
            Self::Internal(reason) => write!(f, "internal error: {reason}"),
        }
    }
}

impl std::error::Error for GenerationFailure {}

pub struct GenerationGate {
    state: Mutex<GateState>,
    changed: Condvar,
}

impl Default for GenerationGate {
    fn default() -> Self {
        Self { state: Mutex::new(GateState::AwaitingFingerprint), changed: Condvar::new() }
    }
}

const POISONED: &str = "generation gate lock poisoned";

impl GenerationGate {
    fn lock(&self) -> Result<MutexGuard<'_, GateState>, GenerationFailure> {
        self.state.lock().map_err(|_| GenerationFailure::Internal(POISONED.into()))
    }

    pub fn state(&self) -> GateState {
        self.state.lock().map_or_else(|_| GateState::Failed(POISONED.into()), |state| state.clone())
    }

    /// Opens the gate for a matching fingerprint, refuses it otherwise. A gate that is already
    /// terminal (refused, shutting down, failed) stays so.
    pub fn apply_fingerprint(&self, status: &Result<FingerprintStatus, FingerprintError>) -> GateState {
        let next = match status {
            Ok(_) => GateState::Open,
            Err(error) => GateState::Refused(format!("{error}; new chunks will not be generated until the original settings are restored")),
        };
        self.set(next)
    }

    /// Moves to `next` unless the gate is terminal; `ShuttingDown` and `Failed` always apply.
    /// Returns the resulting state.
    pub fn set(&self, next: GateState) -> GateState {
        let Ok(mut state) = self.state.lock() else { return GateState::Failed(POISONED.into()) };
        let overriding = matches!(next, GateState::ShuttingDown | GateState::Failed(_));
        if overriding || !state.is_terminal() {
            *state = next;
        }
        self.changed.notify_all();
        state.clone()
    }

    /// Ends every wait at once and refuses new generation.
    pub fn shut_down(&self) {
        self.set(GateState::ShuttingDown);
    }

    /// The failure a terminal gate imposes, if any.
    fn terminal(state: &GateState) -> Option<GenerationFailure> {
        match state {
            GateState::Refused(reason) => Some(GenerationFailure::Refused(reason.clone())),
            GateState::ShuttingDown => Some(GenerationFailure::Cancelled),
            GateState::Failed(reason) => Some(GenerationFailure::Internal(reason.clone())),
            GateState::Open | GateState::AwaitingFingerprint => None,
        }
    }

    /// Waits until the gate opens: `Ok` only for [`GateState::Open`]. A refused, stopping or
    /// failed gate (including a poisoned lock) returns its failure at once, and a check still
    /// pending after `max_wait` is an internal failure. `report` runs every `report_every`
    /// while the fingerprint check is pending, outside the lock.
    pub fn wait_open(&self, report_every: Duration, max_wait: Duration, report: &mut dyn FnMut(&GateState)) -> Result<(), GenerationFailure> {
        let deadline = Instant::now() + max_wait;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let pending = {
                let state = self.lock()?;
                if *state == GateState::Open {
                    return Ok(());
                }
                if let Some(failure) = Self::terminal(&state) {
                    return Err(failure);
                }
                if left.is_zero() {
                    return Err(GenerationFailure::Internal(format!("the generation fingerprint was not checked within {} s", max_wait.as_secs())));
                }
                let (state, timeout) = self.changed.wait_timeout(state, report_every.min(left)).map_err(|_| GenerationFailure::Internal(POISONED.into()))?;
                if *state == GateState::Open {
                    return Ok(());
                }
                if let Some(failure) = Self::terminal(&state) {
                    return Err(failure);
                }
                timeout.timed_out().then(|| state.clone())
            };
            if let Some(state) = pending {
                report(&state);
            }
        }
    }

    /// Sleeps for `duration` unless the gate turns terminal first, which it returns.
    pub fn sleep(&self, duration: Duration) -> Result<(), GenerationFailure> {
        let deadline = Instant::now() + duration;
        let mut state = self.lock()?;
        loop {
            if let Some(failure) = Self::terminal(&state) {
                return Err(failure);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(());
            }
            state = self.changed.wait_timeout(state, remaining).map_err(|_| GenerationFailure::Internal(POISONED.into()))?.0;
        }
    }

    /// The gate's failure if it is terminal now.
    pub fn check(&self) -> Result<(), GenerationFailure> {
        match Self::terminal(&*self.lock()?) {
            Some(failure) => Err(failure),
            None => Ok(()),
        }
    }
}

/// Generation statistics shown by the diagnostics command.
#[derive(Default)]
pub struct GenerationCounters {
    pub chunks_written: AtomicU64,
    pub data_retries: AtomicU64,
    pub chunks_failed: AtomicU64,
}

impl GenerationCounters {
    pub fn summary(&self) -> String {
        format!(
            "chunks written: {}, chunk-data retries: {}, chunks failed (not generated): {}",
            self.chunks_written.load(Ordering::Relaxed),
            self.data_retries.load(Ordering::Relaxed),
            self.chunks_failed.load(Ordering::Relaxed)
        )
    }
}

/// What generation workers share: the gate and the statistics.
#[derive(Default)]
pub struct GenerationControl {
    pub gate: GenerationGate,
    pub counters: GenerationCounters,
}

/// The real data for a chunk being generated. Transient failures are retried with backoff
/// (250 ms doubling to 30 s) up to `generation.retry_attempts` (0: until the server stops);
/// permanent failures, a stopping server and a terminal gate end the wait at once. A shut-down
/// loader is a cancellation, never a reason to generate some other way.
pub fn terrain_for_generation(
    stack: &TerraStack,
    control: &GenerationControl,
    chunk_x: i32,
    chunk_z: i32,
    log: &mut dyn FnMut(String),
) -> Result<Arc<ChunkTerrain>, GenerationFailure> {
    let GenerationControl { gate, counters } = control;
    let limit = stack.config().generation_retry_attempts;
    let mut backoff = Duration::from_millis(250);
    for attempt in 1u32.. {
        gate.check()?;
        let error = match stack.loader().load_blocking(chunk_x, chunk_z, stack.config().generation_wait) {
            Ok(terrain) => return Ok(terrain),
            Err(LoaderError::ShutDown) => return Err(gate.check().err().unwrap_or(GenerationFailure::Cancelled)),
            Err(error) => error,
        };
        if error.is_permanent() {
            return Err(GenerationFailure::Permanent(error.to_string()));
        }
        if limit != 0 && attempt >= limit {
            return Err(GenerationFailure::RetriesExhausted { attempts: attempt, last: error.to_string() });
        }
        let retries = counters.data_retries.fetch_add(1, Ordering::Relaxed) + 1;
        log(format!("chunk ({chunk_x}, {chunk_z}) waits for map data: {error} (attempt {attempt}, retry {retries} overall)"));
        gate.sleep(backoff)?;
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
    Err(GenerationFailure::Internal("retry counter overflow".into()))
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
/// own data. Returns the number of blocks written (0 when trees are disabled), or the failure of a
/// neighbour's data, in which case nothing has been written.
pub fn write_trees<S: Copy, W: BlockWriter<S>>(
    stack: &TerraStack,
    control: &GenerationControl,
    terrain: &Arc<ChunkTerrain>,
    target: TreeTarget,
    palette: &mut BlockPalette<S>,
    writer: &mut W,
    log: &mut dyn FnMut(String),
) -> Result<usize, GenerationFailure> {
    if stack.generator().trees().is_none() {
        return Ok(0);
    }
    let TreeTarget { chunk_x, chunk_z, world, level_seed } = target;
    // Neighbour data is needed in full: a tree must not be cut off because a neighbour failed.
    let mut failure = None;
    let mut neighbour = |x: i32, z: i32| -> Option<Arc<ChunkTerrain>> {
        if (x, z) == (chunk_x, chunk_z) {
            return Some(Arc::clone(terrain));
        }
        if failure.is_some() {
            return None;
        }
        match terrain_for_generation(stack, control, x, z, log) {
            Ok(terrain) => Some(terrain),
            Err(error) => {
                failure = Some(error);
                None
            }
        }
    };
    let blocks = stack.generator().tree_blocks(chunk_x, chunk_z, world, level_seed, &mut neighbour);
    if let Some(failure) = failure {
        return Err(failure);
    }
    let (base_x, base_z) = (chunk_x.wrapping_shl(4), chunk_z.wrapping_shl(4));
    for (x, y, z, block) in &blocks {
        let state = palette.state(block);
        writer.set_block([base_x + *x as i32, *y, base_z + *z as i32], state);
    }
    Ok(blocks.len())
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

/// Checks a block state string strictly: `namespace:path` (or a bare path, namespaced as
/// `minecraft:`), lower-case resource characters, and optional `[key=value,...]` properties
/// with distinct keys. Whether the block and its properties exist in the game is checked when
/// the engine resolves the state; nothing is ever substituted.
pub fn validate_block_state(state: &str) -> Result<(), String> {
    let malformed = |why: &str| Err(format!("malformed block state `{state}`: {why}"));
    let Some((id, properties)) = split_block_state(state) else { return malformed("expected `namespace:block[key=value,...]`") };
    let (namespace, path) = id.split_once(':').unwrap_or(("minecraft", id));
    let resource = |text: &str, extra: &str| !text.is_empty() && text.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_-.".contains(c) || extra.contains(c));
    if !resource(namespace, "") || !resource(path, "/") {
        return malformed("block ids use lower-case letters, digits and `_-./`");
    }
    let mut keys = std::collections::HashSet::new();
    for (key, value) in properties {
        if !resource(key, "") || !resource(value, "") {
            return malformed("property names and values use lower-case letters, digits and `_-.`");
        }
        if !keys.insert(key) {
            return malformed(&format!("property `{key}` repeated"));
        }
    }
    Ok(())
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
    fn block_states_are_validated_strictly() {
        for valid in ["minecraft:stone", "stone", "minecraft:oak_leaves[persistent=true]", "mymod:path/to.block[a=1,b=x_y]"] {
            assert_eq!(validate_block_state(valid), Ok(()), "{valid}");
        }
        for invalid in ["", "Minecraft:Stone", "minecraft:stone[", "minecraft:x[a=b,a=c]", "minecraft:x[A=b]", "minecraft:x y", ":stone", "minecraft:", "a:b:c"] {
            assert!(validate_block_state(invalid).is_err(), "{invalid}");
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
                let result = gate.wait_open(Duration::from_millis(5), Duration::from_secs(60), &mut |_| reports += 1);
                (result, reports)
            })
        };
        std::thread::sleep(Duration::from_millis(30));
        gate.apply_fingerprint(&Ok(FingerprintStatus::Matched));
        let (result, reports) = waiter.join().unwrap();
        assert_eq!(result, Ok(()));
        assert!(reports >= 1);
    }

    /// Spawns a waiter and returns how long it waited and what it got.
    fn wait_in_thread(gate: &Arc<GenerationGate>) -> std::thread::JoinHandle<(Result<(), GenerationFailure>, Duration)> {
        let gate = Arc::clone(gate);
        std::thread::spawn(move || {
            let started = Instant::now();
            let result = gate.wait_open(Duration::from_secs(60), Duration::from_secs(60), &mut |_| {});
            (result, started.elapsed())
        })
    }

    #[test]
    fn a_refused_gate_fails_waiters_at_once_and_stays_refused() {
        let gate = Arc::new(GenerationGate::default());
        let waiter = wait_in_thread(&gate);
        std::thread::sleep(Duration::from_millis(30));
        let mismatch = FingerprintError::Mismatch(vec![("y_offset".into(), "0".into(), "5".into())]);
        gate.apply_fingerprint(&Err(mismatch));
        let (result, waited) = waiter.join().unwrap();
        assert!(matches!(result, Err(GenerationFailure::Refused(ref reason)) if reason.contains("y_offset")), "{result:?}");
        assert!(waited < Duration::from_secs(5), "{waited:?}");
        // A refused world never reopens, and later waits fail immediately.
        assert!(matches!(gate.apply_fingerprint(&Ok(FingerprintStatus::Matched)), GateState::Refused(_)));
        assert!(matches!(gate.wait_open(Duration::from_secs(60), Duration::from_secs(60), &mut |_| panic!("no wait")), Err(GenerationFailure::Refused(_))));
    }

    #[test]
    fn shutting_down_releases_waiters_and_interrupts_backoff() {
        let gate = Arc::new(GenerationGate::default());
        let waiter = wait_in_thread(&gate);
        let sleeper = {
            let gate = Arc::clone(&gate);
            std::thread::spawn(move || {
                let started = Instant::now();
                (gate.sleep(Duration::from_secs(60)), started.elapsed())
            })
        };
        std::thread::sleep(Duration::from_millis(30));
        gate.shut_down();
        let (result, waited) = waiter.join().unwrap();
        assert_eq!(result, Err(GenerationFailure::Cancelled));
        assert!(waited < Duration::from_secs(5));
        let (result, slept) = sleeper.join().unwrap();
        assert_eq!(result, Err(GenerationFailure::Cancelled));
        assert!(slept < Duration::from_secs(5));
        // Even an open gate turns into a cancellation once the server stops.
        assert_eq!(gate.set(GateState::Open), GateState::ShuttingDown);
        assert_eq!(gate.check(), Err(GenerationFailure::Cancelled));
    }

    #[test]
    fn a_fingerprint_check_that_never_comes_fails_after_the_bound() {
        let gate = GenerationGate::default();
        let started = Instant::now();
        let result = gate.wait_open(Duration::from_millis(10), Duration::from_millis(100), &mut |_| {});
        assert!(matches!(result, Err(GenerationFailure::Internal(ref reason)) if reason.contains("not checked")), "{result:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_poisoned_gate_is_an_internal_failure_not_an_open_gate() {
        let gate = Arc::new(GenerationGate::default());
        let poisoner = Arc::clone(&gate);
        let _ = std::thread::spawn(move || {
            let _held = poisoner.state.lock().unwrap();
            panic!("poison the gate");
        })
        .join();
        assert!(gate.state.is_poisoned());
        assert!(matches!(gate.wait_open(Duration::from_secs(60), Duration::from_secs(60), &mut |_| panic!("no wait")), Err(GenerationFailure::Internal(_))));
        assert!(matches!(gate.sleep(Duration::from_secs(60)), Err(GenerationFailure::Internal(_))));
        assert!(matches!(gate.check(), Err(GenerationFailure::Internal(_))));
        assert!(matches!(gate.state(), GateState::Failed(_)));
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
