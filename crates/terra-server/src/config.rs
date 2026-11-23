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
    /// Hosts whose `https://` URLs are fetched over plain HTTP instead of through curl.
    pub plain_http_hosts: Vec<String>,
    pub cache_directory: Option<PathBuf>,
    pub cache_max_bytes: u64,
    pub max_per_host: usize,
    pub attempts: u32,
    pub connect_timeout: Duration,
    pub io_timeout: Duration,
    pub total_timeout: Duration,
    pub memory_cache_tiles: usize,
    /// `network.https_command`: the program that fetches `https://` URLs (the system `curl` by
    /// default). `None` turns HTTPS off: only mirrors, plain HTTP and `plain_http_hosts` remain.
    pub https_command: Option<String>,
    /// TerraMinusMinus' `http.maxConcurrentRequests` (`"<n>: <url>"`), as (host, n).
    pub host_limits: Vec<(String, usize)>,
    /// TerraMinusMinus' `http.cacheTTL`: how long cached tiles and 404 answers stay fresh.
    /// `None` (0 minutes) keeps tiles until evicted.
    pub cache_ttl: Option<Duration>,
    /// Sent with every request; upstream's `<plugin>/<version> (<library>/<version>; +<site>)`.
    pub user_agent: String,
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
    /// `t+-.forcetpll`: `/tpll -p <players> <coordinates>`.
    pub forcetpll: u8,
    /// `t+-.autoteleport`: moved between linked levels when crossing their boundary.
    pub autoteleport: u8,
    /// `t+-.notify.update`: told on join when a newer release exists.
    pub notify_update: u8,
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
    /// `reduced_console_messages`: when false, every tile fetch is logged.
    pub reduced_console_messages: bool,
    /// `height_in_actionbar`: show players their real-world height above the hotbar.
    pub height_in_actionbar: bool,
    /// `passthrough_tpll`: another mod that owns `/tpll`; this one then registers neither
    /// `/tpll` nor `/tpc`.
    pub passthrough_tpll: Option<String>,
    /// Port extension, `linked_worlds.server_name`: this server's name among the linked servers
    /// (method SERVER). Upstream only learns it from the proxy's `GetServer` answer, which the
    /// mod still asks for; this is known from startup, so the first `/tpll` after a restart
    /// never finds the name missing. A proxy answer that differs wins and is logged.
    pub linked_server_name: Option<String>,
    /// Port extension, `updates`: upstream's `Updater` (release check and join notification).
    pub updates: crate::updates::UpdateSettings,
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
                https_command: Some("curl".to_owned()),
                host_limits: terra_acquisition::DEFAULT_HOST_LIMITS.iter().filter_map(|entry| terra_acquisition::parse_host_limit(entry)).collect(),
                cache_ttl: Some(terra_acquisition::DEFAULT_CACHE_TTL),
                user_agent: terra_acquisition::DEFAULT_USER_AGENT.to_owned(),
            },
            openstreetmap: OpenStreetMapConfig::default(),
            permissions: PermissionLevels { tpll: 0, tpll_other_world: 2, tpll_ungenerated_chunks: 2, admin: 2, diagnostics: 2, info_commands: 0, forcetpll: 2, autoteleport: 2, notify_update: 2 },
            loader: LoaderLimits::for_host(),
            generation_wait: Duration::from_secs(120),
            generation_retry_attempts: 8,
            max_pending_teleports: 64,
            reduced_console_messages: true,
            height_in_actionbar: false,
            passthrough_tpll: None,
            linked_server_name: None,
            updates: crate::updates::UpdateSettings::default(),
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

/// A level this mod generates: the earth dimension and, with linked levels, one per linked world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LevelPlan {
    /// Dimension id, `terraplusminus:<path>`.
    pub id: String,
    /// The dimension's path, as registered by the mod.
    pub path: String,
    pub y_offset: i32,
}

