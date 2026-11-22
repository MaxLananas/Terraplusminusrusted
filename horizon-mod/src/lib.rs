//! TerraPlusMinus on Horizon: a chunk generator and biome source over the terra crates, the
//! `terraplusminus:earth` dimension, `/tpll` (`/tpc`), `/where`, `/offset`, `/distortion` and
//! `/terraplusminus status`.
//!
//! Generation runs on Horizon's worldgen workers and waits there for real elevation data; the
//! server tick only polls finished work. The world's generation fingerprint is checked when the
//! server starts, and generation stays closed if it does not match.
#![deny(unsafe_code)]

use mod_api::{
    BiomeSource, BlockKey, BlockState, ChunkAccess, ChunkGenerator, Command, CommandArgumentValue, CommandSource, DimensionSettings, Flow, Host, Hooks, InstanceDirectory,
    PlayerRef, Registrar, RegisterCommandsEvent, RegistrationContext, RunModCommandEvent, ServerHost, ServerStartedEvent, ServerTickEvent, ServerTickPhase, Text, TextClick, TextHover, TextStyle,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use terra_chunk_generation::WorldHeight;
use terra_server::{
    Authorization, BiomeLookup, BlockPalette, BlockWriter, GateState, GenerationCounters, GenerationGate, LevelHeight, PendingTeleports, RegionIndex,
    ServerConfig, TeleportOutcome, TeleportWorld, TerraStack, TpllRequest, TpllStep, extract_default_osm_mapping, messages, messages::{ChatMessage, legacy_spans}, parse_server_config,
    split_block_state, terrain_for_generation, write_chunk, write_trees, TreeTarget,
};

const MOD_ID: &str = "terraplusminus";
const DIMENSION: &str = "earth";
const LEVEL_ID: &str = "terraplusminus:earth";
const CONFIG_FILE: &str = "config/terraplusminus/terraplusminus.json5";

struct Shared {
    stack: TerraStack,
    gate: GenerationGate,
    counters: GenerationCounters,
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
    Ok(Shared { stack, gate: GenerationGate::default(), counters: GenerationCounters::default() })
}

struct EarthGenerator {
    shared: Arc<Shared>,
    /// Every block the generator can place, by block state string, resolved at registration.
    blocks: Vec<RegisteredBlock>,
}

struct RegisteredBlock {
    name: String,
    key: BlockKey,
    properties: Vec<(String, String)>,
}

struct AccessWriter<'a>(&'a mut dyn ChunkAccess);

impl BlockWriter<BlockState> for AccessWriter<'_> {
    fn set_block(&mut self, position: [i32; 3], state: BlockState) {
        self.0.set_block(position, state);
    }
}

