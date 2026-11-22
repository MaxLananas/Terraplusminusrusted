//! Server configuration: TerraPlusMinus' `config.yml` keys, in the relaxed JSON dialect, plus
//! the settings this implementation adds (world height, elevation, network, permissions).
//! Unknown keys are errors so a misspelt setting never silently falls back to a default.

use crate::tpll::{LinkedWorld, LinkedWorlds, LinkedWorldsMethod, TeamBounds};
use std::path::PathBuf;
use std::time::Duration;
use terra_chunk_generation::{Biome, Block, GeneratorSettings, LoaderLimits, WorldHeight};
use terra_codec::{JsonValue, parse_relaxed_json};
use terra_elevation::UnsupportedLayerPolicy;

#[derive(Debug, Clone, PartialEq)]
pub struct NetworkConfig {
    /// URL prefix to local directory; checked before any network access.
    pub mirrors: Vec<(String, PathBuf)>,
    /// Hosts whose `https://` URLs may be fetched over plain HTTP (no TLS transport is built in).
    pub plain_http_hosts: Vec<String>,
    pub cache_directory: Option<PathBuf>,
    pub cache_max_bytes: u64,
    pub max_per_host: usize,
    pub attempts: u32,
    pub connect_timeout: Duration,
    pub io_timeout: Duration,
    pub total_timeout: Duration,
    pub memory_cache_tiles: usize,
}

/// Settings for TerraMinusMinus' OpenStreetMap data (water, coastlines, roads, buildings).
#[derive(Debug, Clone, PartialEq)]
pub struct OpenStreetMapConfig {
    pub enabled: bool,
    /// URL prefixes of the GeoJSON tile servers, tried in order.
    pub servers: Vec<String>,
    /// Mapping file; `None` uses `osm.json5` beside the configuration, else the bundled default.
    pub mapping: Option<PathBuf>,
    pub memory_cache_tiles: usize,
}

impl Default for OpenStreetMapConfig {
    fn default() -> Self {
        let settings = terra_osm::OsmSettings::default();
        Self { enabled: true, servers: settings.servers, mapping: None, memory_cache_tiles: settings.tile_cache_entries }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermissionLevels {
    pub tpll: u8,
    /// `t+-.tpll.otherWorld`: run /tpll from a level that is not a TerraPlusMinus level.
    pub tpll_other_world: u8,
    pub tpll_ungenerated_chunks: u8,
    pub admin: u8,
    pub diagnostics: u8,
    /// `/where`, `/offset` and `/distortion`.
    pub info_commands: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServerConfig {
    /// Written in front of chat messages, as upstream's `prefix`.
    pub prefix: String,
    pub generator: GeneratorSettings,
    pub generate_trees: bool,
    pub world: WorldHeight,
    pub team_bounds: TeamBounds,
    pub linked_worlds: LinkedWorlds,
    pub heights_config: Option<PathBuf>,
    /// Tree-cover dataset config (`tree_cover.json5` schema); the upstream default when unset.
    pub tree_cover_config: Option<PathBuf>,
    pub unsupported_layers: UnsupportedLayerPolicy,
    pub network: NetworkConfig,
    pub openstreetmap: OpenStreetMapConfig,
    pub permissions: PermissionLevels,
    pub loader: LoaderLimits,
    pub generation_wait: Duration,
    /// Attempts at loading a chunk's data through transient failures before the chunk fails;
    /// 0 retries until the server stops. Permanent failures never retry.
    pub generation_retry_attempts: u32,
    pub max_pending_teleports: usize,
    /// Accepted but deprecated settings, worded as upstream's startup warnings. Callers log them.
    pub warnings: Vec<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            prefix: crate::messages::DEFAULT_PREFIX.to_owned(),
            generator: GeneratorSettings::default(),
            generate_trees: true,
            world: WorldHeight { min_y: -64, max_y: 320 },
            team_bounds: TeamBounds { min_latitude: 0.0, max_latitude: 0.0, min_longitude: 0.0, max_longitude: 0.0 },
            linked_worlds: LinkedWorlds { enabled: false, method: LinkedWorldsMethod::Levels, worlds: Vec::new() },
            heights_config: None,
            tree_cover_config: None,
            unsupported_layers: UnsupportedLayerPolicy::Reject,
            network: NetworkConfig {
                mirrors: Vec::new(),
                plain_http_hosts: Vec::new(),
                cache_directory: None,
                cache_max_bytes: 2 << 30,
                max_per_host: 8,
                attempts: 4,
                connect_timeout: Duration::from_secs(5),
                io_timeout: Duration::from_secs(10),
                total_timeout: Duration::from_secs(30),
                memory_cache_tiles: 512,
            },
            openstreetmap: OpenStreetMapConfig::default(),
            permissions: PermissionLevels { tpll: 0, tpll_other_world: 2, tpll_ungenerated_chunks: 2, admin: 2, diagnostics: 2, info_commands: 0 },
            loader: LoaderLimits::for_host(),
            generation_wait: Duration::from_secs(120),
            generation_retry_attempts: 8,
            max_pending_teleports: 64,
            warnings: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "configuration: {}", self.0)
    }
}

impl std::error::Error for ConfigError {}

struct Section<'a> {
    path: String,
    members: &'a [(String, JsonValue)],
}

