# Feature matrix

Upstream revisions are listed in [upstream-pins.md](upstream-pins.md). "TPM" is TerraPlusMinus,
"TMM" is terraminusminus. Paths without a crate prefix are under `crates/`.

## Status values

| Status | Meaning |
|---|---|
| Verified | Ported, with a differential test against the Java oracle or a test of the upstream contract. |
| Implemented | Ported and unit or integration tested, but without an oracle comparison. |
| Horizon: stub-checked | The Horizon adapter type-checks against the API stub (`tools/horizon-api-stub`), but has never run inside Horizon. |
| Not ported | Absent. Listed on purpose so that nothing is silently missing. |

None of the Horizon rows has run in a real Horizon server, because Horizon cannot be built in
this environment. See [Limits](#limits).

## Projection

| Upstream symbol | Behavior | Rust implementation | Horizon integration | Acceptance test | Status |
|---|---|---|---|---|---|
| TMM `DymaxionProjection` | Icosahedral Dymaxion forward and inverse | `terra-projection/src/dymaxion.rs` | Used through `TerraStack` | `terra-projection/tests/oracle_differential.rs::dymaxion_from_geo_matches_upstream` | Verified |
| TMM `ConformalDymaxionProjection` | Conformal variant driven by `conformal.gz`, with Newton inverse | `dymaxion.rs`, `resources/conformal.gz` | Same | `conformal_dymaxion_from_geo_matches_upstream` | Verified |
| TMM `BTEDymaxionProjection` | BTE projection: Alaska and Iceland adjustments | `dymaxion.rs::BteDymaxionProjection` | Same | `bte_from_geo_matches_upstream`, `bte_to_geo_matches_upstream` (max 7.45e-9 blocks) | Verified |
| TMM `ScaleProjectionTransform`, `FlipVerticalProjectionTransform`, `OffsetProjectionTransform` | Scale (7318261.522857145), flip, offset | `transform.rs` | Same | `offset_bte_matches_upstream`, `tests.rs::offset_shifts_projected_coordinates_exactly` | Verified |
| TMM `WebMercatorProjection` | Tile maths for raster datasets | `mercator.rs` | Same | `web_mercator_matches_upstream` | Verified; zoom above 22 rejected |
| TMM `GeographicProjection.bounds`, `.vector`, `.tissot` | Bounds, displacement, Tissot indicatrix | `lib.rs::GeographicProjection` | `/distortion` | `bte_tissot_matches_upstream` (400 points, relative error at most 3.1e-7) | Verified |
| TMM `OutOfProjectionBoundsException` | Out-of-domain inputs fail | `ProjectionResult` errors | Refusals in `/tpll` and `/where` | `tests.rs::rejects_out_of_range_geographic_input` | Verified |

## Elevation and interpolation

| Upstream symbol | Behavior | Rust implementation | Horizon integration | Acceptance test | Status |
|---|---|---|---|---|---|
| TMM `heights.json5`, `DoubleTiledDataset` layers | Layered provider configuration, conditions and bounds | `terra-elevation/src/config.rs`, `condition.rs`, `bounds.rs`, `layered.rs` | Loaded from `elevation.heights_config` | `config.rs::default_world_layers_load`, `nested_conditions_and_multiple_bounds` | Implemented |
| TMM terrarium PNG decoding | `(r*256 + g + b/256) - 32768` | `terrarium.rs`, `terra-codec/src/png.rs` | Same | `terrarium_decode_is_bit_exact` | Verified |
| TMM `BlendMode` (NEAR, LINEAR, CUBIC, SMOOTH) | Sub-tile interpolation | `blend.rs` | Same | `blend_modes_are_bit_exact` (0 of 18000 differ) | Verified |
| TMM `HeightsBaker` and chunk sampling | Per-chunk 16x16 heights, including offsets | `chunk.rs` | `EarthGenerator::build_terrain` | `chunk_heights_match_upstream`, `offset_chunk_heights_match_upstream` (625 chunks, 0 mismatches) | Verified |
| TMM GeoTIFF datasets (`parse_tiff`, `parse_tiff_auto`, `parse_tiff_float`, `parse_tiff_int`) | TIFF elevation and tree-cover layers: first grayscale directory, width and height equal to the resolution, GDAL_NODATA as NaN | `terra-codec/src/tiff.rs` (classic TIFF; none, LZW, Deflate, PackBits; predictors 2 and 3; 8/16/32-bit integers and 32/64-bit floats held as f32), `terra-elevation/src/dataset.rs` | Same as terrarium layers | `terra-codec/tests/tiff_reference.rs`: 16 files written by `tifffile` (`fixtures/tiff`) decode bit for bit; an LZW code-width mutation is caught | Verified against tifffile, not against Java (upstream decodes with Apache Commons Imaging) |
| TMM parser chain (`add`, `multiply`, `divide`, `flip_x`, `flip_z`, `swap_axes`) and projection transforms (`scale`, `offset`, `flip_*`, `swap_axes`) | Post-decoding arithmetic and transformed tile projections; `divide` multiplies by the reciprocal as upstream | `dataset.rs::TileFormat`, `config.rs` | Same | `config.rs::nested_parsers_and_projection_transforms_load`, `default_tree_cover_loads_with_its_parser_chain_and_scaled_projection`; upstream's complete `heights.json5` (1979 layers) loads | Implemented |
| TMM `from_int` parser, BigTIFF, equirectangular-style projections | Other formats | Refused or skipped with a reason (`elevation.unsupported_layers`) | `/terraplusminus status` lists skipped layers | `config.rs::unported_layers_are_never_silently_dropped` | Not ported |
| TMM `zooms` key | Multi-zoom selection | Parsed, unused | n/a | n/a | Not ported |

## Provider acquisition and caching

| Upstream symbol | Behavior | Rust implementation | Horizon integration | Acceptance test | Status |
|---|---|---|---|---|---|
| TMM `Http` | Bounded concurrent fetching with retries | `terra-acquisition/src/http.rs`, `policy.rs` | Worker threads only, never on the tick | `loopback_http.rs::fetches_through_retries_and_reports_missing`, `stalled_server_times_out`, `policy.rs::*` | Implemented |
| Rate limits (429, `Retry-After`) | Back off per host | `policy.rs` | Same | `rate_limit_cooldown_applies_to_the_host` | Implemented |
| HTTPS | TLS fetching | Refused, never downgraded. `plain_http_hosts` or a mirror is required; a configuration with an enabled source that cannot be fetched is refused before the dimension registers (`stack.rs::check_sources`) | Same | `https_is_refused_not_downgraded`, `generation_failures.rs::https_only_sources_are_refused_before_anything_starts` | Not ported (no TLS without crates.io) |
| Transport deadlines | One hard deadline per exchange (DNS, connect, write, read) and per fetch (queueing, attempts, backoff) | `http.rs` (`resolve`, `DeadlineStream`), `policy.rs` (`fetch_deadline`) | Same | `loopback_http.rs::total_timeout_is_a_hard_deadline_even_with_a_long_io_timeout`, `a_dripping_server_cannot_extend_the_deadline`, `one_deadline_covers_every_attempt_and_backoff`, `a_stalled_attempt_is_cut_to_the_remaining_fetch_deadline` | Implemented; the DNS bound abandons the resolver thread, which ends on its own |
| Generation failure | Permanent errors fail at once, transient ones retry `generation.retry_attempts` times, stopping cancels waits and backoffs, a refused or poisoned gate fails | `engine_bridge.rs` (`GenerationGate`, `GenerationFailure`, `terrain_for_generation`), `loader.rs::shut_down` | `build_terrain` panics on failure (caught by Horizon's job runner, chunk not published or saved); `level_load` checks the fingerprint, `server_stopping` cancels | `generation_failures.rs::*`, `engine_bridge.rs::tests::*gate*` | Implemented; Horizon: stub-checked, engine behaviour read from source only |
| TMM `Disk` cache | On-disk tile cache | `disk_cache.rs`: atomic publication, versioned keys, LRU byte budget | `<world>/horizon-mod-data/terraplusminus` and the configured cache directory | `round_trips_and_survives_reopen`, `detects_corruption_and_discards_partial_writes`, `evicts_least_recently_used_beyond_budget` | Implemented |
| TPM chunk cache (expire after 5 min access) | RAM cache of baked chunks | `terra-chunk-generation/src/loader.rs`: bounded queue, dedup, cancellation | Same | `loader_concurrency.rs::*` (5 tests) | Implemented |
| Decompression bombs and path traversal | Defensive input handling | `terra-codec` output limits; `routing.rs` | Same | `deflate.rs::output_limit_is_enforced`, `png.rs::rejects_corruption_and_limits`, `routing.rs::routes_mirrors_rewrites_listed_hosts_and_refuses_others` | Implemented |

## Terrain generation

| Upstream symbol | Behavior | Rust implementation | Horizon integration | Acceptance test | Status |
|---|---|---|---|---|---|
| TPM `RealWorldGenerator.generateNoise` and `generateSurface` | Stone column, surface block, water to sea level | `generator.rs`, `chunk_data.rs` | `EarthGenerator::build_terrain` writes through `ChunkAccess::set_block` (no post-generation edits) | `offline_area.rs::generates_real_terrain_with_continuous_seams`, `block_columns_are_stone_capped_by_surface`, `engine_bridge.rs::writes_real_columns_up_to_the_surface` | Implemented; Horizon: stub-checked |
| Missing and NaN heights | TMM defaults (shallow ocean, sea-level column) | `chunk_data.rs` | Same | `missing_heights_become_shallow_ocean`, `nan_heights_are_sea_level_columns` | Implemented |
| Negative coordinates, chunk borders | Floor division everywhere | Typed coordinates in `terra-projection`, `chunk_data.rs` | Same | `negative_heights_floor_toward_negative_infinity`, `null_island_spans_six_chunks_per_axis`, `region_index.rs::reads_saved_chunks_including_negative_coordinates` | Implemented |
| TPM `BlockMapper` and `surface_material` | Configurable surface block | `generator.rs` | Block keys resolved at registration | `upstream_keys_and_extensions_parse` | Implemented |
| Paper `buildSurface` RNG | `LegacyRandomSource(0)` seeded per column | `java_random.rs` | Same | `matches_java_util_random_sequences` | Implemented |
| Deterministic output | Independent of scheduling | Pure function of configuration and chunk position | Same | `generation_is_independent_of_order_and_threads`, `output_does_not_depend_on_worker_count_or_request_order` | Implemented |
| Generation fingerprint | Refuse reuse with incompatible settings | `fingerprint.rs`, `engine_bridge.rs::GenerationGate` | Checked in `server_started`; generation stays closed on mismatch | `records_then_matches_then_refuses_changes`, `gate_follows_fingerprint_outcomes` | Implemented |
| Never overwrite player builds | Generation touches only new chunks | Horizon calls `build_terrain` only for chunks without saved data | Same | Engine contract (`world/chunk_generator.rs`) | Horizon: stub-checked |
| Lighting, heightmaps, generation states, persistence | Engine-owned | Not reimplemented | Horizon runs these after `build_terrain` | Needs a real Horizon run | Horizon: stub-checked |

## Biomes and vegetation

| Upstream symbol | Behavior | Rust implementation | Horizon integration | Acceptance test | Status |
|---|---|---|---|---|---|
| TPM `CustomBiomeProvider` and `koppen_map.gz` | Köppen climate to biome | `biome.rs`; `koppen_map.gz` byte-identical to upstream | `EarthBiomes::noise_biome` through `register_biome_source` | `koppen_biomes_cover_the_offline_area`, `biome_lookup_matches_the_generator_and_ignores_height` | Implemented; surface biome is quart-aligned |
| `biomes.use_dataset = false` | One configured biome everywhere | `biome.rs` | Same | `upstream_keys_and_extensions_parse` | Implemented |
| TMM `TreeCoverBaker`, `NoTrees`, `NullIslandBaker` (tree cover) | Per-column tree chance `ceil(v/4*255*1.5)`, 0 for missing data, cleared by OSM and null island, 255 when not baked | `chunk_data.rs` (`tree_chance`, `bake_tree_cover`), `generator.rs::chunk_terrain` (sampled only when trees are on, unlike upstream, see TPM issue #77) | `elevation.tree_cover_config`, upstream `tree_cover.json5` by default | `chunk_data.rs::tree_chance_matches_tree_cover_baker`, `tree_cover_is_cleared_by_no_trees_and_null_island`; `terra-server/tests/trees.rs` (synthetic LZW TIFF tiles in `fixtures/treecover`, including GDAL no-data) | Implemented |
| TPM `TreePopulator` | Custom schematics from `customTrees.json` by Köppen class, positions from Paper's decoration random, water abort, height limits, no trees on OSM surface blocks, quarter-turn rotations | `terra-chunk-generation/src/trees.rs`, `java_random.rs::decoration`, `next_bytes`; `customTrees.json` byte-identical | `write_trees` after `write_chunk` in `build_terrain`; each chunk replays its 8 neighbours' populators and keeps its own blocks (no populator hook in Horizon); tree blocks registered in the palette | `java_random.rs::decoration_seed_and_next_bytes_match_java` (JDK values); `trees.rs` unit tests (6 mutations caught); `terra-server/tests/trees.rs` | Implemented with documented differences: seeded variant and rotation (upstream: unseeded `new Random()`), per-chunk zeroed density buffer (upstream: thread-local, order-dependent), climate at the tree column (upstream: last computed biome), persistent leaves. No Java oracle: upstream's populator needs Bukkit's `LimitedRegion` |
| TMM OSM pipeline and `osm.json5` | Roads, buildings, water polygons from OpenStreetMap: GeoJSON tiles and references, the mapper language, narrow, sharp, wide and polygon drawing, `OSMBaker` ordering | `crates/terra-osm` (`geojson.rs`, `mapper.rs`, `geometry.rs`, `draw.rs`, `dataset.rs`); bundled `osm.json5` byte-identical to upstream | `openstreetmap` config section; `osm.json5` extracted next to the config; tiles fetched on worldgen workers | `terra-chunk-generation/tests/osm_oracle.rs`: mapped elements and all 51,200 drawn columns (surface block, water depth byte, tree cover) of four scenes match the Java oracle, including a custom mapping that saturates the depth byte; `terra-osm/tests/dataset.rs`; `terra-server/tests/openstreetmap.rs` | Verified; needs a plain-HTTP or mirrored server (no TLS) |
| Oceans, lakes, rivers, coastlines | Upstream water comes only from the OSM baker (water polygons, coastlines) and from NaN or missing heights; the generator fills water up to `waterHeight` | OSM water depth applied in `chunk_data.rs`; NaN and missing heights as upstream | Same | `osm_oracle.rs` (`caspian_edge` and `southern_coast` scenes: inland-sea and ocean coastlines), `missing_heights_become_shallow_ocean`, `nan_heights_are_sea_level_columns`, `inland_water_lowers_ground_only` | Verified |
| TPM `BlockMapper` deprecated keys `building_outlines_material`, `road_material`, `path_material` | Replace the block drawn for `minecraft:bricks`, `minecraft:gray_concrete`, `minecraft:dirt_path`, with a deprecation warning | `config.rs` (`surface_replacements`, `warnings`), `generator.rs` | Warnings logged at registration | `config.rs::openstreetmap_settings_and_deprecated_materials` | Implemented; unknown materials are not detected (upstream skips them with a warning) |

## LiDAR, offsets, bounds and heights

| Upstream symbol | Behavior | Rust implementation | Horizon integration | Acceptance test | Status |
|---|---|---|---|---|---|
| LiDAR datasets | High-resolution layers in `heights.json5` | TIFF and terrarium PNG layers load like any other layer | Same | Upstream's complete `heights.json5` loads | Implemented; no real LiDAR tile has been decoded in tests (no network access to the TLS hosts) |
| `terrain_offset.x/y/z` | Shift world against projection and height | `config.rs`, `OffsetProjectionTransform` | Dimension `sea_level = 63 + y` | `offset_chunk_heights_match_upstream`, `offset_bte_matches_upstream` | Verified |
| `min/max_latitude`, `min/max_longitude` | Team bounds for `/tpll` | `tpll.rs` | `/tpll` | `tpll_flow.rs::enforces_authorization_and_bounds` | Implemented |
| Extended heights (`world-height-datapack.zip`) | Build height up to 2032 | `world.min_y`, `world.height` → `DimensionSettings` | `register_dimension` | `config.rs::rejects_unknown_keys_and_invalid_values` (range checks) | Implemented; Horizon: stub-checked |

## Linked worlds

| Upstream symbol | Behavior | Rust implementation | Horizon integration | Acceptance test | Status |
|---|---|---|---|---|---|
| `linked_worlds` with method MULTIVERSE | Stacked levels with Y offsets; `/tpll` routes above max or at/below min | `tpll.rs::linked_world`, config method `Levels` | Levels by dimension id | `tpll_flow.rs::explicit_heights_route_through_linked_worlds` | Implemented |
| Method SERVER (proxy plugin message) | Cross-server routing | Refused by the config parser | n/a | `rejects_unknown_keys_and_invalid_values` | Not ported |
| `PlayerMoveEvent` auto-teleport between linked worlds | Move players across level boundaries while walking | Not implemented | n/a | n/a | Not ported |

## Commands, permissions and teleportation

| Upstream symbol | Behavior | Rust implementation | Horizon integration | Acceptance test | Status |
|---|---|---|---|---|---|
| TPM `TpllCommand` coordinate parsing | Decimal, DMS and suffix formats; height | `coordinates.rs`, `java_text.rs`, `tpll.rs` | `/tpll <coordinates:text>` and alias `/tpc` | `coordinate_oracle.rs::matches_upstream_on_every_recorded_input` (245 of 245) | Verified |
| TPM `TpllCommand` flow | Height lookup, ungenerated-chunk permission, linked worlds, messages | `tpll.rs`, `teleports.rs`, `messages.rs` | Off-tick surface loading; `entity.teleport_to_level` | `tpll_flow.rs::*` (4 tests), `messages.rs::teleport_lines_follow_upstream_number_formatting` | Implemented; Horizon: stub-checked |
| `/tpll -p <player>` (`forcetpll`) | Teleport other players | Not implemented | n/a | n/a | Not ported |
| TPM `WhereCommand` | Geographic position plus a Google Maps link | `messages.rs::where_message` | `/where`, click and hover style | `where_links_the_reported_coordinates`, `command_text_oracle.rs::matches_java_double_text` | Implemented; Horizon: stub-checked |
| TPM `OffsetCommand` | Offsets, or Y per linked level | `messages.rs::offset_lines` | `/offset` | `offsets_list_linked_levels_instead_of_y` | Implemented; Horizon: stub-checked |
| TPM `DistortionCommand` | Tissot-based scale and angle report | `messages.rs::distortion_message` | `/distortion` | `bte_tissot_matches_upstream`, `matches_java_double_text` (`%.6f`, `%.4f`) | Implemented; Horizon: stub-checked |
| TPM `Permission` nodes | `t+-.tpll`, `.tpll.otherWorld`, `.tpll.ungenerated-chunks`, `.admin`, `.where`, `.offset`, `.distortion` | `config.rs::PermissionLevels` maps nodes to vanilla operator levels | `Command::op(usage, level)` plus checks in the handlers | `enforces_authorization_and_bounds` | Implemented (operator levels replace permission plugins) |
| `t+-.autoteleport`, `t+-.notify.update` | Auto-teleport, update notices | Not implemented | n/a | n/a | Not ported |
| Teleport readiness | Never teleport into ungenerated or unloaded terrain | `teleports.rs`: bounded pending table, timeout, replaceable | Polled at `ServerTickPhase::ServerTickStart`; refused or dropped while generation is not open | `pending_teleports_are_bounded_and_replaceable`, `the_exclusive_top_of_the_world_is_a_valid_feet_position` | Implemented; Horizon: stub-checked |
| Diagnostics | Queue, cache and fingerprint state | `TerraStack::diagnostics_report` | `/terraplusminus status` (diagnostics level) | Exercised by `terra-cli benchmark` | Implemented |
| `PlayerJoinEvent`, `PlayerCommandPreprocessEvent`, `PluginMessageEvent` | Join handling, command rewriting, proxy messages | Not implemented | n/a | n/a | Not ported |

## Limits

- **The Horizon deliverable is incomplete.** There has been no real SDK build and no server run,
  so joining, generation in the engine, lighting, collision, teleport, save, stop and restart
  are all unverified. It must not be called complete or production-ready.
- Formatting is unverified: no rustfmt for the pinned toolchain is reachable, and
  `scripts/release-build.sh` fails on that by design.
- Mirrors (`network.mirrors`) are trusted to hold the same data as the URLs they replace. The
  fingerprint records the configured URLs, not the tile contents.

- Horizon is not compiled or run here: it needs Rust 1.95 and crates.io, and neither is reachable.
  The adapter is only type-checked against a hand-written stub whose signatures cite the SDK
  sources (`tools/horizon-api-stub/README.md`).
- So no bot join, lighting or collision check, save/restart, or in-engine teleport has been run.
- Live provider tests are opt-in (`scripts/verify.sh --live`) and were not run, because provider
  hosts are unreachable from this environment.
