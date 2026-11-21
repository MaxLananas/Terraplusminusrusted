//! TerraPlusMinus on Horizon: a chunk generator and biome source over the terra crates, the
//! `terraplusminus:earth` dimension, `/tpll` and `/terraplusminus status`.
//!
//! Generation runs on Horizon's worldgen workers and waits there for real elevation data; the
//! server tick only polls finished work. The world's generation fingerprint is checked when the
//! server starts, and generation stays closed if it does not match.
#![deny(unsafe_code)]

// The glob brings the host-area and registrar traits whose methods are called below into scope.
#[allow(unused_imports)]
use mod_api::*;
use mod_api::{
    BiomeSource, BlockKey, BlockState, ChunkAccess, ChunkGenerator, Command, CommandArgumentValue, CommandSource, DimensionSettings, Flow, Hooks, InstanceDirectory,
    PlayerRef, Registrar, RegisterCommandsEvent, RegistrationContext, RunModCommandEvent, ServerHost, ServerStartedEvent, ServerTickEvent, Text,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use terra_chunk_generation::WorldHeight;
use terra_server::{
    Authorization, BiomeLookup, BlockPalette, BlockWriter, GateState, GenerationCounters, GenerationGate, LevelHeight, PendingTeleports, RegionIndex,
    ServerConfig, TeleportOutcome, TeleportWorld, TerraStack, TpllRequest, TpllStep, parse_server_config, terrain_for_generation, write_chunk,
};

const MOD_ID: &str = "terraplusminus";
const DIMENSION: &str = "earth";
const LEVEL_ID: &str = "terraplusminus:earth";
const CONFIG_FILE: &str = "config/terraplusminus/terraplusminus.json5";
const USAGE: &str = "§cUsage: /tpll <latitude> <longitude> [height]";

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
            log(&format!("no {CONFIG_FILE}; using the upstream defaults (AWS terrain tiles need network.plain_http_hosts or a mirror)"));
            ServerConfig::default()
        }
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let base = path.parent().map_or(instance.clone(), PathBuf::from);
    let stack = TerraStack::build(config, &base).map_err(|error| error.to_string())?;
    Ok(Shared { stack, gate: GenerationGate::default(), counters: GenerationCounters::default() })
}

struct EarthGenerator {
    shared: Arc<Shared>,
    /// Every block the generator can place, by resource name, resolved at registration.
    blocks: Vec<(String, BlockKey)>,
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
        let resolved: Vec<(String, BlockState)> = self.blocks.iter().map(|(name, key)| (name.clone(), chunk.state(*key))).collect();
        let Some(&(_, stone)) = resolved.first() else { return };
        let mut palette = BlockPalette::new(move |name: &str| resolved.iter().find(|(known, _)| known == name).map_or(stone, |(_, state)| *state));
        write_chunk(&shared.stack, &terrain, position.x, position.z, world, &mut palette, &mut AccessWriter(chunk));
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
    host: &'a dyn ServerHost,
    regions: &'a RegionIndex,
    air: &'a [BlockKey],
}

