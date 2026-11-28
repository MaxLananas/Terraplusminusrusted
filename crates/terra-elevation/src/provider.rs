//! Tile acquisition: URL fetchers, `Http.getFirst` semantics, and a bounded, deduplicating cache.

use crate::bounds::TilePosition;
use crate::dataset::{Tile, TiledScalarDataset};
use crate::error::ElevationError;
use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchOutcome {
    Found(Vec<u8>),
    /// The source answered authoritatively that the resource does not exist (HTTP 404).
    Missing,
}

pub trait TileFetcher: Send + Sync {
    fn fetch(&self, url: &str) -> Result<FetchOutcome, ElevationError>;
}

pub trait TileProvider: Send + Sync {
    /// `Ok(None)` when every source reported the tile missing.
    fn load(&self, dataset: &TiledScalarDataset, position: TilePosition) -> Result<Option<Tile>, ElevationError>;

    fn load_many(&self, dataset: &TiledScalarDataset, positions: &[TilePosition]) -> Result<Vec<Option<Tile>>, ElevationError> {
        positions.iter().map(|position| self.load(dataset, *position)).collect()
    }
}

/// Fetches and decodes one tile with `Http.getFirst` semantics: the first URL that yields a
/// decodable tile wins; a 404 on any URL makes total failure resolve to "missing"; otherwise the
/// collected failures are reported.
pub fn fetch_first(fetcher: &dyn TileFetcher, dataset: &TiledScalarDataset, position: TilePosition) -> Result<Option<Tile>, ElevationError> {
    let urls = dataset.urls(position);
    if urls.is_empty() {
        return Ok(None);
    }
    let mut found_missing = false;
    let mut failures = Vec::new();
    for url in &urls {
        let attempt = fetcher.fetch(url).and_then(|outcome| match outcome {
            FetchOutcome::Missing => Ok(None),
            FetchOutcome::Found(bytes) => dataset
                .decode(&bytes)
                .map(Some)
                .map_err(|error| match error {
                    ElevationError::TileDecode(error) => ElevationError::Decode { url: url.clone(), error },
                    other => other,
                }),
        });
        match attempt {
            Ok(Some(tile)) => return Ok(Some(tile)),
            Ok(None) => found_missing = true,
            Err(error) if urls.len() == 1 => return Err(error),
            Err(error) => failures.push(error.to_string()),
        }
    }
    if found_missing { Ok(None) } else { Err(ElevationError::AllSourcesFailed { dataset: dataset.id.clone(), failures }) }
}

/// Serves URLs from a local mirror: each URL prefix maps to a directory. Only plain path segments
/// (`[A-Za-z0-9._-]`, never `.` or `..`) are accepted, so a URL cannot escape its root.
#[derive(Debug, Clone, Default)]
pub struct MirrorFetcher {
    mappings: Vec<(String, PathBuf)>,
}

impl MirrorFetcher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_mapping(mut self, url_prefix: impl Into<String>, root: impl Into<PathBuf>) -> Self {
        self.mappings.push((url_prefix.into(), root.into()));
        self
    }

    pub fn resolve(&self, url: &str) -> Result<PathBuf, ElevationError> {
        let rejected = |reason: &str| ElevationError::Fetch { url: url.to_owned(), reason: reason.to_owned() };
        let (prefix, root) = self
            .mappings
            .iter()
            .filter(|(prefix, _)| url.starts_with(prefix.as_str()))
            .max_by_key(|(prefix, _)| prefix.len())
            .ok_or_else(|| rejected("no mirror mapping for url"))?;
        let relative = &url[prefix.len()..];
        let mut path = root.clone();
        for segment in relative.split('/') {
            let safe = !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
            if !safe {
                return Err(rejected("unsafe path segment"));
            }
            path.push(segment);
        }
        if Path::new(relative).components().any(|component| !matches!(component, Component::Normal(_))) {
            return Err(rejected("unsafe path"));
        }
        Ok(path)
    }
}

/// Upper bound on a single tile download, compressed.
pub const MAX_TILE_BYTES: u64 = 16 << 20;

impl TileFetcher for MirrorFetcher {
    fn fetch(&self, url: &str) -> Result<FetchOutcome, ElevationError> {
        use std::io::Read;
        let path = self.resolve(url)?;
        let failed = |reason: String| ElevationError::Fetch { url: url.to_owned(), reason };
        let file = match std::fs::File::open(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(FetchOutcome::Missing),
            Err(error) => return Err(failed(error.to_string())),
            Ok(file) => file,
        };
        if !file.metadata().map_err(|error| failed(error.to_string()))?.is_file() {
            return Err(failed("not a regular file".into()));
        }
        // The size is enforced on what is read, not on what the file claimed when opened: a file
        // growing meanwhile (or a device) cannot exceed the limit.
        let mut bytes = Vec::new();
        file.take(MAX_TILE_BYTES + 1).read_to_end(&mut bytes).map_err(|error| failed(error.to_string()))?;
        if bytes.len() as u64 > MAX_TILE_BYTES {
            return Err(failed(format!("more than {MAX_TILE_BYTES} bytes exceeds limit")));
        }
        Ok(FetchOutcome::Found(bytes))
    }
}

