//! TerraPlusMinus on Horizon: a chunk generator and biome source over the terra crates, the
//! `terraplusminus:earth` dimension (and one `terraplusminus:<name>` dimension per linked level,
//! at its offset), `/tpll` and `/tpll -p <players>` (`/tpc`), `/where`, `/offset`,
//! `/distortion`, `/terraplusminus status` and the height in the action bar.
//!
//! Generation runs on Horizon's worldgen workers and waits there for real elevation data; the
//! server tick only polls finished work. The world's generation fingerprint is checked when the
//! earth level opens (`level_load`, before any of its chunks), and generation stays closed if it
//! does not match.
//!
//! A chunk that cannot be generated (refused fingerprint, permanent data error, retries
//! exhausted, server stopping, unknown block state) fails: `build_terrain` panics. Horizon's
//! worldgen job runner catches the panic (`horizon-threads` `jobs.rs`), the chunk's job delivers
//! nothing, and the chunk is neither published nor saved, so a later run generates it properly.
//! Returning normally would publish whatever the chunk holds, and an engine-side terrain error
//! is turned into an empty chunk, so neither is used.
#![deny(unsafe_code)]

use mod_api::{
    BiomeSource, BlockKey, BlockState, ChunkAccess, ChunkGenerator, Command, CommandArgumentValue, CommandSource, DimensionSettings, EquipmentSlot, Flow, Host, Hooks,
    InstanceDirectory, LevelLoadEvent, PlayerRef, Registrar, RegisterCommandsEvent, RegistrationContext, RunModCommandEvent, ServerHost, ServerStartedEvent,
    ServerStoppingEvent, ServerTickEvent, ServerTickPhase, StateConstraint, Text, TextClick, TextHover, TextStyle,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use terra_chunk_generation::WorldHeight;
use terra_server::{
    Authorization, BiomeLookup, BlockPalette, BlockWriter, GateState, GenerationControl, GenerationFailure, LevelHeight, LevelPlan, PendingTeleports, RegionIndex,
    ServerConfig, TeleportOutcome, TeleportWorld, TerraStack, TpllRequest, TpllStep, extract_default_osm_mapping, messages, messages::{ChatMessage, legacy_spans}, parse_server_config,
    split_block_state, terrain_for_generation, write_chunk_at, write_trees, TreeTarget,
};

const MOD_ID: &str = terra_server::MOD_NAMESPACE;
const LEVEL_ID: &str = "terraplusminus:earth";
const CONFIG_FILE: &str = "config/terraplusminus/terraplusminus.json5";
/// `PlayerMoveEvent.startKeepActionBarAlive`: the action bar is resent every 20 ticks.
const ACTIONBAR_PERIOD_TICKS: u64 = 20;
const DEBUG_STICK: &str = "minecraft:debug_stick";

struct Shared {
    stack: TerraStack,
    control: GenerationControl,
}

fn log(message: &str) {
    eprintln!("[{MOD_ID}] {message}");
}

fn instance_path(directory: &InstanceDirectory) -> Option<PathBuf> {
    match directory {
        #[cfg(unix)]
        InstanceDirectory::Unix(bytes) => Some(PathBuf::from(<std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(bytes))),
        #[cfg(windows)]
        InstanceDirectory::Windows(units) => Some(PathBuf::from(<std::ffi::OsString as std::os::windows::ffi::OsStringExt>::from_wide(units))),
        _ => None,
    }
}

fn load_shared(context: &RegistrationContext) -> Result<Shared, String> {
    let instance = context.instance_dir().ok().and_then(instance_path).ok_or("the instance directory is unavailable")?;
    let path = instance.join(CONFIG_FILE);
    let config = match std::fs::read_to_string(&path) {
        Ok(text) => parse_server_config(&text).map_err(|error| format!("{}: {error}", path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            log(&format!("no {CONFIG_FILE}; using the upstream defaults (AWS terrain tiles and the OpenStreetMap server need network.plain_http_hosts or a mirror)"));
            ServerConfig::default()
        }
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    for warning in &config.warnings {
        log(warning);
    }
    let base = path.parent().map_or(instance.clone(), PathBuf::from);
    if config.openstreetmap.enabled && config.openstreetmap.mapping.is_none() {
        // TerraPlusMinus drops the default mapping beside its config so it can be edited.
        match extract_default_osm_mapping(&base) {
            Ok(true) => log(&format!("created the default OpenStreetMap mapping at {}", base.join(terra_server::OSM_MAPPING_FILE).display())),
            Ok(false) => {}
            Err(error) => log(&format!("could not write the default OpenStreetMap mapping into {}: {error}", base.display())),
        }
    }
    let stack = TerraStack::build(config, &base).map_err(|error| error.to_string())?;
    Ok(Shared { stack, control: GenerationControl::default() })
}

struct EarthGenerator {
    shared: Arc<Shared>,
    /// Every block the generator can place, by block state string, resolved at registration.
    blocks: Arc<Vec<RegisteredBlock>>,
    /// The level's Y offset: `terrain_offset.y`, or its linked offset.
    y_offset: i32,
}

struct RegisteredBlock {
    name: String,
    key: BlockKey,
    properties: Vec<(String, String)>,
}

/// Writes into the engine chunk and counts states the engine refused.
struct AccessWriter<'a> {
    chunk: &'a mut dyn ChunkAccess,
    refused: usize,
}

impl BlockWriter<BlockState> for AccessWriter<'_> {
    fn set_block(&mut self, position: [i32; 3], state: BlockState) {
        if !self.chunk.set_block(position, state) {
            self.refused += 1;
        }
    }
}

/// Ends a chunk that must not be completed. The engine's terrain callback has no failure
/// channel: returning would publish the chunk as generated, and the engine turns terrain errors
/// into empty chunks. Its worldgen job runner catches this panic, so only this chunk's job is
/// lost; the chunk is not published or saved.
#[allow(clippy::panic)] // Deliberate fail-closed; see above and the crate documentation.
fn fail_chunk(chunk_x: i32, chunk_z: i32, failure: &GenerationFailure) -> ! {
    panic!("[{MOD_ID}] chunk ({chunk_x}, {chunk_z}) was not generated: {failure}")
}

impl EarthGenerator {
    /// Every registered block resolved strictly in the engine's registry: an unknown block,
    /// property or value fails, it is never replaced.
    fn resolve_blocks(&self, chunk: &dyn ChunkAccess) -> Result<Vec<(String, BlockState)>, GenerationFailure> {
        self.blocks
            .iter()
            .map(|block| {
                let constraints: Vec<StateConstraint> = block.properties.iter().map(|(key, value)| StateConstraint::new(key.as_str(), value.as_str())).collect();
                BlockState::of(chunk.metadata(), block.key, &constraints)
                    .map(|state| (block.name.clone(), state))
                    .map_err(|error| GenerationFailure::Permanent(format!("block state `{}` does not exist in this game ({error:?})", block.name)))
            })
            .collect()
    }

    fn generate(&self, chunk: &mut dyn ChunkAccess) -> Result<(), GenerationFailure> {
        let shared = &self.shared;
        let position = chunk.pos();
        let wait = shared.stack.config().generation_wait;
        shared.control.gate.wait_open(Duration::from_secs(30), wait, &mut |_| log(&format!("chunk ({}, {}) waits for the generation fingerprint check", position.x, position.z)))?;
        let resolved = self.resolve_blocks(&*chunk)?;
        let terrain = terrain_for_generation(&shared.stack, &shared.control, position.x, position.z, &mut |message| log(&message))?;
        let world = WorldHeight { min_y: chunk.min_y(), max_y: chunk.min_y() + chunk.height() };
        // The generator only places registered blocks; a miss is recorded and fails the chunk
        // instead of placing something else.
        let missing: Arc<Mutex<Vec<String>>> = Arc::default();
        let Some(&(_, any_state)) = resolved.first() else { return Err(GenerationFailure::Internal("no blocks registered".into())) };
        let misses = Arc::clone(&missing);
        let mut palette = BlockPalette::new(move |name: &str| match resolved.iter().find(|(known, _)| known == name) {
            Some((_, state)) => *state,
            None => {
                if let Ok(mut misses) = misses.lock() {
                    misses.push(name.to_owned());
                }
                any_state
            }
        });
        let level_seed = chunk.seed();
        let mut writer = AccessWriter { chunk, refused: 0 };
        let target = TreeTarget { chunk_x: position.x, chunk_z: position.z, world, level_seed, y_offset: self.y_offset };
        write_chunk_at(&shared.stack, &terrain, target, &mut palette, &mut writer);
        // Trees come after the terrain and may overwrite it, as upstream's populator does.
        write_trees(&shared.stack, &shared.control, &terrain, target, &mut palette, &mut writer, &mut |message| log(&message))?;
        let missing = missing.lock().map_err(|_| GenerationFailure::Internal("palette lock poisoned".into()))?;
        if !missing.is_empty() {
            return Err(GenerationFailure::Internal(format!("blocks without a registered state: {}", missing.join(", "))));
        }
        if writer.refused > 0 {
            return Err(GenerationFailure::Internal(format!("the engine refused {} block writes", writer.refused)));
        }
        Ok(())
    }
}

impl ChunkGenerator for EarthGenerator {
    fn build_terrain(&self, chunk: &mut dyn ChunkAccess) {
        let position = chunk.pos();
        let counters = &self.shared.control.counters;
        match self.generate(chunk) {
            Ok(()) => {
                counters.chunks_written.fetch_add(1, Ordering::Relaxed);
            }
            Err(failure) => {
                counters.chunks_failed.fetch_add(1, Ordering::Relaxed);
                if failure != GenerationFailure::Cancelled {
                    log(&format!("chunk ({}, {}) not generated: {failure}", position.x, position.z));
                }
                fail_chunk(position.x, position.z, &failure);
            }
        }
    }
}

struct EarthBiomes {
    shared: Arc<Shared>,
    lookup: BiomeLookup,
}

impl BiomeSource for EarthBiomes {
    fn possible_biomes(&self) -> Vec<String> {
        BiomeLookup::possible_biomes()
    }

    fn noise_biome(&self, [quart_x, _, quart_z]: [i32; 3], _seed: i64) -> u16 {
        self.lookup.biome_index(&self.shared.stack, quart_x, quart_z)
    }
}

/// The saved-chunk index of each level of this mod, by level id.
type LevelRegions = Vec<(String, RegionIndex)>;

struct HostWorld<'a> {
    host: &'a dyn Host,
    regions: &'a LevelRegions,
    air: &'a [BlockKey],
}