impl<'a> Section<'a> {
    fn new(path: &str, value: &'a JsonValue, allowed: &[&str]) -> Result<Self, ConfigError> {
        let members = value.as_object().ok_or_else(|| ConfigError(format!("`{path}` must be an object")))?;
        if let Some((unknown, _)) = members.iter().find(|(key, _)| !allowed.contains(&key.as_str())) {
            return Err(ConfigError(format!("unknown key `{}{unknown}`", if path.is_empty() { String::new() } else { format!("{path}.") })));
        }
        Ok(Self { path: path.to_owned(), members })
    }

    fn get(&self, key: &str) -> Option<&'a JsonValue> {
        self.members.iter().find(|(name, _)| name == key).map(|(_, value)| value)
    }

    fn name(&self, key: &str) -> String {
        if self.path.is_empty() { key.to_owned() } else { format!("{}.{key}", self.path) }
    }

    fn number(&self, key: &str, default: f64) -> Result<f64, ConfigError> {
        match self.get(key) {
            None => Ok(default),
            Some(value) => value.as_f64().filter(|v| v.is_finite()).ok_or_else(|| ConfigError(format!("`{}` must be a finite number", self.name(key)))),
        }
    }

    fn integer(&self, key: &str, default: i64, range: std::ops::RangeInclusive<i64>) -> Result<i64, ConfigError> {
        let value = self.number(key, default as f64)?;
        if value.fract() != 0.0 || !range.contains(&(value as i64)) {
            return Err(ConfigError(format!("`{}` must be an integer in {range:?}", self.name(key))));
        }
        Ok(value as i64)
    }

    fn boolean(&self, key: &str, default: bool) -> Result<bool, ConfigError> {
        match self.get(key) {
            None => Ok(default),
            Some(value) => value.as_bool().ok_or_else(|| ConfigError(format!("`{}` must be true or false", self.name(key)))),
        }
    }

    fn string(&self, key: &str) -> Result<Option<&'a str>, ConfigError> {
        match self.get(key) {
            None | Some(JsonValue::Null) => Ok(None),
            Some(value) => value.as_str().map(Some).ok_or_else(|| ConfigError(format!("`{}` must be a string", self.name(key)))),
        }
    }

    fn section(&self, key: &str, allowed: &[&str]) -> Result<Option<Section<'a>>, ConfigError> {
        self.get(key).map(|value| Section::new(&self.name(key), value, allowed)).transpose()
    }
}

fn millis(value: i64) -> Duration {
    Duration::from_millis(value as u64)
}