/// The mod's namespace and the dimension it always registers.
pub const MOD_NAMESPACE: &str = "terraplusminus";
pub const EARTH_PATH: &str = "earth";

impl ServerConfig {
    /// Every level to register: `terraplusminus:earth`, then each other linked level
    /// (`terraplusminus:<path>`), each at [`ServerConfig::level_offset`]. Validated by
    /// [`parse_server_config`].
    pub fn levels(&self) -> Vec<LevelPlan> {
        let earth = format!("{MOD_NAMESPACE}:{EARTH_PATH}");
        let mut levels = vec![LevelPlan { y_offset: self.level_offset(&earth), id: earth.clone(), path: EARTH_PATH.to_owned() }];
        if self.linked_worlds.levels_active() {
            for world in &self.linked_worlds.worlds {
                let id = world.name.to_ascii_lowercase();
                if id == earth {
                    continue;
                }
                if let Some(path) = id.strip_prefix(MOD_NAMESPACE).and_then(|rest| rest.strip_prefix(':')) {
                    levels.push(LevelPlan { y_offset: self.level_offset(&id), path: path.to_owned(), id });
                }
            }
        }
        levels
    }

    /// `PlayerMoveEvent`'s offset for the height in the action bar: the listed offset of the
    /// player's level while linked levels are active (0 included), else `terrain_offset.y`.
    pub fn actionbar_offset(&self, level: Option<&str>) -> i32 {
        level.and_then(|level| self.linked_worlds.offset_of(level)).unwrap_or(self.generator.y_offset)
    }

    /// The Y offset a level of this mod is generated at, as upstream's `getDefaultWorldGenerator`
    /// and `RealWorldGenerator` choose it: the linked offset while linked levels are active,
    /// where 0 (and an unlisted level) means `terrain_offset.y`.
    pub fn level_offset(&self, level: &str) -> i32 {
        match self.linked_worlds.offset_of(level) {
            Some(offset) if offset != 0 => offset,
            _ => self.generator.y_offset,
        }
    }
}

/// Linked levels must be dimensions this mod generates, so their terrain lines up (upstream
/// warns when a linked world uses another generator). Names become dimension ids, lower case.
fn validate_linked_levels(config: &mut ServerConfig) -> Result<(), ConfigError> {
    if !config.linked_worlds.levels_active() {
        return Ok(());
    }
    let earth = format!("{MOD_NAMESPACE}:{EARTH_PATH}");
    for world in &mut config.linked_worlds.worlds {
        world.name.make_ascii_lowercase();
        let id = world.name.clone();
        if id == earth {
            continue;
        }
        let path = id.strip_prefix(MOD_NAMESPACE).and_then(|rest| rest.strip_prefix(':')).unwrap_or("");
        let valid = !path.is_empty() && path.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.' | '/'));
        if !valid {
            return Err(ConfigError(format!(
                "linked world `{}` must name a level of this mod, `{MOD_NAMESPACE}:<name>` (lower case letters, digits, `_`, `-`, `.`, `/`), which is registered at its offset",
                world.name
            )));
        }
    }
    Ok(())
}

/// The overworld range of upstream's `world-height-datapack.zip`.
pub const DATAPACK_WORLD: WorldHeight = WorldHeight { min_y: -64, max_y: 2032 };

/// `ConfigurationHelper.PLACEHOLDER_NAMES`: sample linked-world names, skipped.
const LINKED_WORLD_PLACEHOLDERS: [&str; 3] = ["world/server", "another_world/server", "current_world/server"];

