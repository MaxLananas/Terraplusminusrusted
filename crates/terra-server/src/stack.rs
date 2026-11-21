//! Assembles the generation stack from a [`ServerConfig`]: elevation layers, tile acquisition
//! (mirrors, HTTP, disk and memory caches), the terrain generator, the chunk loader and the
//! generation fingerprint.

use crate::config::ServerConfig;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use terra_acquisition::{DiskCache, DiskCacheLimits, DiskCachedFetcher, HttpTileFetcher, PlainHttpTransport, RetryPolicy, RoutingFetcher, TransportLimits};
use terra_chunk_generation::{
    ChunkLoader, ChunkTerrain, FingerprintError, FingerprintStatus, GenerationFingerprint, KoppenMap, LoaderError, TerrainGenerator, verify_or_record,
};
use terra_elevation::{CachingTileProvider, DEFAULT_WORLD_HEIGHTS, ElevationError, FetchOutcome, TileFetcher, load_heights_config};

#[derive(Debug)]
pub enum StackError {
    Io(String),
    Elevation(ElevationError),
    Biomes(String),
    Projection(String),
}

impl std::fmt::Display for StackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(reason) => write!(f, "{reason}"),
            Self::Elevation(error) => write!(f, "elevation config: {error}"),
            Self::Biomes(reason) => write!(f, "biome raster: {reason}"),
            Self::Projection(reason) => write!(f, "projection: {reason}"),
        }
    }
}

impl std::error::Error for StackError {}

struct BoxedFetcher(Box<dyn TileFetcher>);

impl TileFetcher for BoxedFetcher {
    fn fetch(&self, url: &str) -> Result<FetchOutcome, ElevationError> {
        self.0.fetch(url)
    }
}

pub struct TerraStack {
    config: ServerConfig,
    loader: ChunkLoader,
    fingerprint: GenerationFingerprint,
    skipped_layers: Vec<String>,
}

impl TerraStack {
    /// Relative paths in `config` resolve against `base_directory`.
    pub fn build(config: ServerConfig, base_directory: &Path) -> Result<Self, StackError> {
        let heights_text = match &config.heights_config {
            Some(path) => {
                let path = base_directory.join(path);
                std::fs::read_to_string(&path).map_err(|error| StackError::Io(format!("reading {}: {error}", path.display())))?
            }
            None => DEFAULT_WORLD_HEIGHTS.to_owned(),
        };
        let layers = load_heights_config(&heights_text, config.unsupported_layers).map_err(StackError::Elevation)?;
        let network = &config.network;
        let limits = TransportLimits { connect_timeout: network.connect_timeout, io_timeout: network.io_timeout, total_timeout: network.total_timeout, ..TransportLimits::default() };
        let policy = RetryPolicy { max_attempts: network.attempts, max_per_host: network.max_per_host, ..RetryPolicy::default() };
        let mirrors: Vec<_> = network.mirrors.iter().map(|(prefix, directory)| (prefix.clone(), base_directory.join(directory))).collect();
        let routing = RoutingFetcher::new(&mirrors, HttpTileFetcher::new(PlainHttpTransport::default(), limits, policy), network.plain_http_hosts.clone());
        let fetcher: Box<dyn TileFetcher> = match &network.cache_directory {
            Some(directory) => {
                let directory = base_directory.join(directory);
                let cache = DiskCache::open(&directory, DiskCacheLimits { max_bytes: network.cache_max_bytes, ..DiskCacheLimits::default() })
                    .map_err(|error| StackError::Io(format!("opening tile cache {}: {error}", directory.display())))?;
                Box::new(DiskCachedFetcher::new(cache, routing))
            }
            None => Box::new(routing),
        };
        let provider = Arc::new(CachingTileProvider::new(BoxedFetcher(fetcher), network.memory_cache_tiles));
        let koppen = if config.generator.use_biome_dataset { Some(KoppenMap::embedded().map_err(|error| StackError::Biomes(error.to_string()))?) } else { None };
        let projection = terra_projection::terraplusminus_projection(config.generator.x_offset, config.generator.z_offset).map_err(|error| StackError::Projection(error.to_string()))?;
        let fingerprint = GenerationFingerprint::compute(&config.generator, config.world, heights_text.as_bytes(), koppen.as_ref().map(|_| KoppenMap::embedded_bytes()));
        let generator = TerrainGenerator::new(projection, Arc::new(layers.dataset), provider, koppen, config.generator.clone());
        let loader = ChunkLoader::new(Arc::new(generator), config.loader).map_err(|error| StackError::Io(format!("starting chunk workers: {error}")))?;
        Ok(Self { config, loader, fingerprint, skipped_layers: layers.skipped })
    }

    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    pub fn loader(&self) -> &ChunkLoader {
        &self.loader
    }

    pub fn generator(&self) -> &TerrainGenerator {
        self.loader.generator()
    }

    pub fn skipped_layers(&self) -> &[String] {
        &self.skipped_layers
    }

    pub fn fingerprint(&self) -> &GenerationFingerprint {
        &self.fingerprint
    }

    /// Records the fingerprint for a new world, or refuses a world generated differently.
    pub fn verify_world(&self, world_directory: &Path) -> Result<FingerprintStatus, FingerprintError> {
        verify_or_record(world_directory, &self.fingerprint)
    }

    /// For generation worker threads, which must produce real terrain or nothing: waits and
    /// retries with backoff until the chunk data loads. `on_retry` reports each failure. Only a
    /// loader shutdown ends the wait unsuccessfully.
    pub fn terrain_for_generation(&self, chunk_x: i32, chunk_z: i32, on_retry: &mut dyn FnMut(&LoaderError, u32)) -> Result<Arc<ChunkTerrain>, LoaderError> {
        let mut backoff = Duration::from_millis(500);
        for attempt in 1.. {
            match self.loader.load_blocking(chunk_x, chunk_z, self.config.generation_wait) {
                Ok(terrain) => return Ok(terrain),
                Err(LoaderError::ShutDown) => return Err(LoaderError::ShutDown),
                Err(error) => {
                    on_retry(&error, attempt);
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(Duration::from_secs(30));
                }
            }
        }
        Err(LoaderError::ShutDown)
    }

    pub fn diagnostics_report(&self) -> String {
        let loader = self.loader.diagnostics();
        let mut report = String::new();
        let _ = writeln!(report, "chunk data: {} queued, {} running, {} ready; {} completed, {} failed", loader.queued, loader.running, loader.ready, loader.completed, loader.failed);
        let _ = writeln!(report, "requests: {} deduplicated, {} cancelled, {} refused (queue full)", loader.deduplicated, loader.cancelled, loader.rejected);
        if !self.skipped_layers.is_empty() {
            let _ = writeln!(report, "elevation layers skipped: {}", self.skipped_layers.len());
        }
        report
    }
}