impl TeleportWorld for HostWorld<'_> {
    fn level_height(&self, world: &str) -> Option<LevelHeight> {
        let level = self.host.level(world)?;
        let (min_y, height) = level.height(self.host)?;
        Some(LevelHeight { min_y, max_y: min_y + height })
    }

    fn is_chunk_generated(&self, world: &str, chunk_x: i32, chunk_z: i32) -> bool {
        let loaded = self.host.level(world).and_then(|level| self.host.chunk(level, [chunk_x, chunk_z])).is_some();
        loaded || self.regions.iter().any(|(level, regions)| level == world && regions.chunk_saved(chunk_x, chunk_z).unwrap_or(false))
    }

    fn highest_block_y(&self, world: &str, x: i32, z: i32) -> Option<i32> {
        let level = self.host.level(world)?;
        self.host.chunk(level, [x >> 4, z >> 4])?;
        let (min_y, height) = level.height(self.host)?;
        (min_y..min_y + height).rev().find(|y| level.block(self.host, [x, *y, z]).is_some_and(|block| !self.air.contains(&block)))
    }
}

/// Chat text from legacy `§` codes: Horizon's literal components do not interpret them.
fn chat_text(message: &ChatMessage) -> Text {
    let span_text = |text: &str| {
        legacy_spans(text).into_iter().fold(Text::literal(""), |parent, span| {
            let mut style = TextStyle::default();
            style.color = span.color.map(str::to_owned);
            for (field, enabled) in [
                (&mut style.bold, span.bold),
                (&mut style.italic, span.italic),
                (&mut style.underlined, span.underlined),
                (&mut style.strikethrough, span.strikethrough),
                (&mut style.obfuscated, span.obfuscated),
            ] {
                *field = enabled.then_some(true);
            }
            parent.append(Text::literal(span.text).with_style(style))
        })
    };
    let text = span_text(&message.text);
    match &message.link {
        Some(link) => {
            let mut style = TextStyle::default();
            style.click = Some(TextClick::OpenUrl(link.url.clone()));
            style.hover = Some(TextHover::Text(Box::new(span_text(&link.hover))));
            text.with_style(style)
        }
        None => text,
    }
}

