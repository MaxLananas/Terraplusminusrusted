# Horizon mod API signature stub (development only)

The real SDK cannot be built in this environment: it requires Rust 1.95 and crates.io. This
stub declares only the items `horizon-mod/src/lib.rs` uses, with signatures copied from
`horizon-mod-vocab` at Horizon `ffd59c4e0cebd16c0982016f040b3a9b70e8b83b`. Its bodies do
nothing.

`scripts/check-horizon-mod.sh` stages the mod with this stub in place of
`.horizon/sdk/api/crates/horizon-mod-api`, then type-checks and lints it. That catches mistakes
in the mod's own code. It does not prove the mod compiles against the real SDK, and it is never
part of a mod package.

## Where each signature comes from

Paths are relative to `crates/horizon-mod-vocab/src` unless noted.

| Stub item | Source |
|---|---|
| `ChunkAccess`, `ChunkGenerator` | `world/chunk_generator.rs` (`vanilla_stages` omitted; it has a default) |
| `BiomeSource` | `world/biome_source.rs` |
| `DimensionSettings` (subset of fields) | `world/dimension.rs`; the real struct has more fields and implements `Default` |
| `register_dimension`, `register_chunk_generator`, `register_biome_source` | `parts/dimensions.rs` (`impl dyn Registrar`) |
| `Registrar::block_key` | `parts/blocks.rs` (`BlocksRegistrar`, part of the generated `Registrar` union) |
| `RegistrationContext::instance_dir`, `InstanceDirectory` | `world/phase.rs`, `instance_directory.rs` |
| `Host::level`, `Host::chunk` | `host/entities.rs` (`EntitiesHost`), `host/levels.rs` (`LevelsHost`) |
| `ServerHost::commands`, `ServerHost::world_dir` | `host/services.rs`, `host/config.rs` (`ConfigServerHost`) |
| `LevelRef::height`, `LevelRef::block`, `DimensionKey::id`, `EntityRef::position`, `EntityRef::teleport_to_level`, `PlayerRef::tell` | backend methods `level_height`, `level_block`, `dimension_key_id`, `entity_position`, `entity_teleport_to_level`, `player_tell`, exposed as handle methods by `crates/horizon-mod-api-gen/src/host.rs::handle_method` (reads take `&dyn Host`, writes `&mut dyn ServerHost`) |
| `Command`, `PermissionLevel` | `command/types.rs` |
| `CommandSource`, `CommandInvocation`, `CommandArgumentValue` | `command/mod.rs` (variants beyond those used omitted; all are `non_exhaustive`) |
| `RunModCommandEvent { source: &CommandSource, command: &CommandInvocation }` | generated; field types from `crates/horizon-client/src/game/native_hooks/base.rs::native_run_mod_command` |
| `ServerTickEvent { phase }`, `ServerTickPhase` | generated; `world/types.rs`, `crates/horizon-mod-dimension-game/src/main.rs` |
| `Text`, `TextStyle`, `TextClick`, `TextHover` | `text/mod.rs` (`TextContent::Literal` does not interpret legacy `§` codes) |
| `Flow` | `runtime.rs` |
