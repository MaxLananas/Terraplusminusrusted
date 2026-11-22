//! Engine-neutral TerraPlusMinus server behaviour: configuration, `/tpll` parsing and
//! authorization, teleport planning across linked worlds, and assembly of the generation stack.

pub mod config;
pub mod coordinates;
pub mod engine_bridge;
pub mod java_text;
pub mod messages;
pub mod region_index;
pub mod stack;
pub mod teleports;
pub mod tpll;

pub use coordinates::{LatLng, UpstreamParseOutcome, parse_coordinates, parse_coordinates_upstream};
pub use config::{ConfigError, NetworkConfig, PermissionLevels, ServerConfig, parse_server_config};
pub use tpll::{Authorization, LevelHeight, LinkedWorld, LinkedWorlds, LinkedWorldsMethod, TeamBounds, TeleportWorld, TpllRequest, TpllStep, TpllTarget, parse_tpll_arguments};
pub use stack::{StackError, TerraStack};
pub use teleports::{PendingTeleports, TeleportOutcome, TeleportRefusal};
pub use region_index::RegionIndex;
pub use engine_bridge::{BiomeLookup, BlockPalette, BlockWriter, GateState, GenerationCounters, GenerationGate, terrain_for_generation, write_chunk};