/// Who a `/tpll` teleports and what they may do.
struct TpllTarget {
    player: PlayerRef,
    authorization: Authorization,
    /// The target's level, when it is one of this mod's.
    level: Option<String>,
    /// `t+-.tpll.otherWorld` (of the target, as upstream checks it): from another level, go to
    /// the earth level instead of refusing.
    other_world: bool,
}

/// The height last shown in a player's action bar, and when.
type ActionBarState = HashMap<PlayerRef, (String, u64)>;

#[derive(Default)]
pub struct TerraPlusMinus {
    shared: OnceLock<Arc<Shared>>,
    regions: OnceLock<LevelRegions>,
    air: OnceLock<Vec<BlockKey>>,
    pending: Mutex<Option<PendingTeleports<PlayerRef>>>,
    ticks: AtomicU64,
    actionbar: Mutex<ActionBarState>,
}

/// The level of this mod `player` is in, by asking each level for its players.
fn player_level(host: &dyn Host, levels: &[LevelPlan], player: PlayerRef) -> Option<String> {
    levels
        .iter()
        .find(|level| host.level(&level.id).and_then(|handle| handle.online_players(host)).is_some_and(|players| players.contains(&player)))
        .map(|level| level.id.clone())
}

fn holds_debug_stick(host: &dyn Host, player: PlayerRef) -> bool {
    host.living(player.entity()).and_then(|living| living.equipment(host, EquipmentSlot::MainHand)).is_some_and(|stack| stack.item().id(host) == DEBUG_STICK)
}

