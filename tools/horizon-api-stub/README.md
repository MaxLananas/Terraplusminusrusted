# Horizon mod API signature stub (development only)

The real SDK cannot be built in this environment: it requires Rust 1.95 and crates.io. This
stub declares only the items `horizon-mod/src/lib.rs` uses, with signatures copied from
`horizon-mod-vocab` at Horizon `2b77287de099c9316278ba7ac4d83590fdb7ef30` (`nightly-2026-11-22`). Its
bodies do nothing.

The mod was first written against `ffd59c4e0cebd16c0982016f040b3a9b70e8b83b`. That build has no
published SDK (`sdk-<commit>` release), so the launcher could not build the mod for it. The pin moved
to the dated Nightly above, which has an SDK with Windows x64, Linux and macOS slices. Between the two
commits, every source file in the table below is byte-identical, except `host/services.rs`. That file
only gains `RuntimeLevels::reset_vanilla`, which has a default body. The only removal is the
`RuntimeGenerator`/`RuntimeLevel` re-export, which the mod does not use.

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
| `LevelRef::online_players`, `PlayerRef::action_bar`, `PlayerRef::permission_level`, `LivingRef::equipment`, `ItemKey::id` | backend methods `level_online_players` (`host/levels.rs`, `LevelsBackend`), `player_action_bar` (`host/players.rs`, `PlayersBackendMut`), `player_permission_level` (`host/players.rs`, `PlayersBackend`), `living_equipment` (`host/living.rs`, `LivingBackend`), `item_key_id` (`host/keys.rs`, `KeysBackend`), by the same `handle_method` rule; the receiver is the handle type's snake-case name (`LivingRef` → `living`, `ItemKey` → `item_key`), as its `block_key_id` test shows |
| `Host::living`, `LivingRef` | `host/living.rs` (`LivingHost`), `entity/types.rs` |
| `ServerHost::players`, `Players` (`server_players`, `player_by_name`, `acting_player`) | `host/services.rs` (`ServicesServerHost`, `Players`) |
| `ItemStack::item`, `ItemKey`, `EquipmentSlot` | `item/item_stack.rs`, `crates/horizon-core/src/equipment_slot.rs` |
| `Command`, `PermissionLevel` | `command/types.rs` |
| `CommandSource`, `CommandInvocation`, `CommandArgumentValue` | `command/mod.rs`: every `CommandSource` variant; `CommandArgumentValue` up to `Players` (later variants omitted; it is `non_exhaustive`); `Identifier` is opaque here |
| `RunModCommandEvent { source: &CommandSource, command: &CommandInvocation }` | generated; field types from `crates/horizon-client/src/game/native_hooks/base.rs::native_run_mod_command` |
| `ServerTickEvent { phase }`, `ServerTickPhase` | generated; `world/types.rs`, `crates/horizon-mod-dimension-game/src/main.rs` |
| `Text`, `TextStyle`, `TextClick`, `TextHover` | `text/mod.rs` (`TextContent::Literal` does not interpret legacy `§` codes) |
| `Flow` | `runtime.rs` |
| `Hooks::server_stopping`, `ServerStoppingEvent` | `hooks/server_stopping.rs`; signature as implemented in `crates/horizon-client/src/game/world/server_lifecycle/server_lifecycle_tests.rs` |
| `Hooks::level_load`, `LevelLoadEvent { dimension: &str }` | `hooks/level_load.rs`; signature and field as used in `crates/horizon-client/src/game/world/runtime_levels_tests.rs` |
| `BlockState::of`, `StateConstraint`, `BlockStateError` | `block/block_state.rs`, `block/block_states.rs`, `block/block_metadata.rs` (the stub's `of` always returns `Unavailable`) |
