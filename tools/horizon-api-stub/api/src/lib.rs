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
pub struct LivingRef(EntityRef);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ItemKey(u32);
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Identifier(String);
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ItemStack {
    item: ItemKey,
}
impl ItemStack {
    pub fn item(&self) -> ItemKey {
        self.item
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EquipmentSlot {
    MainHand,
    OffHand,
    Feet,
    Legs,
    Chest,
    Head,
    Body,
    Saddle,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ChunkPos {
    pub x: i32,
    pub z: i32,
}
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Text {
    pub content: String,
    pub style: TextStyle,
    pub children: Vec<Text>,
}
#[derive(Clone, Debug, PartialEq, Default)]
#[non_exhaustive]
pub struct TextStyle {
    pub color: Option<String>,
    pub shadow_color: Option<u32>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underlined: Option<bool>,
    pub strikethrough: Option<bool>,
    pub obfuscated: Option<bool>,
    pub click: Option<TextClick>,
    pub hover: Option<TextHover>,
    pub insertion: Option<String>,
    pub font: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextClick {
    OpenUrl(String),
    ChangePage(i32),
    SuggestCommand(String),
    RunCommand(String),
    CopyToClipboard(String),
}
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum TextHover {
    Text(Box<Text>),
}
impl Text {
    pub fn literal(value: impl Into<String>) -> Self {
        Self { content: value.into(), style: TextStyle::default(), children: Vec::new() }
    }
    pub fn append(mut self, child: Text) -> Self {
        self.children.push(child);
        self
    }
    pub fn with_style(mut self, style: TextStyle) -> Self {
        self.style = style;
        self
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
impl CommandSource {
    pub fn permission(&self) -> PermissionLevel {
        match self {
            Self::Player { permission, .. }
            | Self::Console { permission }
            | Self::CommandBlock { permission, .. }
            | Self::CommandBlockMinecart { permission, .. }
            | Self::Function { permission, .. } => *permission,
        }
    }
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
    CommandBlock { position: [i32; 3], level: DimensionKey, permission: PermissionLevel },
    CommandBlockMinecart { entity: EntityRef, permission: PermissionLevel },
    Function { id: Identifier, permission: PermissionLevel },
}
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum CommandArgumentValue {
    String(String),
    Integer(i32),
    Number(f64),
    Boolean(bool),
    Player(PlayerRef),
    Entity(EntityRef),
    Entities(Vec<EntityRef>),
    Players(Vec<PlayerRef>),
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
    pub source: &'a CommandSource,
    pub command: &'a CommandInvocation,
}
#[non_exhaustive]
pub struct RegisterCommandsEvent<'a> {
    pub commands: &'a mut Vec<Command>,
}
#[non_exhaustive]
pub struct ServerTickEvent<'a> {
    pub phase: ServerTickPhase,
    marker: std::marker::PhantomData<&'a ()>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ServerTickPhase {
    ServerTickStart { world_running: bool },
    ServerTickEnd { world_running: bool },
}
#[non_exhaustive]
pub struct ServerStartedEvent;
#[non_exhaustive]
pub struct ServerStoppingEvent;
#[non_exhaustive]
pub struct LevelLoadEvent<'a> {
    pub dimension: &'a str,
}

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

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct StateConstraint {
    pub name: String,
    pub value: String,
}
impl StateConstraint {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self { name: name.into(), value: value.into() }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum BlockStateError {
    Unavailable,
    UnknownBlock,
    UnknownState,
    UnknownProperty(u32),
    InvalidValue(u32),
    DuplicateProperty(u32),
    InvalidCombination,
}
impl BlockState {
    pub fn of(metadata: &dyn BlockMetadata, block: BlockKey, properties: &[StateConstraint]) -> Result<Self, BlockStateError> {
        Err(BlockStateError::Unavailable)
    }
}

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

/// The public host areas (`host/*.rs`, `*Host` traits); `*Backend` methods are reached only
/// through the handle methods below, as horizon-mod-api-gen's `handle_method` generates them.
pub trait Host {
    fn level(&self, dimension: &str) -> Option<LevelRef>;
    fn chunk(&self, level: LevelRef, pos: [i32; 2]) -> Option<ChunkRef>;
    fn living(&self, entity: EntityRef) -> Option<LivingRef>;
}
pub trait Players {
    fn server_players(&self) -> Vec<PlayerRef>;
    fn player_by_name(&self, name: &str) -> Option<PlayerRef>;
    fn acting_player(&self) -> Option<PlayerRef>;
}
pub trait ServerHost: Host {
    fn commands(&mut self) -> &mut dyn Commands;
    fn world_dir(&self) -> Option<PathBuf>;
    fn players(&self) -> &dyn Players;
}

impl LevelRef {
    pub fn height(self, host: &dyn Host) -> Option<(i32, i32)> {
        None
    }
    pub fn block(self, host: &dyn Host, pos: [i32; 3]) -> Option<BlockKey> {
        None
    }
    pub fn online_players(self, host: &dyn Host) -> Option<Vec<PlayerRef>> {
        None
    }
}
impl LivingRef {
    pub fn equipment(self, host: &dyn Host, slot: EquipmentSlot) -> Option<ItemStack> {
        None
    }
}
impl ItemKey {
    pub fn id(self, host: &dyn Host) -> String {
        String::new()
    }
}
impl DimensionKey {
    pub fn id(self, host: &dyn Host) -> Option<String> {
        None
    }
}
impl EntityRef {
    pub fn position(self, host: &dyn Host) -> Option<[f64; 3]> {
        None
    }
    pub fn teleport_to_level(self, host: &mut dyn ServerHost, dimension: &str, pos: [f64; 3], rotation: Option<[f32; 2]>) -> bool {
        false
    }
}
impl PlayerRef {
    pub fn tell(self, host: &mut dyn ServerHost, line: Text) -> Result<bool, TextFormError> {
        Ok(false)
    }
    pub fn action_bar(self, host: &mut dyn ServerHost, line: Text) -> Result<bool, TextFormError> {
        Ok(false)
    }
    pub fn permission_level(self, host: &dyn Host) -> Option<u8> {
        None
    }
}

pub trait Hooks {
    fn register(&self, registrar: &mut dyn Registrar, context: &RegistrationContext) {}
    fn server_started(&self, host: &mut dyn ServerHost, event: &mut ServerStartedEvent) {}
    fn server_stopping(&self, host: &mut dyn ServerHost, event: &mut ServerStoppingEvent) {}
    fn level_load(&self, host: &mut dyn ServerHost, event: &mut LevelLoadEvent<'_>) {}
    fn register_commands(&self, host: &mut dyn ServerHost, event: &mut RegisterCommandsEvent<'_>) {}
    fn run_mod_command(&self, host: &mut dyn ServerHost, event: &mut RunModCommandEvent<'_>) -> Flow {
        Flow::Continue
    }
    fn server_tick(&self, host: &mut dyn ServerHost, event: &mut ServerTickEvent<'_>) {}
}