type CacheKey = (Arc<str>, TilePosition);

#[derive(Default)]
struct CacheState {
    entries: HashMap<CacheKey, (Option<Tile>, u64)>,
    recency: BTreeMap<u64, CacheKey>,
    in_flight: HashMap<CacheKey, Arc<InFlight>>,
    clock: u64,
    hits: u64,
    misses: u64,
}

#[derive(Default)]
enum FlightOutcome {
    #[default]
    Pending,
    Done(Result<Option<Tile>, ElevationError>),
    /// The leader's fetch panicked: followers start over (one of them becomes the new leader).
    Abandoned,
}

#[derive(Default)]
struct InFlight {
    result: Mutex<FlightOutcome>,
    ready: Condvar,
}

impl InFlight {
    fn finish(&self, outcome: FlightOutcome) {
        *self.result.lock().unwrap_or_else(PoisonError::into_inner) = outcome;
        self.ready.notify_all();
    }
}

/// Unregisters the leader's flight if its fetch unwinds, so the tile is not left "loading"
/// forever with every later request for it blocked.
struct LeaderGuard<'a> {
    state: &'a Mutex<CacheState>,
    key: Option<CacheKey>,
    flight: Arc<InFlight>,
}

impl Drop for LeaderGuard<'_> {
    fn drop(&mut self) {
        if let Some(key) = self.key.take() {
            self.state.lock().unwrap_or_else(PoisonError::into_inner).in_flight.remove(&key);
            self.flight.finish(FlightOutcome::Abandoned);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CacheStatistics {
    pub entries: usize,
    pub hits: u64,
    pub misses: u64,
}

/// Decoded-tile cache bounded by entry count (a 256x256 tile is 512 KiB), with in-flight
/// deduplication: concurrent requests for one tile share a single fetch. Errors are not cached.
pub struct CachingTileProvider<F> {
    fetcher: F,
    capacity: usize,
    state: Mutex<CacheState>,
}

impl<F: TileFetcher> CachingTileProvider<F> {
    pub fn new(fetcher: F, capacity: usize) -> Self {
        Self { fetcher, capacity: capacity.max(1), state: Mutex::new(CacheState::default()) }
    }

    pub fn statistics(&self) -> CacheStatistics {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        CacheStatistics { entries: state.entries.len(), hits: state.hits, misses: state.misses }
    }

    fn insert(&self, state: &mut CacheState, key: CacheKey, tile: Option<Tile>) {
        state.clock += 1;
        let tick = state.clock;
        if let Some((_, old_tick)) = state.entries.insert(key.clone(), (tile, tick)) {
            state.recency.remove(&old_tick);
        }
        state.recency.insert(tick, key);
        while state.entries.len() > self.capacity {
            let Some((_, oldest)) = state.recency.pop_first() else { break };
            state.entries.remove(&oldest);
        }
    }
}

enum Role {
    Leader(Arc<InFlight>),
    Follower(Arc<InFlight>),
}

impl<F: TileFetcher> TileProvider for CachingTileProvider<F> {
    fn load(&self, dataset: &TiledScalarDataset, position: TilePosition) -> Result<Option<Tile>, ElevationError> {
        let key: CacheKey = (Arc::from(dataset.id.as_str()), position);
        loop {
            let role = {
                let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                state.clock += 1;
                let tick = state.clock;
                if let Some((tile, old_tick)) = state.entries.get_mut(&key) {
                    let tile = tile.clone();
                    let previous = std::mem::replace(old_tick, tick);
                    state.recency.remove(&previous);
                    state.recency.insert(tick, key);
                    state.hits += 1;
                    return Ok(tile);
                }
                state.misses += 1;
                match state.in_flight.get(&key) {
                    Some(flight) => Role::Follower(Arc::clone(flight)),
                    None => {
                        let flight = Arc::new(InFlight::default());
                        state.in_flight.insert(key.clone(), Arc::clone(&flight));
                        Role::Leader(flight)
                    }
                }
            };
            match role {
                Role::Follower(flight) => {
                    let mut result = flight.result.lock().unwrap_or_else(PoisonError::into_inner);
                    loop {
                        match &*result {
                            FlightOutcome::Done(outcome) => return outcome.clone(),
                            FlightOutcome::Abandoned => break,
                            FlightOutcome::Pending => result = flight.ready.wait(result).unwrap_or_else(PoisonError::into_inner),
                        }
                    }
                }
                Role::Leader(flight) => {
                    let mut guard = LeaderGuard { state: &self.state, key: Some(key.clone()), flight: Arc::clone(&flight) };
                    let outcome = fetch_first(&self.fetcher, dataset, position);
                    guard.key = None;
                    {
                        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                        state.in_flight.remove(&key);
                        if let Ok(tile) = &outcome {
                            self.insert(&mut state, key, tile.clone());
                        }
                    }
                    flight.finish(FlightOutcome::Done(outcome.clone()));
                    return outcome;
                }
            }
        }
    }
}
