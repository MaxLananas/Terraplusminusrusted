//! Signatures from horizon-mod-vocab at ffd59c4e0cebd16c0982016f040b3a9b70e8b83b; see README.
#![allow(unused_variables, dead_code, clippy::all)]

use std::path::PathBuf;
use std::sync::Arc;

pub use horizon_mod_api_stub_macros::horizon_mod;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlockKey(u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct BlockState(u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DimensionKey(u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LevelRef(u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ChunkRef(u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EntityRef(u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PlayerRef(EntityRef);
impl PlayerRef {
    pub fn entity(self) -> EntityRef {
        self.0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ChunkPos {
    pub x: i32,
    pub z: i32,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Text(String);
impl Text {
    pub fn literal(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}
#[derive(Debug)]
pub struct TextFormError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum PermissionLevel {
    All,
    Moderators,
    Gamemasters,
    Admins,
    Owners,
}
impl PermissionLevel {
    pub fn level(self) -> u8 {
        self as u8
    }
}

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum CommandSource {
    Player { player: PlayerRef, permission: PermissionLevel },
    Console { permission: PermissionLevel },
}
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum CommandArgumentValue {
    String(String),
    Integer(i32),
    Number(f64),
}
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct CommandArgument {
    pub name: String,
    pub value: CommandArgumentValue,
}
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct CommandInvocation {
    pub literals: Vec<String>,
    pub arguments: Vec<CommandArgument>,
    pub dimension: DimensionKey,
    pub position: [f64; 3],
}
impl CommandInvocation {
    pub fn argument(&self, name: &str) -> Option<&CommandArgumentValue> {
        self.arguments.iter().rev().find(|argument| argument.name == name).map(|argument| &argument.value)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Command {
    pub usage: String,
    pub level: u8,
}
impl Command {
    pub fn new(usage: &str) -> Self {
        Self { usage: usage.to_owned(), level: 0 }
    }
    pub fn op(usage: &str, level: u8) -> Self {
        Self { usage: usage.to_owned(), level: level.min(4) }
    }
}
pub enum Flow {
    Continue,
    Cancel,
}

#[non_exhaustive]
pub struct RunModCommandEvent<'a> {
    pub source: CommandSource,
    pub command: CommandInvocation,
    marker: std::marker::PhantomData<&'a ()>,
}
#[non_exhaustive]
pub struct RegisterCommandsEvent<'a> {
    pub commands: &'a mut Vec<Command>,
}
#[non_exhaustive]
pub struct ServerTickEvent<'a> {
    marker: std::marker::PhantomData<&'a ()>,
}
#[non_exhaustive]
pub struct ServerStartedEvent;

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum InstanceDirectory {
    Unix(Vec<u8>),
    Windows(Vec<u16>),
}
#[derive(Debug)]
pub enum RegistrationReadError {
    Unavailable,
}
pub struct RegistrationContext;
impl RegistrationContext {
    pub fn instance_dir(&self) -> Result<&InstanceDirectory, RegistrationReadError> {
        Err(RegistrationReadError::Unavailable)
    }
}

#[derive(Clone, Debug, Default)]
pub struct DimensionSettings {
    pub min_y: i32,
    pub height: i32,
    pub logical_height: i32,
    pub has_skylight: bool,
    pub generator: String,
    pub mod_biome_source: Option<String>,
    pub sea_level: Option<i32>,
}

pub trait BlockMetadata {}

pub trait ChunkAccess {
    fn metadata(&self) -> &dyn BlockMetadata;
    fn pos(&self) -> ChunkPos;
    fn level(&self) -> &str;
    fn seed(&self) -> i64;
    fn min_y(&self) -> i32;
    fn height(&self) -> i32;
    fn state(&self, block: BlockKey) -> BlockState;
    fn state_with(&self, block: BlockKey, properties: &[(&str, &str)]) -> BlockState;
    fn set_block(&mut self, pos: [i32; 3], state: BlockState) -> bool;
}
pub trait ChunkGenerator: Send + Sync + 'static {
    fn build_terrain(&self, chunk: &mut dyn ChunkAccess);
}
pub trait BiomeSource: Send + Sync + 'static {
    fn possible_biomes(&self) -> Vec<String>;
    fn noise_biome(&self, quart: [i32; 3], seed: i64) -> u16;
}

pub trait Registrar {
    fn block_key(&mut self, id: &str) -> BlockKey;
    fn add_dimension(&mut self, name: &str, settings: DimensionSettings);
    fn add_chunk_generator(&mut self, name: &str, generator: Arc<dyn ChunkGenerator>);
    fn add_biome_source(&mut self, name: &str, source: Arc<dyn BiomeSource>);
}
impl dyn Registrar + '_ {
    pub fn register_dimension(&mut self, name: &str, settings: DimensionSettings) {
        self.add_dimension(name, settings);
    }
    pub fn register_chunk_generator<T: ChunkGenerator>(&mut self, name: &str, generator: T) {
        self.add_chunk_generator(name, Arc::new(generator));
    }
    pub fn register_biome_source<T: BiomeSource>(&mut self, name: &str, source: T) {
        self.add_biome_source(name, Arc::new(source));
    }
}

pub trait Commands {
    fn feedback(&mut self, line: Text) -> Result<(), TextFormError>;
}

pub trait ServerHost {
    fn commands(&mut self) -> &mut dyn Commands;
    fn level(&self, dimension: &str) -> Option<LevelRef>;
    fn chunk(&self, level: LevelRef, pos: [i32; 2]) -> Option<ChunkRef>;
    fn level_height(&self, level: LevelRef) -> Option<(i32, i32)>;
    fn level_block(&self, level: LevelRef, pos: [i32; 3]) -> Option<BlockKey>;
    fn dimension_key_id(&self, key: DimensionKey) -> Option<String>;
    fn world_dir(&self) -> Option<PathBuf>;
    fn player_tell(&mut self, player: PlayerRef, line: Text) -> Result<bool, TextFormError>;
    fn entity_teleport_to_level(&mut self, entity: EntityRef, dimension: &str, pos: [f64; 3], rotation: Option<[f32; 2]>) -> bool;
}

pub trait Hooks {
    fn register(&self, registrar: &mut dyn Registrar, context: &RegistrationContext) {}
    fn server_started(&self, host: &mut dyn ServerHost, event: &mut ServerStartedEvent) {}
    fn register_commands(&self, host: &mut dyn ServerHost, event: &mut RegisterCommandsEvent<'_>) {}
    fn run_mod_command(&self, host: &mut dyn ServerHost, event: &mut RunModCommandEvent<'_>) -> Flow {
        Flow::Continue
    }
    fn server_tick(&self, host: &mut dyn ServerHost, event: &mut ServerTickEvent<'_>) {}
}