impl TerraPlusMinus {
    fn feedback(host: &mut dyn ServerHost, line: &str) {
        Self::feedback_message(host, &ChatMessage::from(line.to_owned()));
    }

    fn feedback_message(host: &mut dyn ServerHost, message: &ChatMessage) {
        let _ = host.commands().feedback(chat_text(message));
    }

    fn tell(host: &mut dyn ServerHost, player: PlayerRef, line: &str) {
        let _ = player.tell(host, chat_text(&ChatMessage::from(line.to_owned())));
    }

    fn finish(host: &mut dyn ServerHost, player: PlayerRef, step: TpllStep, prefix: &str, reply: &mut dyn FnMut(&mut dyn ServerHost, &str)) {
        let lines = messages::tpll_step_lines(&step, prefix);
        if let TpllStep::Teleport { world, x, y, z, .. } = &step
            && !player.entity().teleport_to_level(host, world, [*x, *y, *z], None)
        {
            return reply(host, &messages::unavailable(prefix));
        }
        for line in lines {
            reply(host, &line);
        }
    }

    fn player_position(host: &dyn ServerHost, event: &RunModCommandEvent<'_>) -> Option<(PlayerRef, [f64; 3])> {
        let CommandSource::Player { player, .. } = event.source.clone() else { return None };
        Some((player, player.entity().position(host)?))
    }

