//! Asynchronous chunk-data loading off the server tick: a fixed worker pool fed by a bounded,
//! deduplicating queue, with cancellation of unwanted requests, backpressure when full and a
//! bounded cache of finished chunks.

use crate::chunk_data::ChunkTerrain;
use crate::generator::{GenerationError, TerrainGenerator};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub type ChunkKey = (i32, i32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoaderLimits {
    pub workers: usize,
    /// Requests waiting for a worker; beyond this new requests are refused.
    pub queue_capacity: usize,
    /// Finished chunks kept for reuse.
    pub ready_capacity: usize,
}

impl LoaderLimits {
    /// Workers from the measured parallelism (leaving one core to the server), the queue sized to
    /// a few seconds of work.
    pub fn for_host() -> Self {
        let cores = std::thread::available_parallelism().map_or(1, usize::from);
        let workers = cores.saturating_sub(1).clamp(1, 16);
        Self { workers, queue_capacity: 1024, ready_capacity: 4096 }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum LoaderError {
    Overloaded,
    TimedOut,
    ShutDown,
    Generation(GenerationError),
}

impl std::fmt::Display for LoaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Overloaded => write!(f, "chunk loader queue is full"),
            Self::TimedOut => write!(f, "timed out waiting for chunk data"),
            Self::ShutDown => write!(f, "chunk loader is shut down"),
            Self::Generation(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for LoaderError {}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LoaderDiagnostics {
    pub queued: usize,
    pub running: usize,
    pub ready: usize,
    pub completed: u64,
    pub failed: u64,
    pub deduplicated: u64,
    pub cancelled: u64,
    pub rejected: u64,
}

enum Slot {
    Queued { interest: usize },
    Running { interest: usize },
    Ready { terrain: Arc<ChunkTerrain>, tick: u64 },
}

#[derive(Default)]
struct State {
    queue: VecDeque<ChunkKey>,
    slots: HashMap<ChunkKey, Slot>,
    /// Failures delivered to the tickets waiting at the time, then forgotten.
    failures: HashMap<ChunkKey, (GenerationError, usize)>,
    recency: BTreeMap<u64, ChunkKey>,
    clock: u64,
    shutdown: bool,
    diagnostics: LoaderDiagnostics,
}

struct Shared {
    generator: Arc<TerrainGenerator>,
    limits: LoaderLimits,
    state: Mutex<State>,
    work_available: Condvar,
    progress: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub struct ChunkLoader {
    shared: Arc<Shared>,
    workers: Vec<JoinHandle<()>>,
}

/// Interest in one chunk. Dropping every ticket of a chunk that has not started cancels it.
pub struct ChunkTicket {
    shared: Arc<Shared>,
    key: ChunkKey,
    resolved: Option<Result<Arc<ChunkTerrain>, LoaderError>>,
}

impl ChunkLoader {
    pub fn new(generator: Arc<TerrainGenerator>, limits: LoaderLimits) -> std::io::Result<Self> {
        let limits = LoaderLimits { workers: limits.workers.max(1), queue_capacity: limits.queue_capacity.max(1), ready_capacity: limits.ready_capacity.max(1) };
        let shared = Arc::new(Shared { generator, limits, state: Mutex::new(State::default()), work_available: Condvar::new(), progress: Condvar::new() });
        let mut workers = Vec::with_capacity(limits.workers);
        for index in 0..limits.workers {
            let worker_shared = Arc::clone(&shared);
            let spawned = std::thread::Builder::new().name(format!("terra-chunk-{index}")).spawn(move || worker(&worker_shared));
            match spawned {
                Ok(handle) => workers.push(handle),
                Err(error) => {
                    shut_down(&shared, &mut workers);
                    return Err(error);
                }
            }
        }
        Ok(Self { shared, workers })
    }

    pub fn generator(&self) -> &Arc<TerrainGenerator> {
        &self.shared.generator
    }

    /// Never blocks: queues the chunk, joins an existing request or returns the cached result.
    pub fn request(&self, chunk_x: i32, chunk_z: i32) -> Result<ChunkTicket, LoaderError> {
        let key = (chunk_x, chunk_z);
        let mut state = self.shared.lock();
        if state.shutdown {
            return Err(LoaderError::ShutDown);
        }
        state.clock += 1;
        let clock = state.clock;
        let mut resolved = None;
        let mut touched = None;
        match state.slots.get_mut(&key) {
            Some(Slot::Ready { terrain, tick }) => {
                resolved = Some(Ok(Arc::clone(terrain)));
                touched = Some(std::mem::replace(tick, clock));
            }
            Some(Slot::Queued { interest } | Slot::Running { interest }) => {
                *interest += 1;
                state.diagnostics.deduplicated += 1;
            }
            None => {
                if state.queue.len() >= self.shared.limits.queue_capacity {
                    state.diagnostics.rejected += 1;
                    return Err(LoaderError::Overloaded);
                }
                state.slots.insert(key, Slot::Queued { interest: 1 });
                state.queue.push_back(key);
                self.shared.work_available.notify_one();
            }
        }
        if let Some(previous) = touched {
            state.recency.remove(&previous);
            state.recency.insert(clock, key);
        }
        drop(state);
        Ok(ChunkTicket { shared: Arc::clone(&self.shared), key, resolved })
    }

    /// For generation worker threads: waits for queue space as well as for the result.
    pub fn load_blocking(&self, chunk_x: i32, chunk_z: i32, timeout: Duration) -> Result<Arc<ChunkTerrain>, LoaderError> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.request(chunk_x, chunk_z) {
                Ok(mut ticket) => return ticket.wait(deadline.saturating_duration_since(Instant::now())),
                Err(LoaderError::Overloaded) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err(LoaderError::TimedOut);
                    }
                    let state = self.shared.lock();
                    drop(self.shared.progress.wait_timeout(state, remaining.min(Duration::from_millis(50))));
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub fn is_ready(&self, chunk_x: i32, chunk_z: i32) -> bool {
        matches!(self.shared.lock().slots.get(&(chunk_x, chunk_z)), Some(Slot::Ready { .. }))
    }

    pub fn diagnostics(&self) -> LoaderDiagnostics {
        let state = self.shared.lock();
        let mut diagnostics = state.diagnostics;
        diagnostics.queued = state.queue.len();
        diagnostics.running = state.slots.values().filter(|slot| matches!(slot, Slot::Running { .. })).count();
        diagnostics.ready = state.recency.len();
        diagnostics
    }
}

impl Drop for ChunkLoader {
    fn drop(&mut self) {
        shut_down(&self.shared, &mut self.workers);
    }
}

fn shut_down(shared: &Shared, workers: &mut Vec<JoinHandle<()>>) {
    {
        let mut state = shared.lock();
        state.shutdown = true;
        state.queue.clear();
        state.slots.retain(|_, slot| !matches!(slot, Slot::Queued { .. }));
    }
    shared.work_available.notify_all();
    shared.progress.notify_all();
    for handle in workers.drain(..) {
        let _ = handle.join();
    }
}

fn worker(shared: &Shared) {
    loop {
        let key = {
            let mut state = shared.lock();
            loop {
                if state.shutdown {
                    return;
                }
                if let Some(key) = state.queue.pop_front() {
                    if let Some(slot) = state.slots.get_mut(&key)
                        && let Slot::Queued { interest } = *slot
                    {
                        *slot = Slot::Running { interest };
                        break key;
                    }
                    continue;
                }
                state = shared.work_available.wait(state).unwrap_or_else(PoisonError::into_inner);
            }
        };
        let result = shared.generator.chunk_terrain(key.0, key.1);
        let mut state = shared.lock();
        let interest = match state.slots.remove(&key) {
            Some(Slot::Running { interest }) => interest,
            _ => 0,
        };
        match result {
            Ok(terrain) => {
                state.clock += 1;
                let tick = state.clock;
                state.slots.insert(key, Slot::Ready { terrain: Arc::new(terrain), tick });
                state.recency.insert(tick, key);
                state.diagnostics.completed += 1;
                while state.recency.len() > shared.limits.ready_capacity {
                    let Some((_, evicted)) = state.recency.pop_first() else { break };
                    state.slots.remove(&evicted);
                }
            }
            Err(error) => {
                state.diagnostics.failed += 1;
                if interest > 0 {
                    state.failures.insert(key, (error, interest));
                }
            }
        }
        drop(state);
        shared.progress.notify_all();
    }
}

impl ChunkTicket {
    pub fn key(&self) -> ChunkKey {
        self.key
    }

    fn poll(&mut self, state: &mut State) -> Option<Result<Arc<ChunkTerrain>, LoaderError>> {
        if self.resolved.is_some() {
            return self.resolved.clone();
        }
        let outcome = match state.slots.get(&self.key) {
            Some(Slot::Ready { terrain, .. }) => Ok(Arc::clone(terrain)),
            Some(Slot::Queued { .. } | Slot::Running { .. }) => return None,
            None => match state.failures.get_mut(&self.key) {
                Some((error, waiting)) => {
                    let error = error.clone();
                    *waiting -= 1;
                    if *waiting == 0 {
                        state.failures.remove(&self.key);
                    }
                    Err(LoaderError::Generation(error))
                }
                None if state.shutdown => Err(LoaderError::ShutDown),
                None => return None,
            },
        };
        self.resolved = Some(outcome.clone());
        Some(outcome)
    }

    pub fn try_get(&mut self) -> Option<Result<Arc<ChunkTerrain>, LoaderError>> {
        let shared = Arc::clone(&self.shared);
        let mut state = shared.lock();
        self.poll(&mut state)
    }

    pub fn wait(&mut self, timeout: Duration) -> Result<Arc<ChunkTerrain>, LoaderError> {
        let deadline = Instant::now() + timeout;
        let shared = Arc::clone(&self.shared);
        let mut state = shared.lock();
        loop {
            if let Some(outcome) = self.poll(&mut state) {
                return outcome;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(LoaderError::TimedOut);
            }
            state = shared.progress.wait_timeout(state, remaining).unwrap_or_else(PoisonError::into_inner).0;
        }
    }
}

impl Drop for ChunkTicket {
    fn drop(&mut self) {
        if self.resolved.is_some() {
            return;
        }
        let mut state = self.shared.lock();
        let cancel = match state.slots.get_mut(&self.key) {
            Some(Slot::Queued { interest }) => {
                *interest -= 1;
                *interest == 0
            }
            Some(Slot::Running { interest }) => {
                *interest = interest.saturating_sub(1);
                false
            }
            _ => {
                if let Some((_, waiting)) = state.failures.get_mut(&self.key) {
                    *waiting -= 1;
                    if *waiting == 0 {
                        state.failures.remove(&self.key);
                    }
                }
                false
            }
        };
        if cancel {
            state.slots.remove(&self.key);
            state.queue.retain(|key| *key != self.key);
            state.diagnostics.cancelled += 1;
        }
    }
}