pub fn parse_server_config(text: &str) -> Result<ServerConfig, ConfigError> {
    let document = parse_relaxed_json(text).map_err(|error| ConfigError(error.to_string()))?;
    let root = Section::new(
        "",
        &document,
        &[
            "terrain_offset", "min_latitude", "max_latitude", "min_longitude", "max_longitude", "linked_worlds", "generate_trees", "biomes",
            "surface_material", "world", "elevation", "network", "permissions", "generation", "config_version", "prefix", "openstreetmap",
            "building_outlines_material", "road_material", "path_material",
        ],
    )?;
    let mut config = ServerConfig::default();
    if let Some(offset) = root.section("terrain_offset", &["x", "y", "z"])? {
        let block_range = -30_000_000..=30_000_000;
        config.generator.x_offset = offset.integer("x", 0, block_range.clone())? as i32;
        config.generator.y_offset = offset.integer("y", 0, -4064..=4064)? as i32;
        config.generator.z_offset = offset.integer("z", 0, block_range)? as i32;
    }
    config.team_bounds = TeamBounds {
        min_latitude: root.number("min_latitude", 0.0)?,
        max_latitude: root.number("max_latitude", 0.0)?,
        min_longitude: root.number("min_longitude", 0.0)?,
        max_longitude: root.number("max_longitude", 0.0)?,
    };
    if let Some(linked) = root.section("linked_worlds", &["enabled", "method", "worlds"])? {
        config.linked_worlds.enabled = linked.boolean("enabled", false)?;
        config.linked_worlds.method = match linked.string("method")?.map(str::to_ascii_uppercase).as_deref() {
            None | Some("MULTIVERSE") | Some("LEVELS") => LinkedWorldsMethod::Levels,
            Some("SERVER") => LinkedWorldsMethod::Servers,
            Some(other) => return Err(ConfigError(format!("unknown linked_worlds.method `{other}`"))),
        };
        for (index, entry) in linked.get("worlds").map(JsonValue::as_array_lenient).unwrap_or_default().iter().enumerate() {
            let world = Section::new(&format!("linked_worlds.worlds[{index}]"), entry, &["name", "offset"])?;
            let name = world.string("name")?.ok_or_else(|| ConfigError(format!("linked_worlds.worlds[{index}].name is required")))?;
            config.linked_worlds.worlds.push(LinkedWorld { name: name.to_owned(), offset: world.integer("offset", 0, -100_000..=100_000)? as i32 });
        }
        if config.linked_worlds.enabled && config.linked_worlds.method == LinkedWorldsMethod::Servers {
            return Err(ConfigError("linked_worlds.method SERVER needs a proxy bridge, which Horizon does not provide".into()));
        }
    }
    if let Some(prefix) = root.string("prefix")? {
        config.prefix = prefix.to_owned();
    }
    config.generate_trees = root.boolean("generate_trees", true)?;
    if let Some(biomes) = root.section("biomes", &["use_dataset", "biome"])? {
        config.generator.use_biome_dataset = biomes.boolean("use_dataset", true)?;
        if let Some(name) = biomes.string("biome")? {
            config.generator.default_biome = Biome::from_resource_name(name).ok_or_else(|| ConfigError(format!("biomes.biome `{name}` is not a biome this generator places")))?;
        }
    }
    if let Some(material) = root.string("surface_material")? {
        crate::engine_bridge::validate_block_state(&material.trim().to_ascii_lowercase()).map_err(|reason| ConfigError(format!("surface_material: {reason}")))?;
        config.generator.surface_material = Block::from_material_name(material);
    }
    for (key, block) in [("building_outlines_material", "minecraft:bricks"), ("road_material", "minecraft:gray_concrete"), ("path_material", "minecraft:dirt_path")] {
        if let Some(material) = root.string(key)? {
            // `BlockMapper.withConfiguredMapping`: honored, with a deprecation warning.
            config.warnings.push(format!(
                "Configuration entry {key} is set to {material}, but is deprecated and may be removed in future versions. \
                 Consider removing it from your configuration and editing {} instead.",
                crate::stack::OSM_MAPPING_FILE
            ));
            crate::engine_bridge::validate_block_state(&material.trim().to_ascii_lowercase()).map_err(|reason| ConfigError(format!("{key}: {reason}")))?;
            config.generator.surface_replacements.push((block.to_owned(), Block::from_material_name(material)));
        }
    }
    if let Some(osm) = root.section("openstreetmap", &["enabled", "servers", "mapping", "memory_cache_tiles"])? {
        let target = &mut config.openstreetmap;
        target.enabled = osm.boolean("enabled", true)?;
        if let Some(servers) = osm.get("servers") {
            target.servers.clear();
            for server in servers.as_array_lenient() {
                let server = server.as_str().ok_or_else(|| ConfigError("openstreetmap.servers must be URL prefixes".into()))?;
                if !(server.starts_with("http://") || server.starts_with("https://")) || !server.ends_with('/') {
                    return Err(ConfigError(format!("openstreetmap.servers entry `{server}` must be an http(s) URL ending in `/`")));
                }
                target.servers.push(server.to_owned());
            }
            if target.servers.is_empty() && target.enabled {
                return Err(ConfigError("openstreetmap.servers is empty; set openstreetmap.enabled to false instead".into()));
            }
        }
        target.mapping = osm.string("mapping")?.map(PathBuf::from);
        target.memory_cache_tiles = osm.integer("memory_cache_tiles", target.memory_cache_tiles as i64, 1..=65_536)? as usize;
    }
    if let Some(world) = root.section("world", &["min_y", "height"])? {
        let min_y = world.integer("min_y", -64, -2032..=2016)? as i32;
        let height = world.integer("height", 384, 16..=4064)? as i32;
        if min_y % 16 != 0 || height % 16 != 0 || min_y + height > 2032 {
            return Err(ConfigError("world.min_y and world.height must be multiples of 16 with min_y + height <= 2032".into()));
        }
        config.world = WorldHeight { min_y, max_y: min_y + height };
    }
    if let Some(elevation) = root.section("elevation", &["heights_config", "tree_cover_config", "unsupported_layers"])? {
        config.heights_config = elevation.string("heights_config")?.map(PathBuf::from);
        config.tree_cover_config = elevation.string("tree_cover_config")?.map(PathBuf::from);
        config.unsupported_layers = match elevation.string("unsupported_layers")? {
            None | Some("reject") => UnsupportedLayerPolicy::Reject,
            Some("skip") => UnsupportedLayerPolicy::Skip,
            Some(other) => return Err(ConfigError(format!("elevation.unsupported_layers must be reject or skip, not `{other}`"))),
        };
    }
    if let Some(network) = root.section(
        "network",
        &["mirrors", "plain_http_hosts", "cache_directory", "cache_max_bytes", "max_per_host", "attempts", "connect_timeout_ms", "io_timeout_ms", "total_timeout_ms", "memory_cache_tiles"],
    )? {
        let target = &mut config.network;
        if let Some(mirrors) = network.get("mirrors") {
            let members = mirrors.as_object().ok_or_else(|| ConfigError("network.mirrors must map URL prefixes to directories".into()))?;
            for (prefix, directory) in members {
                let directory = directory.as_str().ok_or_else(|| ConfigError(format!("network.mirrors.{prefix} must be a path")))?;
                target.mirrors.push((prefix.clone(), PathBuf::from(directory)));
            }
        }
        if let Some(hosts) = network.get("plain_http_hosts") {
            for host in hosts.as_array_lenient() {
                target.plain_http_hosts.push(host.as_str().ok_or_else(|| ConfigError("network.plain_http_hosts must be strings".into()))?.to_ascii_lowercase());
            }
        }
        target.cache_directory = network.string("cache_directory")?.map(PathBuf::from);
        target.cache_max_bytes = network.integer("cache_max_bytes", target.cache_max_bytes as i64, 0..=(1 << 50))? as u64;
        target.max_per_host = network.integer("max_per_host", target.max_per_host as i64, 1..=64)? as usize;
        target.attempts = network.integer("attempts", i64::from(target.attempts), 1..=10)? as u32;
        target.connect_timeout = millis(network.integer("connect_timeout_ms", 5_000, 100..=120_000)?);
        target.io_timeout = millis(network.integer("io_timeout_ms", 10_000, 100..=300_000)?);
        target.total_timeout = millis(network.integer("total_timeout_ms", 30_000, 100..=600_000)?);
        target.memory_cache_tiles = network.integer("memory_cache_tiles", target.memory_cache_tiles as i64, 1..=65_536)? as usize;
    }
    if let Some(permissions) = root.section("permissions", &["tpll", "tpll_other_world", "tpll_ungenerated_chunks", "admin", "diagnostics", "info_commands"])? {
        let level = |key: &str, default: u8| permissions.integer(key, i64::from(default), 0..=4).map(|value| value as u8);
        config.permissions = PermissionLevels {
            tpll: level("tpll", 0)?,
            tpll_other_world: level("tpll_other_world", 2)?,
            info_commands: level("info_commands", 0)?,
            tpll_ungenerated_chunks: level("tpll_ungenerated_chunks", 2)?,
            admin: level("admin", 2)?,
            diagnostics: level("diagnostics", 2)?,
        };
    }
    if let Some(generation) = root.section("generation", &["workers", "queue_capacity", "ready_capacity", "wait_timeout_ms", "retry_attempts", "max_pending_teleports"])? {
        let defaults = config.loader;
        config.loader = LoaderLimits {
            workers: generation.integer("workers", defaults.workers as i64, 1..=64)? as usize,
            queue_capacity: generation.integer("queue_capacity", defaults.queue_capacity as i64, 1..=1 << 20)? as usize,
            ready_capacity: generation.integer("ready_capacity", defaults.ready_capacity as i64, 1..=1 << 20)? as usize,
        };
        config.generation_wait = millis(generation.integer("wait_timeout_ms", 120_000, 1_000..=3_600_000)?);
        config.max_pending_teleports = generation.integer("max_pending_teleports", 64, 1..=10_000)? as usize;
        config.generation_retry_attempts = generation.integer("retry_attempts", 8, 0..=1_000_000)? as u32;
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_keys_and_extensions_parse() {
        let config = parse_server_config(
            r#"{
                // keys from TerraPlusMinus config.yml
                "terrain_offset": {"x": 10, "y": -5, "z": -20},
                "min_latitude": 46.9, "max_latitude": 55.3, "min_longitude": 1.9, "max_longitude": 15.6,
                "linked_worlds": {"enabled": true, "method": "MULTIVERSE", "worlds": [{"name": "low", "offset": 0}, {"name": "high", "offset": -2032}]},
                "biomes": {"use_dataset": false, "biome": "minecraft:desert"},
                "surface_material": "GRASS_BLOCK",
                "world": {"min_y": -2032, "height": 4064},
                "network": {"plain_http_hosts": ["S3.amazonaws.com"], "mirrors": {"https://tiles.example/": "/srv/tiles"}},
                "config_version": 1.5,
            }"#,
        )
        .unwrap();
        assert_eq!((config.generator.x_offset, config.generator.y_offset, config.generator.z_offset), (10, -5, -20));
        assert_eq!(config.generator.default_biome, Biome::Desert);
        assert_eq!(config.world, WorldHeight { min_y: -2032, max_y: 2032 });
        assert_eq!(config.linked_worlds.worlds[1].offset, -2032);
        assert_eq!(config.network.plain_http_hosts, vec!["s3.amazonaws.com".to_owned()]);
        assert!(config.openstreetmap.enabled);
    }

    #[test]
    fn openstreetmap_settings_and_deprecated_materials() {
        let config = parse_server_config(
            r#"{"openstreetmap": {"enabled": false, "servers": ["http://osm.example/data/"], "mapping": "osm-custom.json5"}, "road_material": "STONE"}"#,
        )
        .unwrap();
        assert!(!config.openstreetmap.enabled);
        assert_eq!(config.openstreetmap.servers, vec!["http://osm.example/data/".to_owned()]);
        assert_eq!(config.openstreetmap.mapping, Some(PathBuf::from("osm-custom.json5")));
        assert_eq!(config.generator.surface_replacements, vec![("minecraft:gray_concrete".to_owned(), Block::Stone)]);
        assert_eq!(config.warnings.len(), 1);
        assert!(config.warnings[0].starts_with("Configuration entry road_material is set to STONE, but is deprecated"), "{:?}", config.warnings);
        assert!(parse_server_config("{}").unwrap().warnings.is_empty());
        for text in [r#"{"openstreetmap": {"servers": []}}"#, r#"{"openstreetmap": {"servers": ["ftp://x/"]}}"#, r#"{"openstreetmap": {"server": []}}"#] {
            assert!(parse_server_config(text).is_err(), "{text}");
        }
    }

    #[test]
    fn malformed_materials_are_configuration_errors_not_stone() {
        for (key, value) in [("surface_material", "grass block"), ("surface_material", "minecraft:oak_slab[type=top"), ("road_material", "minecraft:x[a=b,a=c]"), ("path_material", "")] {
            let error = parse_server_config(&format!(r#"{{"{key}": "{value}"}}"#)).unwrap_err();
            assert!(error.0.contains(key) && error.0.contains("malformed block state"), "{error:?}");
        }
        assert!(parse_server_config(r#"{"surface_material": "minecraft:oak_slab[type=top]"}"#).is_ok());
    }

    #[test]
    fn rejects_unknown_keys_and_invalid_values() {
        for text in [
            r#"{"terain_offset": {}}"#,
            r#"{"terrain_offset": {"w": 1}}"#,
            r#"{"world": {"min_y": -60}}"#,
            r#"{"world": {"min_y": 0, "height": 4064}}"#,
            r#"{"biomes": {"biome": "minecraft:the_void"}}"#,
            r#"{"linked_worlds": {"enabled": true, "method": "SERVER"}}"#,
            r#"{"permissions": {"tpll": 5}}"#,
            r#"{"terrain_offset": {"x": 1.5}}"#,
        ] {
            assert!(parse_server_config(text).is_err(), "{text}");
        }
    }
}