    fn info_command(&self, host: &mut dyn ServerHost, event: &RunModCommandEvent<'_>, command: &str) {
        let Some(shared) = self.shared.get() else { return Self::feedback(host, "§cTerraPlusMinus failed to start; see the server log.") };
        let Some((_, [x, _, z])) = Self::player_position(&*host, event) else { return Self::feedback(host, "§cOnly players can use this command.") };
        let config = shared.stack.config();
        let projection = shared.stack.generator().projection().as_ref();
        let lines: Vec<ChatMessage> = match command {
            "where" => vec![messages::where_message(projection, x, z, &config.prefix)],
            "offset" => messages::offset_lines(config, &config.prefix).into_iter().map(ChatMessage::from).collect(),
            _ => vec![messages::distortion_message(projection, x, z).into()],
        };
        for line in &lines {
            Self::feedback_message(host, line);
        }
    }

    /// `/tpll <coordinates>` for the player who ran it, or `/tpll -p <players> <coordinates>`
    /// for each selected player (`t+-.forcetpll`, from any source).
    fn tpll(&self, host: &mut dyn ServerHost, event: &RunModCommandEvent<'_>) {
        let Some(shared) = self.shared.get() else { return Self::feedback(host, "§cTerraPlusMinus failed to start; see the server log.") };
        let config = shared.stack.config();
        let prefix = config.prefix.as_str();
        let Some(CommandArgumentValue::String(arguments)) = event.command.argument("coordinates").cloned() else { return Self::feedback(host, &messages::tpll_usage(prefix)) };
        let sender_level = event.source.permission().level();
        let admin = sender_level >= config.permissions.admin;
        match event.command.argument("targets").cloned() {
            Some(CommandArgumentValue::Players(targets)) => {
                // The usage is registered at this level; checked again as upstream's `requires` is.
                if sender_level < config.permissions.forcetpll {
                    return Self::feedback(host, &messages::tpll_usage(prefix));
                }
                let levels = config.levels();
                for player in &targets {
                    let target_level = player.permission_level(&*host).unwrap_or(0);
                    let target = TpllTarget {
                        player: *player,
                        authorization: Authorization { sender_is_target: false, admin, may_generate_chunks: target_level >= config.permissions.tpll_ungenerated_chunks },
                        level: player_level(&*host, &levels, *player),
                        other_world: target_level >= config.permissions.tpll_other_world,
                    };
                    self.tpll_for(host, shared, &arguments, target);
                }
                Self::feedback(host, &messages::forced_tpll_summary(targets.len(), prefix));
            }
            Some(_) => Self::feedback(host, &messages::tpll_usage(prefix)),
            None => {
                let CommandSource::Player { player, .. } = event.source.clone() else { return Self::feedback(host, "§cOnly players can use /tpll.") };
                let current = event.command.dimension.id(&*host).map(|id| id.to_ascii_lowercase());
                let level = current.filter(|id| config.levels().iter().any(|level| &level.id == id));
                let target = TpllTarget {
                    player,
                    authorization: Authorization { sender_is_target: true, admin, may_generate_chunks: sender_level >= config.permissions.tpll_ungenerated_chunks },
                    level,
                    other_world: sender_level >= config.permissions.tpll_other_world,
                };
                self.tpll_for(host, shared, &arguments, target);
            }
        }
    }

    fn tpll_for(&self, host: &mut dyn ServerHost, shared: &Shared, arguments: &str, target: TpllTarget) {
        let (Some(regions), Some(air)) = (self.regions.get(), self.air.get()) else { return Self::feedback(host, "§cThe world is not open yet.") };
        let config = shared.stack.config();
        let prefix = config.prefix.as_str();
        let TpllTarget { player, authorization, level, other_world } = target;
        let world_name = match level {
            Some(name) => name,
            None if other_world => LEVEL_ID.to_owned(),
            None => return Self::feedback(host, &format!("{prefix}§cYou cannot use /tpll in this world.")),
        };
        let y_offset = config.level_offset(&world_name);
        let (request, step) = {
            let world = HostWorld { host: &*host, regions, air };
            TpllRequest::start(arguments, &world_name, shared.stack.generator().projection().as_ref(), &config.team_bounds, authorization, &config.linked_worlds, y_offset, &world)
        };
        match (request, &step) {
            // The target chunk is not generated yet: refuse while generation cannot produce it,
            // rather than teleporting the player into a chunk that will fail.
            (Some(_), TpllStep::NeedsSurface { .. }) if shared.control.gate.state() != GateState::Open => {
                log(&format!("/tpll refused: generation is {:?}", shared.control.gate.state()));
                Self::feedback(host, &messages::unavailable(prefix));
            }
            (Some(request), TpllStep::NeedsSurface { .. }) => {
                let Ok(mut pending) = self.pending.lock() else { return };
                let table = pending.get_or_insert_with(|| PendingTeleports::new(config.max_pending_teleports, config.generation_wait));
                match table.submit(player, request, &step, authorization, shared.stack.loader()) {
                    Ok(()) => {}
                    Err(refusal) => {
                        log(&format!("/tpll refused: {refusal:?}"));
                        Self::feedback(host, &messages::unavailable(prefix));
                    }
                }
            }
            _ => Self::finish(host, player, step, prefix, &mut |host, line| Self::feedback(host, line)),
        }
    }

