//! Assembles the generation stack from a [`ServerConfig`]: elevation layers, tile acquisition
//! (mirrors, HTTP, disk and memory caches), the terrain generator, the chunk loader and the
//! generation fingerprint.

use crate::config::ServerConfig;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use terra_acquisition::{DiskCache, DiskCacheLimits, DiskCachedFetcher, HttpTileFetcher, PlainHttpTransport, RetryPolicy, RoutingFetcher, TransportLimits};
use terra_chunk_generation::{
    Block, ChunkLoader, FingerprintError, FingerprintStatus, GenerationFingerprint, KoppenMap, TerrainGenerator, TreeLibrary, TreePopulator,
    verify_or_record,
};
use terra_osm::{DEFAULT_OSM_CONFIG, OsmDataset, OsmMapper, OsmSettings};
use terra_elevation::{CachingTileProvider, DEFAULT_TREE_COVER, DEFAULT_WORLD_HEIGHTS, ElevationError, FetchOutcome, TileFetcher, load_heights_config, load_tree_cover_config};

#[derive(Debug)]
pub enum StackError {
    Io(String),
    Elevation(ElevationError),
    Biomes(String),
    Projection(String),
    OpenStreetMap(String),
    Trees(String),
    /// Enabled data sources that this build cannot fetch.
    Unreachable(String),
    /// Block states (from the OpenStreetMap mapping or the tree library) that are malformed.
    InvalidBlocks(String),
}

impl std::fmt::Display for StackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(reason) => write!(f, "{reason}"),
            Self::Elevation(error) => write!(f, "elevation config: {error}"),
            Self::Biomes(reason) => write!(f, "biome raster: {reason}"),
            Self::Projection(reason) => write!(f, "projection: {reason}"),
            Self::OpenStreetMap(reason) => write!(f, "OpenStreetMap: {reason}"),
            Self::Trees(reason) => write!(f, "trees: {reason}"),
            Self::Unreachable(reason) => write!(f, "{reason}"),
            Self::InvalidBlocks(reason) => write!(f, "{reason}"),
        }
    }
}

impl std::error::Error for StackError {}

/// The acquisition chain (mirrors, HTTP, disk cache), shared by elevation and OpenStreetMap.
struct SharedFetcher(Arc<dyn TileFetcher>);

impl TileFetcher for SharedFetcher {
    fn fetch(&self, url: &str) -> Result<FetchOutcome, ElevationError> {
        self.0.fetch(url)
    }
}

/// Where the stack writes operator messages (fetch logging).
pub type LogSink = Arc<dyn Fn(&str) + Send + Sync>;

/// `reduced_console_messages: false`: every fetch and its outcome is logged, as TerraMinusMinus'
/// `Http` and `Disk` log cache hits, misses and failures.
struct LoggingFetcher {
    inner: Arc<dyn TileFetcher>,
    log: LogSink,
}

impl TileFetcher for LoggingFetcher {
    fn fetch(&self, url: &str) -> Result<FetchOutcome, ElevationError> {
        let started = std::time::Instant::now();
        let result = self.inner.fetch(url);
        let elapsed = started.elapsed().as_millis();
        match &result {
            Ok(FetchOutcome::Found(bytes)) => (self.log)(&format!("Fetched {url}: {} bytes in {elapsed} ms", bytes.len())),
            Ok(FetchOutcome::Missing) => (self.log)(&format!("Not found: {url} ({elapsed} ms)")),
            Err(error) => (self.log)(&format!("Request failed: {url}: {error} ({elapsed} ms)")),
        }
        result
    }
}

