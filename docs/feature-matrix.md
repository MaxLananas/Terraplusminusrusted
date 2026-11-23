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
| TMM GeoTIFF datasets (`parse_tiff`, `parse_tiff_auto`, `parse_tiff_float`, `parse_tiff_int`) | TIFF elevation and tree-cover layers: first grayscale directory, width and height equal to the resolution, GDAL_NODATA as NaN | `terra-codec/src/tiff.rs` (classic TIFF and BigTIFF; none, LZW, Deflate, PackBits; predictors 2 and 3; 8/16/32-bit integers and 32/64-bit floats held as f32; signed 16-bit differencing summed without wrapping, as Commons Imaging does), `terra-elevation/src/dataset.rs` | Same as terrarium layers | `terra-elevation/tests/image_parsers_oracle.rs`: every raster upstream's `ParseTiffDSP` reads (`fixtures/images/golden/raster.tsv`, dumped by the oracle with Commons Imaging 1.0.0-alpha5 at the pinned commit) gives the same doubles bit for bit, GDAL no-data, BigTIFF, both byte orders and the predictor overflow included; `terra-codec/tests/tiff_reference.rs`: 16 files written by `tifffile` decode bit for bit | Verified against Java. The port also reads what Commons Imaging's raster reader refuses (unsigned and 8-bit integers, a missing `SampleFormat`, which is how the bundled tree-cover tiles and many GDAL files are written); and it reads the true strip offset of a big-endian BigTIFF with a single strip, where Commons Imaging reads the high half of the LONG8 and decodes the header as pixels |
| TMM parser chain (`add`, `multiply`, `divide`, `flip_x`, `flip_z`, `swap_axes`) and projection transforms (`scale`, `offset`, `flip_*`, `swap_axes`) | Post-decoding arithmetic and transformed tile projections; `divide` multiplies by the reciprocal as upstream | `dataset.rs::TileFormat`, `config.rs` | Same | `config.rs::nested_parsers_and_projection_transforms_load`, `default_tree_cover_loads_with_its_parser_chain_and_scaled_projection`; upstream's complete `heights.json5` (1979 layers) loads | Implemented |
| TMM integer parsers (`parse_png`, `parse_jpg`, `parse_tiff`, `add`, `and`, `flip_x`, `flip_z`, `swap_axes`, `grayscale_extract`, `rgb_extract`, `require_opaque`) and the `from_int` double parser | ARGB images turned into heights | `terra-elevation/src/int_format.rs`; images decoded as `ImageIO`/`BufferedImage.getRGB` would (`terra-codec/src/png.rs`, `jpeg.rs`: baseline and progressive, Huffman, restart markers, chroma subsampling, Adobe/YCCK/CMYK) and as Commons Imaging's `TiffImageParser` would (`tiff_rgb.rs`: bilevel, gray, RGB, palette, CMYK, YCbCr, 1 to 32-bit and float samples, associated and unassociated alpha, planar RGB) | Same as other layers; `elevation.unsupported_layers` stays accepted but nothing is skipped any more | `terra-elevation/tests/image_parsers_oracle.rs`: every chain on every square fixture against upstream's classes (`fixtures/images/golden/parsers.tsv`), failures included; `terra-codec/tests/image_oracle.rs`: 103 images (PNG, JPEG, TIFF edge cases) pixel for pixel against the JDK and Commons Imaging; `config.rs::every_integer_parser_chain_loads` | Verified. Refused on purpose: JPEG-compressed TIFF (old and new style), CCITT fax, CIELab and LogLuv TIFF; upstream decodes them, the port reports the layer's tiles as failed |
| TMM `zooms` key | Multi-zoom selection | Required and parsed, then unused, exactly as upstream: `MultiScalarDataset.WrappedDataset.zooms` is marked `//TODO: use this` and never read (`MultiScalarDataset.java:194`) | n/a | `config.rs` (a layer without `zooms` is refused, as Jackson's `required = true`) | Implemented (identical: upstream ignores it too) |

## Provider acquisition and caching

| Upstream symbol | Behavior | Rust implementation | Horizon integration | Acceptance test | Status |
|---|---|---|---|---|---|
| TMM `Http` | Bounded concurrent fetching with retries | `terra-acquisition/src/http.rs`, `policy.rs` | Worker threads only, never on the tick | `loopback_http.rs::fetches_through_retries_and_reports_missing`, `stalled_server_times_out`, `policy.rs::*` | Implemented |
| Rate limits (429, `Retry-After`) | Back off per host | `policy.rs` | Same | `rate_limit_cooldown_applies_to_the_host` | Implemented |
| HTTPS | TLS fetching | `terra-acquisition/src/external.rs`: `CurlTransport` runs the system `curl` (`network.https_command`, default `curl`; certificates checked by curl against the system store) with hard limits (`--max-time`, `--max-filesize`, `--proto =https`, the URL after `--`, no shell); `--dump-header` keeps status codes and `Retry-After`. `DualTransport` sends `http://` to the built-in client. Without curl, HTTPS is refused, never downgraded; `plain_http_hosts` and mirrors still work and a configuration with an unfetchable source is refused before the dimension registers | Same | `external.rs::dumped_headers_are_split_from_the_body`, `arguments_are_bounded_and_end_with_the_url`, `live_https_through_curl` (ignored; run against api.github.com: 200 and 404), `routing.rs::routes_mirrors_rewrites_listed_hosts_and_refuses_others`, `generation_failures.rs::https_only_sources_are_refused_before_anything_starts` | Implemented (through the system curl) |
| Transport deadlines | One hard deadline per exchange (DNS, connect, write, read) and per fetch (queueing, attempts, backoff) | `http.rs` (`resolve`, `DeadlineStream`), `policy.rs` (`fetch_deadline`) | Same | `loopback_http.rs::total_timeout_is_a_hard_deadline_even_with_a_long_io_timeout`, `a_dripping_server_cannot_extend_the_deadline`, `one_deadline_covers_every_attempt_and_backoff`, `a_stalled_attempt_is_cut_to_the_remaining_fetch_deadline` | Implemented; the DNS bound abandons the resolver thread, which ends on its own |
| TMM `http.maxConcurrentRequests` | Requests in flight per host: 8 for cloud.daporkchop.net and s3.amazonaws.com, 1 for gis-treecover.wri.org and each Overpass host | `policy.rs::parse_host_limit` (upstream's `"<n>: <url>"` entries), `HttpTileFetcher::with_host_limits`; other hosts use `network.max_per_host` | `network.max_concurrent_requests`, default upstream's list | `policy.rs::host_limits_follow_terraminusminus`, `config.rs::network_parity_keys_and_updates` | Implemented (hosts matched by authority, so a host moved to plain HTTP keeps its limit) |
| TMM `http.cacheTTL` (`CacheEntry`) | Cached responses expire after 1440 minutes | `disk_cache.rs`: `found_ttl` and `missing_ttl` (default 1440 min); a stale tile is fetched again and served anyway when that fetch fails (stale-if-error) | `network.cache_ttl_minutes` (0 keeps tiles until evicted) | `disk_cache.rs::stale_tiles_are_refreshed_and_served_when_the_source_fails` | Implemented (ETag revalidation is not done: an expired tile is fetched whole) |
| `Terraplusminus.createHttpUserAgent` | `<plugin>/<version> (<library>/<version>; +<website>)` on every request | `http.rs::DEFAULT_USER_AGENT` (`Terraplusminus/<version> (Terra--/rust; +<repository>)`), sent by the plain client and curl | `network.user_agent` | `config.rs::network_parity_keys_and_updates` | Implemented |
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
| `height_datapack` | Installs the datapack (overworld -64..2032) | Selects `DATAPACK_WORLD` (-64..2032) for the dimensions; a different `world` section is refused | `register_dimension` | `config.rs::height_datapack_selects_the_datapack_overworld` | Implemented (no datapack needed: the mod registers its own dimensions) |

## Linked worlds

| Upstream symbol | Behavior | Rust implementation | Horizon integration | Acceptance test | Status |
|---|---|---|---|---|---|
| `linked_worlds` with method MULTIVERSE | Stacked levels with Y offsets; `/tpll` routes above max or at/below min | `tpll.rs::linked_world`, config method `Levels`; names are matched without case (`LinkedWorldLayout.indexOf`) | Levels by dimension id | `tpll_flow.rs::explicit_heights_route_through_linked_worlds` | Implemented |
| `getDefaultWorldGenerator` per linked world | Each linked world runs `RealWorldGenerator` at its offset (0 means `terrain_offset.y`) | `ServerConfig::levels`, `level_offset`; `write_chunk_at`, `tree_blocks_at` (one data stack, offset applied when writing); per-level fingerprint (`TerraStack::verify_level`) | One `terraplusminus:<name>` dimension per linked level, `sea_level = 63 + offset`; linked names must be levels of this mod | `config.rs::linked_levels_are_dimensions_of_this_mod`, `trees.rs::a_linked_level_is_the_same_terrain_and_trees_shifted_by_its_offset` | Implemented; Horizon: stub-checked |
| `LinkedWorldLayout.validate`, `ConfigurationHelper` placeholders | Duplicates and non-decreasing offsets refused, fewer than 2 worlds warned, sample entries dropped | `config.rs` | Config load | `config.rs::linked_worlds_follow_upstream_validation` | Implemented (unloaded-world and generator warnings do not apply: the mod creates the levels) |
| Method SERVER (proxy plugin message) | Cross-server routing through TerraPlusMinusProxyBridge | `bridge.rs`: Java `writeUTF`/`readUTF` (modified UTF-8), `UUID.toString`, the request (`uuid`, `"<server>, <offset>"`, `"<lat>, <lng>"`) and delivery layouts, `GetServer`, `neighbour_server`; `TpllStep::LinkedServer` from `tpll.rs`; message `Teleporting to linked server...` | Raw play channels `terraplusminus:teleportbridge` and `bungeecord:main` (`PayloadRegistrar::play_raw`), sent with `PlayerRef::send_payload`; this server's name from the proxy's `GetServer` answer, or `linked_worlds.server_name` until it comes | `bridge.rs::*` (5 tests), `config.rs::upstream_switches_parse_with_upstream_defaults`, `messages.rs::passthrough_rewrites_like_player_command_event` | Implemented; Horizon: stub-checked |
| `PlayerMoveEvent` auto-teleport between linked worlds (`t+-.autoteleport`) | Move players across level boundaries while walking or flying | `autoteleport.rs`: `LinkedWorldLayout` thresholds (upward at the exclusive top, downward at the level below's top converted), `FAILSAFE_MARGIN` 32 past the cooldown, 1000 ms cooldown, 250 ms retry after a declined ascent, 15 s transition timeout, `FREE_ASCENT_MARGIN` 4, `transitionUp` placement on the target column, `freeSpaceAt`, velocity capped at 3.92 | `player_move` hook decides; the next ticks resolve the height (loaded column, or the generator's surface off-tick for an unloaded one), `teleport_to_level` keeping x, z, yaw and pitch, `Teleporting to linked world...`, then a settle check (raised out of blocks) and motion restore (`set_velocity`, flight when allowed); abandoned when the player leaves or changes level | `autoteleport.rs::*` (5 tests) | Implemented; Horizon: stub-checked |

## Commands, permissions and teleportation

| Upstream symbol | Behavior | Rust implementation | Horizon integration | Acceptance test | Status |
|---|---|---|---|---|---|
| TPM `TpllCommand` coordinate parsing | Decimal, DMS and suffix formats; height | `coordinates.rs`, `java_text.rs`, `tpll.rs` | `/tpll <coordinates:text>` and alias `/tpc` | `coordinate_oracle.rs::matches_upstream_on_every_recorded_input` (245 of 245) | Verified |
| TPM `TpllCommand` flow | Height lookup, ungenerated-chunk permission, linked worlds, messages | `tpll.rs`, `teleports.rs`, `messages.rs` | Off-tick surface loading; `entity.teleport_to_level` | `tpll_flow.rs::*` (4 tests), `messages.rs::teleport_lines_follow_upstream_number_formatting` | Implemented; Horizon: stub-checked |
| `/tpll -p <players>` (`t+-.forcetpll`) | Teleport other players from any source; team bounds skipped; ungenerated-chunk and other-world rights of each target; `Executed tpll for N player(s).` | `TpllTarget` in the adapter, `messages::forced_tpll_summary` | `tpll -p <targets:players> <coordinates:text>` (and `tpc`) at `permissions.forcetpll` | `messages.rs::actionbar_and_forced_tpll_text_match_upstream`, `tpll_flow.rs::enforces_authorization_and_bounds` (`sender_is_target: false`) | Implemented; Horizon: stub-checked |
| Bare `/tpll` | Usage message | Root usage `tpll` / `tpc` | `Command::op("tpll", tpll)` | n/a | Implemented; Horizon: stub-checked |
| TPM `WhereCommand` | Geographic position plus a Google Maps link | `messages.rs::where_message` | `/where`, click and hover style | `where_links_the_reported_coordinates`, `command_text_oracle.rs::matches_java_double_text` | Implemented; Horizon: stub-checked |
| TPM `OffsetCommand` | Offsets, or Y per linked level | `messages.rs::offset_lines` | `/offset` | `offsets_list_linked_levels_instead_of_y` | Implemented; Horizon: stub-checked |
| TPM `DistortionCommand` | Tissot-based scale and angle report | `messages.rs::distortion_message` | `/distortion` | `bte_tissot_matches_upstream`, `matches_java_double_text` (`%.6f`, `%.4f`) | Implemented; Horizon: stub-checked |
| TPM `Permission` nodes | `t+-.tpll`, `.tpll.otherWorld`, `.tpll.ungenerated-chunks`, `.admin`, `.where`, `.offset`, `.distortion` | `config.rs::PermissionLevels` maps nodes to vanilla operator levels | `Command::op(usage, level)` plus checks in the handlers | `enforces_authorization_and_bounds` | Implemented (operator levels replace permission plugins) |
| `t+-.notify.update`, `Updater` (PluginUpdater) | Update notices from GitHub releases | `updates.rs`: latest release (`tag_name`) through the system `curl` or `wget`, or `updates.url` (plain HTTP mirror or `file://`), on its own thread, at startup and then daily; numeric version comparison with pre-release suffixes; upstream's notice text and colours | Operators at `permissions.notify_update` (default 2) are told on join; the console logs it; `/terraplusminus status` shows the last check; `updates.enabled`, `updates.repository` (default the port's repository) | `updates.rs::*` (5 tests) | Implemented |
| `height_in_actionbar` (`PlayerMoveEvent.setHeightInActionBar`) | Bold `<blockY - offset>m` for every online player, on each move and every 20 ticks; not while holding a debug stick | `messages::actionbar_height`, `ServerConfig::actionbar_offset` | `player.action_bar`, polled at `ServerTickStart`: sent when the block Y changes and every 20 ticks | `actionbar_and_forced_tpll_text_match_upstream`, `config.rs::linked_levels_are_dimensions_of_this_mod` | Implemented; Horizon: stub-checked |
| Teleport readiness | Never teleport into ungenerated or unloaded terrain | `teleports.rs`: bounded pending table, timeout, replaceable | Polled at `ServerTickPhase::ServerTickStart`; refused or dropped while generation is not open | `pending_teleports_are_bounded_and_replaceable`, `the_exclusive_top_of_the_world_is_a_valid_feet_position` | Implemented; Horizon: stub-checked |
| Diagnostics | Queue, cache and fingerprint state | `TerraStack::diagnostics_report` | `/terraplusminus status` (diagnostics level) | Exercised by `terra-cli benchmark` | Implemented |
| `PlayerCommandPreprocessEvent` (`passthrough_tpll`) | Hand `/tpll` and `/tpc` to another plugin's namespace | `config.rs` (`passthrough_tpll`, empty = off) | `/tpll` and `/tpc` are not registered, leaving them to the other mod | `config.rs::upstream_switches_parse_with_upstream_defaults` | Implemented (Horizon has no command rewriting; not registering is the equivalent) |
| `PlayerJoinEvent`, `PluginMessageEvent`, `PlayerHashMapManagement` | Pending `/tpll` from the proxy, the `GetServer` request 20 ticks after a join | `bridge.rs::PendingArrivals` (bounded to 1024, 2 min lifetime), delivered coordinates must parse as `/tpll` coordinates (no `-p` or selector from a raw channel anyone can write to) | `player_logged_in`: `GetServer` after 20 ticks while the name is unknown, then the parked `/tpll` run as the player (`run_command` at their level, `performCommand`); `player_logged_out` clears per-player state | `bridge.rs::arrivals_are_bounded_and_expire` | Implemented; Horizon: stub-checked |

## Configuration keys (`config.yml`)

Every key of upstream's `config.yml` at the pinned revision, and where it went.

| Key | Port |
|---|---|
| `prefix` | `prefix`, used by every message |
| `reduced_console_messages` | Default `true` as in `config.yml`; `false` logs every data fetch (found, missing, failed, with size and time), like TMM `Http` |
| `height_datapack` | Selects the -64..2032 world range |
| `height_in_actionbar` | Action bar height, above |
| `passthrough_tpll` | `/tpll` and `/tpc` left unregistered |
| `min_latitude`, `max_latitude`, `min_longitude`, `max_longitude` | Team bounds |
| `terrain_offset.x/y/z` | Projection and height offsets |
| `linked_worlds.enabled`, `.method`, `.worlds[].name/offset` | Linked levels (MULTIVERSE) or linked servers (SERVER); port extension `linked_worlds.server_name` |
| `generate_trees` | Tree populator |
| `biomes.use_dataset`, `biomes.biome` | Köppen dataset or a fixed biome |
| `surface_material` | Default surface block |
| `building_outlines_material`, `road_material`, `path_material` | Deprecated upstream; mapped to surface replacements with upstream's warning |
| `config_version` | Accepted and ignored (the port has a single format) |
| TMM `maxConcurrentRequests` (per host list) | `network.max_concurrent_requests`, same entries and defaults; `network.max_per_host` for unlisted hosts |
| TMM `cacheTTL` | `network.cache_ttl_minutes`, default 1440, for tiles and 404 answers; stale tiles are served when a refresh fails |

Permission nodes become operator levels in `permissions`: `tpll`, `tpll_other_world`,
`tpll_ungenerated_chunks`, `forcetpll`, `admin`, `autoteleport`, `notify_update` and
`info_commands` (`where`, `offset`, `distortion`); `diagnostics` is the port's own. The port's
own sections `network` (including `https_command`, `user_agent`) and `updates` (`enabled`,
`repository`, `url`) replace what upstream hard-codes or takes from TerraMinusMinus.

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