    /// Checks the generation fingerprint of every level of this mod once and opens or refuses
    /// generation; also opens the region indexes used by `/tpll`. The earth level keeps its
    /// fingerprint in `horizon-mod-data/terraplusminus`, a linked level in
    /// `horizon-mod-data/terraplusminus/levels/<name>`, each with that level's Y offset. One level
    /// refused closes generation for all, as they share the generator. Without a world directory
    /// generation fails rather than waiting.
    fn check_fingerprint(&self, host: &mut dyn ServerHost) {
        let Some(shared) = self.shared.get() else { return };
        let Some(world_directory) = host.world_dir() else {
            shared.control.gate.set(GateState::Failed("the world directory is unavailable, so the generation fingerprint cannot be checked".into()));
            return log("generation disabled: the world directory is unavailable");
        };
        let levels = shared.stack.config().levels();
        let dimensions = world_directory.join("dimensions").join(MOD_ID);
        let _ = self.regions.set(levels.iter().map(|level| (level.id.clone(), RegionIndex::new(dimensions.join(&level.path).join("region"), 256, Duration::from_secs(2)))).collect());
        if shared.control.gate.state() != GateState::AwaitingFingerprint {
            return;
        }
        let data = world_directory.join("horizon-mod-data").join(MOD_ID);
        let mut status = None;
        for level in &levels {
            let directory = if level.id == LEVEL_ID { data.clone() } else { data.join("levels").join(&level.path) };
            let level_status = shared.stack.verify_level(&directory, level.y_offset);
            if level_status.is_err() {
                log(&format!("level {}: generation fingerprint refused", level.id));
                status = Some(level_status);
                break;
            }
            if status.is_none() {
                status = Some(level_status);
            }
        }
        let Some(status) = status else { return };
        match shared.control.gate.apply_fingerprint(&status) {
            GateState::Refused(reason) => log(&format!("generation refused: {reason}")),
            state => log(&format!("generation fingerprint {status:?}: {state:?}")),
        }
    }

    /// `height_in_actionbar`: every online player sees `<height>m` in bold, refreshed when their
    /// block Y changes (upstream updates on each move) and every 20 ticks, except while holding
    /// a debug stick in the main hand.
    fn update_action_bars(&self, host: &mut dyn ServerHost, shared: &Shared, tick: u64) {
        let config = shared.stack.config();
        let levels = config.levels();
        let mut level_of: HashMap<PlayerRef, String> = HashMap::new();
        for level in &levels {
            for player in host.level(&level.id).and_then(|handle| handle.online_players(&*host)).unwrap_or_default() {
                level_of.insert(player, level.id.clone());
            }
        }
        let players = host.players().server_players();
        let Ok(mut shown) = self.actionbar.lock() else { return };
        shown.retain(|player, _| players.contains(player));
        for player in players {
            let Some([_, y, _]) = player.entity().position(&*host) else { continue };
            if holds_debug_stick(&*host, player) {
                shown.remove(&player);
                continue;
            }
            let height = messages::actionbar_height(y, config.actionbar_offset(level_of.get(&player).map(String::as_str)));
            let due = match shown.get(&player) {
                Some((last, at)) => *last != height || tick.saturating_sub(*at) >= ACTIONBAR_PERIOD_TICKS,
                None => true,
            };
            if !due {
                continue;
            }
            let mut style = TextStyle::default();
            style.bold = Some(true);
            let _ = player.action_bar(host, Text::literal(height.clone()).with_style(style));
            shown.insert(player, (height, tick));
        }
    }