impl TeleportWorld for HostWorld<'_> {
    fn level_height(&self, world: &str) -> Option<LevelHeight> {
        let level = self.host.level(world)?;
        let (min_y, height) = self.host.level_height(level)?;
        Some(LevelHeight { min_y, max_y: min_y + height })
    }

    fn is_chunk_generated(&self, world: &str, chunk_x: i32, chunk_z: i32) -> bool {
        let loaded = self.host.level(world).and_then(|level| self.host.chunk(level, [chunk_x, chunk_z])).is_some();
        loaded || (world == LEVEL_ID && self.regions.chunk_saved(chunk_x, chunk_z).unwrap_or(false))
    }

    fn highest_block_y(&self, world: &str, x: i32, z: i32) -> Option<i32> {
        let level = self.host.level(world)?;
        self.host.chunk(level, [x >> 4, z >> 4])?;
        let (min_y, height) = self.host.level_height(level)?;
        (min_y..min_y + height).rev().find(|y| self.host.level_block(level, [x, *y, z]).is_some_and(|block| !self.air.contains(&block)))
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
        let _ = host.commands().feedback(Text::literal(line));
    }

    fn tell(host: &mut dyn ServerHost, player: PlayerRef, line: &str) {
        let _ = host.player_tell(player, Text::literal(line));
    }

    fn finish(host: &mut dyn ServerHost, player: PlayerRef, step: TpllStep, reply: &mut dyn FnMut(&mut dyn ServerHost, &str)) {
        match step {
            TpllStep::Teleport { world, x, y, z, reported, reported_height } => {
                if host.entity_teleport_to_level(player.entity(), &world, [x, y, z], None) {
                    reply(host, &format!("§7Teleported to {}, {}, {reported_height}.", reported.latitude, reported.longitude));
                } else {
                    reply(host, "§cThis location is currently unavailable. Please try again later.");
                }
            }
            TpllStep::Usage => reply(host, USAGE),
            TpllStep::OutsideProjection => reply(host, "§cLocation is not within projection bounds."),
            TpllStep::OutsideTeamBounds => reply(host, "§cYou cannot tpll to these coordinates, because this area is being handled by another build team."),
            TpllStep::UngeneratedChunkDenied => reply(host, "§cYou cannot tpll to chunks that have not been generated yet."),
            TpllStep::Unreachable => reply(host, "§cThis height cannot be reached."),
            TpllStep::LinkedWorldUnavailable { world } => reply(host, &format!("§cThe linked world {world} is not loaded.")),
            TpllStep::NeedsSurface { .. } => reply(host, "§cThis location is currently unavailable. Please try again later."),
        }
    }

    fn tpll(&self, host: &mut dyn ServerHost, event: &RunModCommandEvent<'_>) {
        let Some(shared) = self.shared.get() else { return Self::feedback(host, "§cTerraPlusMinus failed to start; see the server log.") };
        let (Some(regions), Some(air)) = (self.regions.get(), self.air.get()) else { return Self::feedback(host, "§cThe world is not open yet.") };
        let CommandSource::Player { player, permission } = event.source.clone() else { return Self::feedback(host, "§cOnly players can use /tpll.") };
        let Some(CommandArgumentValue::String(arguments)) = event.command.argument("coordinates").cloned() else { return Self::feedback(host, USAGE) };
        let config = shared.stack.config();
        let level = permission.level();
        let authorization = Authorization { sender_is_target: true, admin: level >= config.permissions.admin, may_generate_chunks: level >= config.permissions.tpll_ungenerated_chunks };
        let current = host.dimension_key_id(event.command.dimension);
        let world_name = match current {
            Some(name) if name == LEVEL_ID || config.linked_worlds.worlds.iter().any(|linked| linked.name == name) => name,
            _ => LEVEL_ID.to_owned(),
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
                    Ok(()) => Self::feedback(host, "§7Loading terrain for /tpll..."),
                    Err(refusal) => {
                        log(&format!("/tpll refused: {refusal:?}"));
                        Self::feedback(host, "§cThis location is currently unavailable. Please try again later.");
                    }
                }
            }
            _ => Self::finish(host, player, step, &mut |host, line| Self::feedback(host, line)),
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
        let mut names = vec!["minecraft:stone", "minecraft:water", "minecraft:grass_block", "minecraft:dirt", "minecraft:sand", "minecraft:snow_block"];
        let surface = shared.stack.config().generator.surface_material.resource_name().to_owned();
        if !names.contains(&surface.as_str()) {
            names.push(&surface);
        }
        let blocks = names.iter().map(|name| ((*name).to_owned(), registrar.block_key(name))).collect();
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
        event.commands.push(Command::op("terraplusminus status", permissions.diagnostics));
    }

    fn run_mod_command(&self, host: &mut dyn ServerHost, event: &mut RunModCommandEvent<'_>) -> Flow {
        match event.command.literals.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
            ["tpll"] => self.tpll(host, event),
            ["terraplusminus", "status"] => self.status(host),
            _ => return Flow::Continue,
        }
        Flow::Cancel
    }

    fn server_tick(&self, host: &mut dyn ServerHost, _: &mut ServerTickEvent<'_>) {
        let (Some(shared), Some(regions), Some(air)) = (self.shared.get(), self.regions.get(), self.air.get()) else { return };
        let finished = {
            let Ok(mut pending) = self.pending.lock() else { return };
            let Some(table) = pending.as_mut().filter(|table| !table.is_empty()) else { return };
            let world = HostWorld { host: &*host, regions, air };
            table.poll(&shared.stack.config().linked_worlds, &world)
        };
        for (player, outcome) in finished {
            match outcome {
                TeleportOutcome::Step(step) => Self::finish(host, player, step, &mut |host, line| Self::tell(host, player, line)),
                TeleportOutcome::Unavailable(reason) => {
                    log(&format!("/tpll surface lookup failed: {reason}"));
                    Self::tell(host, player, "§cThis location is currently unavailable. Please try again later.");
                }
            }
        }
    }
}