impl ChunkGenerator for EarthGenerator {
    fn build_terrain(&self, chunk: &mut dyn ChunkAccess) {
        let shared = &self.shared;
        let position = chunk.pos();
        shared.gate.wait_open(Duration::from_secs(60), &mut |state| match state {
            GateState::Refused(reason) => log(&format!("chunk ({}, {}) not generated: {reason}", position.x, position.z)),
            _ => log(&format!("chunk ({}, {}) waits for the generation fingerprint check", position.x, position.z)),
        });
        let terrain = terrain_for_generation(&shared.stack, &shared.counters, position.x, position.z, &mut |message| log(&message));
        let world = WorldHeight { min_y: chunk.min_y(), max_y: chunk.min_y() + chunk.height() };
        let resolved: Vec<(String, BlockState)> = self
            .blocks
            .iter()
            .map(|block| {
                let properties: Vec<(&str, &str)> = block.properties.iter().map(|(key, value)| (key.as_str(), value.as_str())).collect();
                let state = if properties.is_empty() { chunk.state(block.key) } else { chunk.state_with(block.key, &properties) };
                (block.name.clone(), state)
            })
            .collect();
        let Some(&(_, stone)) = resolved.first() else { return };
        let mut palette = BlockPalette::new(move |name: &str| resolved.iter().find(|(known, _)| known == name).map_or(stone, |(_, state)| *state));
        let level_seed = chunk.seed();
        write_chunk(&shared.stack, &terrain, position.x, position.z, world, &mut palette, &mut AccessWriter(&mut *chunk));
        // Trees come after the terrain and may overwrite it, as upstream's populator does.
        let target = TreeTarget { chunk_x: position.x, chunk_z: position.z, world, level_seed };
        write_trees(&shared.stack, &shared.counters, &terrain, target, &mut palette, &mut AccessWriter(chunk), &mut |message| log(&message));
        shared.counters.chunks_written.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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

struct HostWorld<'a> {
    host: &'a dyn Host,
    regions: &'a RegionIndex,
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
        loaded || (world == LEVEL_ID && self.regions.chunk_saved(chunk_x, chunk_z).unwrap_or(false))
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

#[derive(Default)]
pub struct TerraPlusMinus {
    shared: OnceLock<Arc<Shared>>,
    regions: OnceLock<RegionIndex>,
    air: OnceLock<Vec<BlockKey>>,
    pending: Mutex<Option<PendingTeleports<PlayerRef>>>,
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

    fn tpll(&self, host: &mut dyn ServerHost, event: &RunModCommandEvent<'_>) {
        let Some(shared) = self.shared.get() else { return Self::feedback(host, "§cTerraPlusMinus failed to start; see the server log.") };
        let (Some(regions), Some(air)) = (self.regions.get(), self.air.get()) else { return Self::feedback(host, "§cThe world is not open yet.") };
        let CommandSource::Player { player, permission } = event.source.clone() else { return Self::feedback(host, "§cOnly players can use /tpll.") };
        let config = shared.stack.config();
        let prefix = config.prefix.as_str();
        let Some(CommandArgumentValue::String(arguments)) = event.command.argument("coordinates").cloned() else { return Self::feedback(host, &messages::tpll_usage(prefix)) };
        let level = permission.level();
        let authorization = Authorization { sender_is_target: true, admin: level >= config.permissions.admin, may_generate_chunks: level >= config.permissions.tpll_ungenerated_chunks };
        let current = event.command.dimension.id(&*host);
        let world_name = match current {
            Some(name) if name == LEVEL_ID || config.linked_worlds.worlds.iter().any(|linked| linked.name == name) => name,
            _ if level >= config.permissions.tpll_other_world => LEVEL_ID.to_owned(),
            _ => return Self::feedback(host, &format!("{prefix}§cYou cannot use /tpll in this world.")),
        };
        let (request, step) = {
            let world = HostWorld { host: &*host, regions, air };
            TpllRequest::start(&arguments, &world_name, shared.stack.generator().projection().as_ref(), &config.team_bounds, authorization, &config.linked_worlds, config.generator.y_offset, &world)
        };
        match (request, &step) {
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

    fn status(&self, host: &mut dyn ServerHost) {
        let Some(shared) = self.shared.get() else { return Self::feedback(host, "§cTerraPlusMinus failed to start; see the server log.") };
        let pending = self.pending.lock().ok().and_then(|table| table.as_ref().map(PendingTeleports::len)).unwrap_or(0);
        let mut lines = vec![format!("generation: {:?}", shared.gate.state()), shared.counters.summary(), format!("pending /tpll: {pending}")];
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
        for name in names {
            match split_block_state(&name) {
                Some((id, properties)) => {
                    let properties = properties.into_iter().map(|(key, value)| (key.to_owned(), value.to_owned())).collect();
                    blocks.push(RegisteredBlock { key: registrar.block_key(id), name, properties });
                }
                None => log(&format!("ignoring malformed block state `{name}`; stone is placed instead")),
            }
        }
        let _ = self.air.set(["minecraft:air", "minecraft:cave_air", "minecraft:void_air"].into_iter().map(|name| registrar.block_key(name)).collect());
        registrar.register_chunk_generator(DIMENSION, EarthGenerator { shared: Arc::clone(&shared), blocks });
        registrar.register_biome_source(DIMENSION, EarthBiomes { shared: Arc::clone(&shared), lookup: BiomeLookup::default() });
        registrar.register_dimension(
            DIMENSION,
            DimensionSettings {
                min_y: world.min_y,
                height: world.max_y - world.min_y,
                logical_height: world.max_y - world.min_y,
                generator: LEVEL_ID.into(),
                mod_biome_source: Some(LEVEL_ID.into()),
                sea_level: Some(63 + shared.stack.config().generator.y_offset),
                ..Default::default()
            },
        );
        let _ = self.shared.set(shared);
    }

    fn server_started(&self, host: &mut dyn ServerHost, _: &mut ServerStartedEvent) {
        let (Some(shared), Some(world_directory)) = (self.shared.get(), host.world_dir()) else { return };
        let status = shared.stack.verify_world(&world_directory.join("horizon-mod-data").join(MOD_ID));
        match shared.gate.apply_fingerprint(&status) {
            GateState::Refused(reason) => log(&format!("generation refused: {reason}")),
            state => log(&format!("generation fingerprint {status:?}: {state:?}")),
        }
        let _ = self.regions.set(RegionIndex::new(world_directory.join("dimensions").join(MOD_ID).join(DIMENSION).join("region"), 256, Duration::from_secs(2)));
    }

    fn register_commands(&self, _: &mut dyn ServerHost, event: &mut RegisterCommandsEvent<'_>) {
        let Some(shared) = self.shared.get() else { return };
        let permissions = shared.stack.config().permissions;
        event.commands.push(Command::op("tpll <coordinates:text>", permissions.tpll));
        event.commands.push(Command::op("tpc <coordinates:text>", permissions.tpll));
        for info in ["where", "offset", "distortion"] {
            event.commands.push(Command::op(info, permissions.info_commands));
        }
        event.commands.push(Command::op("terraplusminus status", permissions.diagnostics));
    }

    fn run_mod_command(&self, host: &mut dyn ServerHost, event: &mut RunModCommandEvent<'_>) -> Flow {
        match event.command.literals.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
            ["tpll"] | ["tpc"] => self.tpll(host, event),
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
        let (Some(shared), Some(regions), Some(air)) = (self.shared.get(), self.regions.get(), self.air.get()) else { return };
        let finished = {
            let Ok(mut pending) = self.pending.lock() else { return };
            let Some(table) = pending.as_mut().filter(|table| !table.is_empty()) else { return };
            let world = HostWorld { host: &*host, regions, air };
            table.poll(&shared.stack.config().linked_worlds, &world)
        };
        let prefix = shared.stack.config().prefix.as_str();
        for (player, outcome) in finished {
            match outcome {
                TeleportOutcome::Step(step) => Self::finish(host, player, step, prefix, &mut |host, line| Self::tell(host, player, line)),
                TeleportOutcome::Unavailable(reason) => {
                    log(&format!("/tpll surface lookup failed: {reason}"));
                    Self::tell(host, player, &messages::unavailable(prefix));
                }
            }
        }
    }
}