    fn status(&self, host: &mut dyn ServerHost) {
        let Some(shared) = self.shared.get() else { return Self::feedback(host, "§cTerraPlusMinus failed to start; see the server log.") };
        let pending = self.pending.lock().ok().and_then(|table| table.as_ref().map(PendingTeleports::len)).unwrap_or(0);
        let mut lines = vec![format!("generation: {:?}", shared.control.gate.state()), shared.control.counters.summary(), format!("pending /tpll: {pending}")];
        lines.extend(shared.stack.diagnostics_report().lines().map(str::to_owned));
        for line in lines {
            Self::feedback(host, &line);
        }
    }
}

#[mod_api::horizon_mod]
impl Hooks for TerraPlusMinus {
    fn register(&self, registrar: &mut dyn Registrar, context: &RegistrationContext) {
        let shared = match load_shared(context) {
            Ok(shared) => Arc::new(shared),
            Err(reason) => {
                log(&format!("not registering the earth dimension: {reason}"));
                return;
            }
        };
        let world = shared.stack.config().world;
        let mut names: Vec<String> = ["minecraft:stone", "minecraft:water", "minecraft:grass_block", "minecraft:dirt", "minecraft:sand", "minecraft:snow_block"].map(str::to_owned).into();
        names.push(shared.stack.config().generator.surface_material.resource_name().to_owned());
        names.extend(shared.stack.surface_block_names());
        let mut seen = std::collections::HashSet::new();
        names.retain(|name| seen.insert(name.clone()));
        let mut blocks = Vec::with_capacity(names.len());
        let mut malformed = Vec::new();
        for name in names {
            match split_block_state(&name) {
                Some((id, properties)) => {
                    let properties = properties.into_iter().map(|(key, value)| (key.to_owned(), value.to_owned())).collect();
                    blocks.push(RegisteredBlock { key: registrar.block_key(id), name, properties });
                }
                None => malformed.push(name),
            }
        }
        if !malformed.is_empty() {
            log(&format!("not registering the earth dimension: malformed block states in the configuration: {}", malformed.join(", ")));
            return;
        }
        let _ = self.air.set(["minecraft:air", "minecraft:cave_air", "minecraft:void_air"].into_iter().map(|name| registrar.block_key(name)).collect());
        let blocks = Arc::new(blocks);
        // One dimension per level, sharing the data stack; they differ only in their Y offset,
        // as upstream's linked worlds each run a RealWorldGenerator with their own offset.
        for level in shared.stack.config().levels() {
            registrar.register_chunk_generator(&level.path, EarthGenerator { shared: Arc::clone(&shared), blocks: Arc::clone(&blocks), y_offset: level.y_offset });
            registrar.register_biome_source(&level.path, EarthBiomes { shared: Arc::clone(&shared), lookup: BiomeLookup::default() });
            registrar.register_dimension(
                &level.path,
                DimensionSettings {
                    min_y: world.min_y,
                    height: world.max_y - world.min_y,
                    logical_height: world.max_y - world.min_y,
                    generator: level.id.clone(),
                    mod_biome_source: Some(level.id.clone()),
                    sea_level: Some(63i32.wrapping_add(level.y_offset)),
                    ..Default::default()
                },
            );
            if level.id != LEVEL_ID {
                log(&format!("registered the linked level {} at Y offset {}", level.id, level.y_offset));
            }
        }
        let _ = self.shared.set(shared);
    }