/// Refuses sources none of whose URLs this build can fetch, naming each with the fixes.
fn check_sources<T: terra_acquisition::HttpTransport>(routing: &RoutingFetcher<T>, sources: &[(String, Vec<String>)]) -> Result<(), StackError> {
    let mut problems = Vec::new();
    for (label, templates) in sources {
        let reasons: Vec<String> = templates.iter().filter_map(|template| routing.check_route(template).err()).collect();
        if reasons.len() == templates.len() {
            let reason = if templates.is_empty() { "no URLs configured".to_owned() } else { reasons.join("; ") };
            problems.push(format!("{label}: {reason}"));
        }
    }
    if problems.is_empty() {
        return Ok(());
    }
    let count = problems.len();
    let shown: Vec<String> = problems.into_iter().take(8).collect();
    let more = if count > shown.len() { format!(" (and {} more)", count - shown.len()) } else { String::new() };
    Err(StackError::Unreachable(format!(
        "{count} enabled data source(s) cannot be fetched by this build, which has no TLS: {}{more}. Fix: map the URL prefixes to local copies with network.mirrors, \
         or list the hosts in network.plain_http_hosts (unencrypted, unauthenticated HTTP; only for hosts that serve the same data over HTTP), \
         or turn the feature off (openstreetmap.enabled, generate_trees, elevation.heights_config)",
        shown.join("; ")
    )))
}

pub struct TerraStack {
    config: ServerConfig,
    loader: ChunkLoader,
    fingerprint: GenerationFingerprint,
    skipped_layers: Vec<String>,
}

impl TerraStack {
    /// Relative paths in `config` resolve against `base_directory`. Messages go to stderr.
    pub fn build(config: ServerConfig, base_directory: &Path) -> Result<Self, StackError> {
        Self::build_logged(config, base_directory, Arc::new(|message: &str| eprintln!("[terraplusminus] {message}")))
    }