pub fn parse_server_config(text: &str) -> Result<ServerConfig, ConfigError> {
    let document = parse_relaxed_json(text).map_err(|error| ConfigError(error.to_string()))?;
    let root = Section::new(
        "",
        &document,
        &[
            "terrain_offset", "min_latitude", "max_latitude", "min_longitude", "max_longitude", "linked_worlds", "generate_trees", "biomes",
            "surface_material", "world", "elevation", "network", "permissions", "generation", "config_version", "prefix", "openstreetmap",
            "building_outlines_material", "road_material", "path_material", "reduced_console_messages", "height_datapack", "height_in_actionbar",
            "passthrough_tpll", "updates",
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
    if let Some(linked) = root.section("linked_worlds", &["enabled", "method", "worlds", "server_name"])? {
        config.linked_worlds.enabled = linked.boolean("enabled", false)?;
        config.linked_worlds.method = match linked.string("method")?.map(str::to_ascii_uppercase).as_deref() {
            None | Some("MULTIVERSE") | Some("LEVELS") => LinkedWorldsMethod::Levels,
            Some("SERVER") => LinkedWorldsMethod::Servers,
            Some(other) => return Err(ConfigError(format!("unknown linked_worlds.method `{other}`"))),
        };
        for (index, entry) in linked.get("worlds").map(JsonValue::as_array_lenient).unwrap_or_default().iter().enumerate() {
            let world = Section::new(&format!("linked_worlds.worlds[{index}]"), entry, &["name", "offset"])?;
            let name = world.string("name")?.ok_or_else(|| ConfigError(format!("linked_worlds.worlds[{index}].name is required")))?;
            let offset = world.integer("offset", 0, -100_000..=100_000)? as i32;
            // `ConfigurationHelper.convertList`: the sample entries of the default config.yml are
            // placeholders, never worlds.
            if LINKED_WORLD_PLACEHOLDERS.contains(&name.to_ascii_lowercase().as_str()) {
                continue;
            }
            if config.linked_worlds.worlds.iter().any(|known| known.name.eq_ignore_ascii_case(name)) {
                return Err(ConfigError(format!("linked_worlds.worlds lists `{name}` more than once")));
            }
            config.linked_worlds.worlds.push(LinkedWorld { name: name.to_owned(), offset });
        }
        if config.linked_worlds.enabled {
            if let Some(pair) = config.linked_worlds.worlds.windows(2).find(|pair| pair[0].offset <= pair[1].offset) {
                return Err(ConfigError(format!(
                    "linked_worlds.worlds must go from the lowest to the highest level with strictly decreasing offsets, but `{}` (offset {}) comes before `{}` (offset {})",
                    pair[0].name, pair[0].offset, pair[1].name, pair[1].offset
                )));
            }
            if config.linked_worlds.worlds.len() < 2 {
                config.warnings.push(format!(
                    "Linked worlds are enabled but only {} world(s) are configured. At least two worlds are required for transitions to happen.",
                    config.linked_worlds.worlds.len()
                ));
            }
        }
        config.linked_server_name = linked.string("server_name")?.map(str::trim).filter(|name| !name.is_empty()).map(str::to_owned);
        if config.linked_worlds.enabled && config.linked_worlds.method == LinkedWorldsMethod::Servers {
            if let Some(name) = &config.linked_server_name
                && config.linked_worlds.index_of(name).is_none()
            {
                return Err(ConfigError(format!("linked_worlds.server_name `{name}` is not one of linked_worlds.worlds")));
            }
            config.warnings.push(
                "Linked worlds use the SERVER method: TerraPlusMinusProxyBridge must run on the proxy, \
                 which moves players between servers over the terraplusminus:teleportbridge channel."
                    .to_owned(),
            );
        }
    }
    if let Some(prefix) = root.string("prefix")? {
        config.prefix = prefix.to_owned();
    }
    config.reduced_console_messages = root.boolean("reduced_console_messages", true)?;
    config.height_in_actionbar = root.boolean("height_in_actionbar", false)?;
    config.passthrough_tpll = root.string("passthrough_tpll")?.map(str::trim).filter(|name| !name.is_empty()).map(str::to_owned);
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
    // `height_datapack`: upstream installs `world-height-datapack.zip`, whose overworld is
    // y -64 to 2031 (min_y -64, height 2096). Here the dimension is registered with that range.
    let height_datapack = root.boolean("height_datapack", false)?;
    if height_datapack {
        config.world = DATAPACK_WORLD;
    }
    if let Some(world) = root.section("world", &["min_y", "height"])? {
        let min_y = world.integer("min_y", -64, -2032..=2016)? as i32;
        let height = world.integer("height", 384, 16..=4064)? as i32;
        if min_y % 16 != 0 || height % 16 != 0 || min_y + height > 2032 {
            return Err(ConfigError("world.min_y and world.height must be multiples of 16 with min_y + height <= 2032".into()));
        }
        let world = WorldHeight { min_y, max_y: min_y + height };
        if height_datapack && world != DATAPACK_WORLD {
            return Err(ConfigError(format!(
                "height_datapack is true, which means min_y {} and height {}, but world sets min_y {min_y} and height {height}; remove one of them",
                DATAPACK_WORLD.min_y,
                DATAPACK_WORLD.max_y - DATAPACK_WORLD.min_y
            )));
        }
        config.world = world;
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
        &["mirrors", "plain_http_hosts", "cache_directory", "cache_max_bytes", "max_per_host", "attempts", "connect_timeout_ms", "io_timeout_ms", "total_timeout_ms", "memory_cache_tiles",
          "https_command", "max_concurrent_requests", "cache_ttl_minutes", "user_agent"],
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
        if let Some(command) = network.string("https_command")? {
            let command = command.trim();
            target.https_command = match command {
                "" | "off" | "none" => None,
                _ if command.chars().any(|c| c.is_control()) => return Err(ConfigError("network.https_command must be a program name or path".into())),
                _ => Some(command.to_owned()),
            };
        }
        if let Some(entries) = network.get("max_concurrent_requests") {
            target.host_limits.clear();
            for entry in entries.as_array_lenient() {
                let text = entry.as_str().ok_or_else(|| ConfigError("network.max_concurrent_requests must be strings like \"8: https://host/\"".into()))?;
                let limit = terra_acquisition::parse_host_limit(text)
                    .filter(|(_, limit)| *limit <= 64)
                    .ok_or_else(|| ConfigError(format!("network.max_concurrent_requests entry `{text}` must read `<1-64>: <url>`")))?;
                target.host_limits.push(limit);
            }
        }
        let ttl = network.integer("cache_ttl_minutes", 1440, 0..=525_600)?;
        target.cache_ttl = (ttl > 0).then(|| Duration::from_secs(ttl as u64 * 60));
        if let Some(agent) = network.string("user_agent")? {
            let agent = agent.trim();
            if agent.is_empty() || agent.chars().any(|c| c.is_control()) {
                return Err(ConfigError("network.user_agent must be printable text".into()));
            }
            target.user_agent = agent.to_owned();
        }
    }
    if let Some(permissions) =
        root.section("permissions", &["tpll", "tpll_other_world", "tpll_ungenerated_chunks", "admin", "diagnostics", "info_commands", "forcetpll", "autoteleport", "notify_update"])?
    {
        let level = |key: &str, default: u8| permissions.integer(key, i64::from(default), 0..=4).map(|value| value as u8);
        config.permissions = PermissionLevels {
            tpll: level("tpll", 0)?,
            tpll_other_world: level("tpll_other_world", 2)?,
            info_commands: level("info_commands", 0)?,
            tpll_ungenerated_chunks: level("tpll_ungenerated_chunks", 2)?,
            admin: level("admin", 2)?,
            diagnostics: level("diagnostics", 2)?,
            forcetpll: level("forcetpll", 2)?,
            autoteleport: level("autoteleport", 2)?,
            notify_update: level("notify_update", 2)?,
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
    if let Some(updates) = root.section("updates", &["enabled", "repository", "url"])? {
        config.updates.enabled = updates.boolean("enabled", true)?;
        if let Some(repository) = updates.string("repository")? {
            if !crate::updates::valid_repository(repository) {
                return Err(ConfigError(format!("updates.repository `{repository}` must be a GitHub `owner/name`")));
            }
            config.updates.repository = repository.to_owned();
        }
        if let Some(url) = updates.string("url")? {
            if !["http://", "https://", "file://"].iter().any(|scheme| url.starts_with(scheme)) || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err(ConfigError(format!("updates.url `{url}` must be an http://, https:// or file:// URL")));
            }
            config.updates.url = Some(url.to_owned());
        }
    }
    validate_linked_levels(&mut config)?;
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
                "linked_worlds": {"enabled": true, "method": "MULTIVERSE", "worlds": [{"name": "terraplusminus:earth", "offset": -5}, {"name": "terraplusminus:high", "offset": -2037}]},
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
        assert_eq!(config.linked_worlds.worlds[1].offset, -2037);
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
    fn network_parity_keys_and_updates() {
        let defaults = ServerConfig::default();
        assert_eq!(defaults.network.https_command.as_deref(), Some("curl"));
        assert_eq!(defaults.network.cache_ttl, Some(Duration::from_secs(1440 * 60)));
        assert!(defaults.network.host_limits.contains(&("overpass.kumi.systems".to_owned(), 1)));
        assert!(defaults.network.host_limits.contains(&("s3.amazonaws.com".to_owned(), 8)));
        assert!(defaults.network.user_agent.starts_with("Terraplusminus/"));
        assert_eq!(defaults.permissions.notify_update, 2);
        assert!(defaults.updates.enabled);
        let config = parse_server_config(
            r#"{"network": {"https_command": "off", "max_concurrent_requests": ["2: https://Tiles.example/"], "cache_ttl_minutes": 0, "user_agent": "Mine/1"},
                "updates": {"enabled": false, "repository": "BTE-Germany/TerraPlusMinus", "url": "file:///srv/latest.json"},
                "permissions": {"notify_update": 4}}"#,
        )
        .unwrap();
        assert_eq!(config.network.https_command, None);
        assert_eq!(config.network.host_limits, vec![("tiles.example".to_owned(), 2)]);
        assert_eq!(config.network.cache_ttl, None);
        assert_eq!(config.network.user_agent, "Mine/1");
        assert!(!config.updates.enabled);
        assert_eq!(config.updates.release_url(), "file:///srv/latest.json");
        assert_eq!(config.permissions.notify_update, 4);
        for bad in [
            r#"{"network": {"max_concurrent_requests": ["eight: https://a/"]}}"#,
            r#"{"network": {"max_concurrent_requests": ["65: https://a/"]}}"#,
            r#"{"network": {"cache_ttl_minutes": -1}}"#,
            r#"{"network": {"user_agent": ""}}"#,
            r#"{"updates": {"repository": "not a repo"}}"#,
            r#"{"updates": {"url": "ftp://x"}}"#,
            r#"{"updates": {"channel": "beta"}}"#,
        ] {
            assert!(parse_server_config(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn rejects_unknown_keys_and_invalid_values() {
        for text in [
            r#"{"terain_offset": {}}"#,
            r#"{"terrain_offset": {"w": 1}}"#,
            r#"{"world": {"min_y": -60}}"#,
            r#"{"world": {"min_y": 0, "height": 4064}}"#,
            r#"{"biomes": {"biome": "minecraft:the_void"}}"#,
            r#"{"linked_worlds": {"enabled": true, "method": "SERVER", "server_name": "nowhere", "worlds": [{"name": "a", "offset": 0}]}}"#,
            r#"{"linked_worlds": {"enabled": true, "method": "SERVER", "server_name": 3}}"#,
            r#"{"permissions": {"tpll": 5}}"#,
            r#"{"terrain_offset": {"x": 1.5}}"#,
            r#"{"passthrough_tpll": 3}"#,
            r#"{"height_in_actionbar": "yes"}"#,
            r#"{"permissions": {"forcetpll": 9}}"#,
        ] {
            assert!(parse_server_config(text).is_err(), "{text}");
        }
    }

    #[test]
    fn upstream_switches_parse_with_upstream_defaults() {
        let defaults = parse_server_config("{}").unwrap();
        assert!(defaults.reduced_console_messages, "config.yml ships reduced_console_messages: true");
        assert!(!defaults.height_in_actionbar);
        assert_eq!(defaults.passthrough_tpll, None);
        assert_eq!(defaults.linked_server_name, None);
        let servers = parse_server_config(
            r#"{"linked_worlds": {"enabled": true, "method": "SERVER", "server_name": " Mid ", "worlds": [{"name": "low", "offset": 2032}, {"name": "mid", "offset": 0}]}}"#,
        )
        .unwrap();
        assert_eq!(servers.linked_server_name.as_deref(), Some("Mid"));
        assert_eq!(servers.linked_worlds.method, LinkedWorldsMethod::Servers);
        assert!(servers.warnings.iter().any(|warning| warning.contains("TerraPlusMinusProxyBridge")));
        assert!(!servers.linked_worlds.levels_active());
        assert_eq!((defaults.permissions.forcetpll, defaults.permissions.autoteleport), (2, 2));
        let config = parse_server_config(
            r#"{"reduced_console_messages": false, "height_in_actionbar": true, "passthrough_tpll": " terraplusplus ", "permissions": {"forcetpll": 3, "autoteleport": 0}}"#,
        )
        .unwrap();
        assert!(!config.reduced_console_messages && config.height_in_actionbar);
        assert_eq!(config.passthrough_tpll.as_deref(), Some("terraplusplus"));
        assert_eq!((config.permissions.forcetpll, config.permissions.autoteleport), (3, 0));
        // An empty namespace is upstream's "disabled".
        assert_eq!(parse_server_config(r#"{"passthrough_tpll": "  "}"#).unwrap().passthrough_tpll, None);
    }

    #[test]
    fn height_datapack_selects_the_datapack_overworld() {
        let config = parse_server_config(r#"{"height_datapack": true}"#).unwrap();
        assert_eq!(config.world, DATAPACK_WORLD);
        assert_eq!(DATAPACK_WORLD, WorldHeight { min_y: -64, max_y: 2032 });
        assert_eq!(parse_server_config(r#"{"height_datapack": false}"#).unwrap().world, ServerConfig::default().world);
        assert!(parse_server_config(r#"{"height_datapack": true, "world": {"min_y": -64, "height": 2096}}"#).is_ok());
        let error = parse_server_config(r#"{"height_datapack": true, "world": {"min_y": -2032, "height": 4064}}"#).unwrap_err();
        assert!(error.0.contains("height_datapack"), "{error:?}");
    }

    fn linked(worlds: &str) -> Result<ServerConfig, ConfigError> {
        parse_server_config(&format!(r#"{{"linked_worlds": {{"enabled": true, "method": "MULTIVERSE", "worlds": [{worlds}]}}}}"#))
    }

    #[test]
    fn linked_worlds_follow_upstream_validation() {
        // ConfigurationHelper drops its sample entries.
        let config = linked(r#"{"name": "world/server", "offset": 0}, {"name": "terraplusminus:earth", "offset": 0}, {"name": "Another_World/Server", "offset": -2032}, {"name": "terraplusminus:sky", "offset": -2032}"#).unwrap();
        assert_eq!(config.linked_worlds.worlds.iter().map(|world| world.name.as_str()).collect::<Vec<_>>(), ["terraplusminus:earth", "terraplusminus:sky"]);
        assert!(config.warnings.is_empty(), "{:?}", config.warnings);
        // LinkedWorldLayout.validate: duplicate names (without case) and offsets that do not
        // strictly decrease are errors; fewer than two worlds is a warning.
        for worlds in [
            r#"{"name": "terraplusminus:earth", "offset": 0}, {"name": "TerraPlusMinus:Earth", "offset": -2032}"#,
            r#"{"name": "terraplusminus:earth", "offset": 0}, {"name": "terraplusminus:sky", "offset": 0}"#,
            r#"{"name": "terraplusminus:earth", "offset": 0}, {"name": "terraplusminus:sky", "offset": 2032}"#,
        ] {
            assert!(linked(worlds).is_err(), "{worlds}");
        }
        let lonely = linked(r#"{"name": "terraplusminus:earth", "offset": 0}"#).unwrap();
        assert_eq!(lonely.warnings.len(), 1, "{:?}", lonely.warnings);
        // A disabled list is only parsed: its order is not enforced.
        assert!(parse_server_config(r#"{"linked_worlds": {"enabled": false, "worlds": [{"name": "a", "offset": 0}, {"name": "b", "offset": 5}]}}"#).is_ok());
    }

    #[test]
    fn linked_levels_are_dimensions_of_this_mod() {
        let config = linked(r#"{"name": "TerraPlusMinus:Earth", "offset": 0}, {"name": "terraplusminus:sky", "offset": -2032}, {"name": "terraplusminus:space/high", "offset": -4064}"#).unwrap();
        let levels: Vec<(String, String, i32)> = config.levels().into_iter().map(|level| (level.id, level.path, level.y_offset)).collect();
        assert_eq!(
            levels,
            [
                ("terraplusminus:earth".to_owned(), "earth".to_owned(), 0),
                ("terraplusminus:sky".to_owned(), "sky".to_owned(), -2032),
                ("terraplusminus:space/high".to_owned(), "space/high".to_owned(), -4064)
            ]
        );
        assert_eq!(config.level_offset("terraplusminus:SKY"), -2032);
        assert_eq!(config.level_offset("terraplusminus:earth"), 0);
        assert_eq!(config.level_offset("minecraft:overworld"), 0);
        assert_eq!(parse_server_config("{}").unwrap().levels().len(), 1);
        // RealWorldGenerator: offset 0 means terrain_offset.y; any other offset is used as is,
        // for the earth level too.
        let shifted = parse_server_config(
            r#"{"terrain_offset": {"y": -5}, "linked_worlds": {"enabled": true, "worlds": [{"name": "terraplusminus:earth", "offset": 0}, {"name": "terraplusminus:sky", "offset": -2032}]}}"#,
        )
        .unwrap();
        assert_eq!(shifted.levels().iter().map(|level| level.y_offset).collect::<Vec<_>>(), [-5, -2032]);
        let raised = parse_server_config(r#"{"linked_worlds": {"enabled": true, "worlds": [{"name": "terraplusminus:earth", "offset": 2032}, {"name": "terraplusminus:low", "offset": 0}]}}"#).unwrap();
        assert_eq!(raised.levels().iter().map(|level| level.y_offset).collect::<Vec<_>>(), [2032, 0]);
        assert_eq!(parse_server_config(r#"{"terrain_offset": {"y": 7}}"#).unwrap().levels()[0].y_offset, 7);
        // The action bar uses the listed offset as is, 0 included.
        assert_eq!(raised.actionbar_offset(Some("terraplusminus:low")), 0);
        assert_eq!(shifted.actionbar_offset(Some("terraplusminus:earth")), 0);
        assert_eq!(shifted.actionbar_offset(Some("minecraft:overworld")), -5);
        assert_eq!(shifted.actionbar_offset(None), -5);
        for worlds in [
            r#"{"name": "minecraft:overworld", "offset": 0}, {"name": "terraplusminus:sky", "offset": -2032}"#,
            r#"{"name": "terraplusminus:", "offset": 0}, {"name": "terraplusminus:sky", "offset": -2032}"#,
            r#"{"name": "terraplusminus:a b", "offset": 0}, {"name": "terraplusminus:sky", "offset": -2032}"#,
        ] {
            assert!(linked(worlds).is_err(), "{worlds}");
        }
    }
}