    fn level_load(&self, host: &mut dyn ServerHost, event: &mut LevelLoadEvent<'_>) {
        // Before any earth chunk: the world the server opens in loads before `server_started`,
        // and its chunks would otherwise wait for a check that comes after them.
        let ours = self.shared.get().is_some_and(|shared| shared.stack.config().levels().iter().any(|level| level.id == event.dimension));
        if ours {
            self.check_fingerprint(host);
        }
    }

    fn server_started(&self, host: &mut dyn ServerHost, _: &mut ServerStartedEvent) {
        self.check_fingerprint(host);
    }

    fn server_stopping(&self, _: &mut dyn ServerHost, _: &mut ServerStoppingEvent) {
        // Ends waits and backoffs at once; chunks still generating fail instead of completing.
        let Some(shared) = self.shared.get() else { return };
        shared.control.gate.shut_down();
        shared.stack.shut_down();
    }

    fn register_commands(&self, _: &mut dyn ServerHost, event: &mut RegisterCommandsEvent<'_>) {
        let Some(shared) = self.shared.get() else { return };
        let config = shared.stack.config();
        let permissions = config.permissions;
        // `passthrough_tpll`: another mod answers /tpll and /tpc (upstream rewrites them to that
        // namespace), so this mod leaves both unregistered.
        if config.passthrough_tpll.is_none() {
            for name in ["tpll", "tpc"] {
                // Bare `/tpll` shows the usage, as upstream's root node does.
                event.commands.push(Command::op(name, permissions.tpll));
                event.commands.push(Command::op(&format!("{name} <coordinates:text>"), permissions.tpll));
                // A literal `-p` before the players, so coordinates are never read as a selector.
                event.commands.push(Command::op(&format!("{name} -p <targets:players> <coordinates:text>"), permissions.forcetpll));
            }
        }
        for info in ["where", "offset", "distortion"] {
            event.commands.push(Command::op(info, permissions.info_commands));
        }
        event.commands.push(Command::op("terraplusminus status", permissions.diagnostics));
    }

    fn run_mod_command(&self, host: &mut dyn ServerHost, event: &mut RunModCommandEvent<'_>) -> Flow {
        match event.command.literals.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
            ["tpll" | "tpc"] | ["tpll" | "tpc", "-p"] => self.tpll(host, event),
            [info @ ("where" | "offset" | "distortion")] => self.info_command(host, event, info),
            ["terraplusminus", "status"] => self.status(host),
            _ => return Flow::Continue,
        }
        Flow::Cancel
    }

    fn server_tick(&self, host: &mut dyn ServerHost, event: &mut ServerTickEvent<'_>) {
        if !matches!(event.phase, ServerTickPhase::ServerTickStart { .. }) {
            return;
        }
        let tick = self.ticks.fetch_add(1, Ordering::Relaxed);
        if let Some(shared) = self.shared.get()
            && shared.stack.config().height_in_actionbar
        {
            self.update_action_bars(host, shared, tick);
        }
        let (Some(shared), Some(regions), Some(air)) = (self.shared.get(), self.regions.get(), self.air.get()) else { return };
        let finished = {
            let Ok(mut pending) = self.pending.lock() else { return };
            let Some(table) = pending.as_mut().filter(|table| !table.is_empty()) else { return };
            let world = HostWorld { host: &*host, regions, air };
            table.poll(&shared.stack.config().linked_worlds, &world)
        };
        let prefix = shared.stack.config().prefix.as_str();
        let generating = shared.control.gate.state() == GateState::Open;
        for (player, outcome) in finished {
            match outcome {
                // Generation closed (refused, stopping, failed) while the surface was looked up.
                TeleportOutcome::Step(TpllStep::Teleport { .. }) if !generating => {
                    log(&format!("/tpll dropped: generation is {:?}", shared.control.gate.state()));
                    Self::tell(host, player, &messages::unavailable(prefix));
                }
                TeleportOutcome::Step(step) => Self::finish(host, player, step, prefix, &mut |host, line| Self::tell(host, player, line)),
                TeleportOutcome::Unavailable(reason) => {
                    log(&format!("/tpll surface lookup failed: {reason}"));
                    Self::tell(host, player, &messages::unavailable(prefix));
                }
            }
        }
    }
}