    /// As [`TerraStack::build`], with operator messages sent to `log`.
    pub fn build_logged(config: ServerConfig, base_directory: &Path, log: LogSink) -> Result<Self, StackError> {
        let heights_text = match &config.heights_config {
            Some(path) => {
                let path = base_directory.join(path);
                std::fs::read_to_string(&path).map_err(|error| StackError::Io(format!("reading {}: {error}", path.display())))?
            }
            None => DEFAULT_WORLD_HEIGHTS.to_owned(),
        };
        let mut layers = load_heights_config(&heights_text, config.unsupported_layers).map_err(StackError::Elevation)?;
        let network = &config.network;
        let limits = TransportLimits { connect_timeout: network.connect_timeout, io_timeout: network.io_timeout, total_timeout: network.total_timeout, ..TransportLimits::default() };
        // One fetch never outlives its attempts plus their backoffs, however the server behaves.
        let base = RetryPolicy::default();
        let fetch_deadline = network.total_timeout.saturating_mul(network.attempts.max(1)) + base.max_backoff.saturating_mul(network.attempts.saturating_sub(1));
        let policy = RetryPolicy { max_attempts: network.attempts, max_per_host: network.max_per_host, fetch_deadline, ..base };
        let mirrors: Vec<_> = network.mirrors.iter().map(|(prefix, directory)| (prefix.clone(), base_directory.join(directory))).collect();
        let routing = RoutingFetcher::new(&mirrors, HttpTileFetcher::new(PlainHttpTransport::default(), limits, policy), network.plain_http_hosts.clone());
        // Every enabled source must be fetchable before anything is registered: generation would
        // otherwise wait on data that can never arrive.
        let mut sources: Vec<(String, Vec<String>)> = layers.dataset.layers().iter().map(|layer| (format!("elevation layer {}", layer.dataset.id), layer.dataset.url_templates.clone())).collect();
        let tree_cover = if config.generate_trees {
            let tree_cover_text = match &config.tree_cover_config {
                Some(path) => {
                    let path = base_directory.join(path);
                    std::fs::read_to_string(&path).map_err(|error| StackError::Io(format!("reading {}: {error}", path.display())))?
                }
                None => DEFAULT_TREE_COVER.to_owned(),
            };
            let tree_cover = load_tree_cover_config(&tree_cover_text, config.unsupported_layers).map_err(|error| StackError::Trees(format!("tree cover config: {error}")))?;
            sources.extend(tree_cover.dataset.layers().iter().map(|layer| (format!("tree cover layer {}", layer.dataset.id), layer.dataset.url_templates.clone())));
            Some((tree_cover_text, tree_cover))
        } else {
            None
        };
        if config.openstreetmap.enabled {
            sources.push(("OpenStreetMap (openstreetmap.servers)".to_owned(), config.openstreetmap.servers.clone()));
        }
        check_sources(&routing, &sources)?;
        let fetcher: Arc<dyn TileFetcher> = match &network.cache_directory {
            Some(directory) => {
                let directory = base_directory.join(directory);
                let cache = DiskCache::open(&directory, DiskCacheLimits { max_bytes: network.cache_max_bytes, ..DiskCacheLimits::default() })
                    .map_err(|error| StackError::Io(format!("opening tile cache {}: {error}", directory.display())))?;
                Arc::new(DiskCachedFetcher::new(cache, routing))
            }
            None => Arc::new(routing),
        };
        let fetcher: Arc<dyn TileFetcher> = if config.reduced_console_messages { fetcher } else { Arc::new(LoggingFetcher { inner: fetcher, log }) };
        let provider = Arc::new(CachingTileProvider::new(SharedFetcher(Arc::clone(&fetcher)), network.memory_cache_tiles));
        let koppen = if config.generator.use_biome_dataset { Some(KoppenMap::embedded().map_err(|error| StackError::Biomes(error.to_string()))?) } else { None };
        let projection = terra_projection::terraplusminus_projection(config.generator.x_offset, config.generator.z_offset).map_err(|error| StackError::Projection(error.to_string()))?;
        let mut fingerprint = GenerationFingerprint::compute(&config.generator, config.world, heights_text.as_bytes(), koppen.as_ref().map(|_| KoppenMap::embedded_bytes()));
        let mut generator = TerrainGenerator::new(Arc::clone(&projection), Arc::new(layers.dataset), provider, koppen, config.generator.clone());
        if let Some((tree_cover_text, tree_cover)) = tree_cover {
            layers.skipped.extend(tree_cover.skipped.into_iter().map(|skipped| format!("tree cover {skipped}")));
            let library = TreeLibrary::upstream().map_err(|error| StackError::Trees(error.to_string()))?;
            fingerprint = fingerprint.with_trees(terra_chunk_generation::trees::UPSTREAM_TREES.as_bytes(), tree_cover_text.as_bytes());
            generator = generator.with_trees(Arc::new(TreePopulator::new(library, config.generator.y_offset)), Arc::new(tree_cover.dataset));
        }
        let osm_config = &config.openstreetmap;
        if osm_config.enabled {
            let (mapping_text, source) = read_osm_mapping(osm_config.mapping.as_deref(), base_directory)?;
            let mapper = OsmMapper::parse(&mapping_text).map_err(|error| StackError::OpenStreetMap(format!("{source}: {error}")))?;
            let settings = OsmSettings { servers: osm_config.servers.clone(), tile_cache_entries: osm_config.memory_cache_tiles, ..OsmSettings::default() };
            fingerprint = fingerprint.with_openstreetmap(mapping_text.as_bytes(), &settings.servers);
            generator = generator.with_openstreetmap(Arc::new(OsmDataset::new(Arc::new(mapper), projection, fetcher, &settings)));
        }
        fingerprint = fingerprint.with_skipped_layers(&layers.skipped);
        let loader = ChunkLoader::new(Arc::new(generator), config.loader).map_err(|error| StackError::Io(format!("starting chunk workers: {error}")))?;
        let stack = Self { config, loader, fingerprint, skipped_layers: layers.skipped };
        let invalid: Vec<String> = stack.surface_block_names().iter().filter_map(|name| crate::engine_bridge::validate_block_state(name).err()).collect();
        if !invalid.is_empty() {
            return Err(StackError::InvalidBlocks(invalid.join("; ")));
        }
        Ok(stack)
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

    /// Every block the generator can place beyond the base terrain blocks: surface blocks from
    /// OpenStreetMap features and tree blocks, as block state strings (`minecraft:bricks`,
    /// `minecraft:oak_slab[type=top]`, `minecraft:oak_leaves[persistent=true]`), deduplicated.
    pub fn surface_block_names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        let replacements = &self.config.generator.surface_replacements;
        if let Some(osm) = self.generator().openstreetmap() {
            for block in osm.mapper().blocks() {
                let name = block.to_string();
                match replacements.iter().find(|(id, _)| *id == block.id) {
                    Some((_, replacement)) => names.push(replacement.resource_name().to_owned()),
                    None => names.push(Block::from_material_name(&name).resource_name().to_owned()),
                }
            }
        }
        if let Some(trees) = self.generator().trees() {
            names.extend(trees.library().blocks().iter().map(|block| block.resource_name().to_owned()));
        }
        let mut seen = std::collections::HashSet::new();
        names.retain(|name| seen.insert(name.clone()));
        names
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

    /// [`TerraStack::verify_world`] for a level generated at `y_offset` (a linked level).
    pub fn verify_level(&self, level_directory: &Path, y_offset: i32) -> Result<FingerprintStatus, FingerprintError> {
        verify_or_record(level_directory, &self.fingerprint.clone().with_y_offset(y_offset))
    }

    /// Stops generation work: the loader refuses new requests and releases every waiter.
    pub fn shut_down(&self) {
        self.loader.shut_down();
    }

    pub fn diagnostics_report(&self) -> String {
        let loader = self.loader.diagnostics();
        let mut report = String::new();
        let _ = writeln!(report, "chunk data: {} queued, {} running, {} ready; {} completed, {} failed", loader.queued, loader.running, loader.ready, loader.completed, loader.failed);
        let _ = writeln!(report, "requests: {} deduplicated, {} cancelled, {} refused (queue full)", loader.deduplicated, loader.cancelled, loader.rejected);
        if let Some(osm) = self.generator().openstreetmap() {
            let statistics = osm.statistics();
            let _ = writeln!(
                report,
                "OpenStreetMap: {} tiles cached ({} hits, {} misses), {} resources cached",
                statistics.tiles.entries, statistics.tiles.hits, statistics.tiles.misses, statistics.resources.entries
            );
        }
        if !self.skipped_layers.is_empty() {
            let _ = writeln!(report, "elevation layers skipped: {}", self.skipped_layers.len());
        }
        report
    }
}

/// File name of the editable mapping beside the configuration, as TerraPlusMinus extracts it.
pub const OSM_MAPPING_FILE: &str = "osm.json5";

/// The `osm.json5` text and where it came from: the configured path, `osm.json5` beside the
/// configuration, or the bundled default.
fn read_osm_mapping(configured: Option<&Path>, base_directory: &Path) -> Result<(String, String), StackError> {
    let read = |path: &Path| std::fs::read_to_string(path).map_err(|error| StackError::Io(format!("reading {}: {error}", path.display())));
    if let Some(path) = configured {
        let path = base_directory.join(path);
        return Ok((read(&path)?, path.display().to_string()));
    }
    let beside = base_directory.join(OSM_MAPPING_FILE);
    match std::fs::metadata(&beside) {
        Ok(_) => Ok((read(&beside)?, beside.display().to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok((DEFAULT_OSM_CONFIG.to_owned(), "bundled osm.json5".to_owned())),
        Err(error) => Err(StackError::Io(format!("reading {}: {error}", beside.display()))),
    }
}

/// Writes the bundled `osm.json5` beside the configuration for editing, unless a file is
/// already there (`Terraplusminus.extractTerraConfigFileToPluginDir`). Returns whether it wrote.
pub fn extract_default_osm_mapping(base_directory: &Path) -> std::io::Result<bool> {
    let path = base_directory.join(OSM_MAPPING_FILE);
    std::fs::create_dir_all(base_directory)?;
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => {
            std::io::Write::write_all(&mut file, DEFAULT_OSM_CONFIG.as_bytes())?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    }
}
