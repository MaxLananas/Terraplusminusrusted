# Terraplusminusrusted

A native Rust port of [TerraPlusMinus](https://github.com/BTE-Germany/TerraPlusMinus) (BTE
Germany) and the parts of [terraminusminus](https://github.com/SmylerMC/terraminusminus) it
depends on, for the [Horizon](https://github.com/horizon-rust/horizon) engine. It generates a
1:1 Earth from real elevation data in the BuildTheEarth projection, and provides `/tpll`,
`/where`, `/offset` and `/distortion`.

Production code is Rust only. Java appears solely in `tools/oracle`, a development-time harness
that runs the pinned upstream algorithms to produce reference fixtures.

## Status

What works and is tested here:

- Projections (BTE conformal Dymaxion, Dymaxion, Web Mercator, transforms) and Tissot's
  indicatrix, compared against the Java oracle.
- Elevation datasets, terrarium decoding and the three blend modes (`NEAR`, `LINEAR`,
  `CUBIC`; upstream has no other), matching upstream bit for bit
  on the recorded fixtures.
- GeoTIFF layers (classic TIFF and BigTIFF; none, LZW, Deflate and PackBits compression;
  predictors 2 and 3) and upstream's parser chain (`add`, `multiply`, `divide`, flips, axis swap)
  and projection transforms (`scale`, `offset`, flips, axis swap). Every raster upstream's
  `parse_tiff` reads decodes to the same doubles bit for bit, checked against Apache Commons
  Imaging (pinned in `scripts/upstream-revisions.env`) run by the oracle. Upstream's complete
  `heights.json5` (1979 layers) loads.
- Every integer parser of terraminusminus (`parse_png`, `parse_jpg`, `parse_tiff`, `add`, `and`,
  flips, axis swap, `grayscale_extract`, `rgb_extract`, `require_opaque`) and `from_int`. Images
  decode exactly as `ImageIO` and `BufferedImage.getRGB` do (sRGB curve on gray PNGs, JPEG
  colour conversion, ...): 103 fixture images are compared pixel for pixel with the JDK, and every
  parser chain with upstream's own classes.
- Tree cover (`TreeCoverBaker`) and TerraPlusMinus's `TreePopulator`: the 16 custom tree
  schematics, chosen by Köppen class, placed by Paper's populator random. See [Trees](#trees).
- Chunk terrain, Köppen biomes and the generation fingerprint.
- Provider acquisition with retries, rate limiting and a bounded disk cache.
- `/tpll` coordinate parsing and flow, and the chat text of all four commands.
- The OpenStreetMap pipeline: GeoJSON tiles, the `osm.json5` mapping language and the drawing of
  roads, buildings, water, coastlines and tree clearing. Its mapped elements and every drawn
  column match the Java oracle on four recorded scenes, one of which exercises a custom mapping.

What is not done:

- **The Horizon deliverable is incomplete and not production-ready.**
- **Horizon has never run this code.** Horizon needs Rust 1.95 and crates.io, which this
  environment cannot reach. The mod in `horizon-mod/` is type-checked against a hand-written
  stub of the API (`tools/horizon-api-stub`), whose README cites the SDK source of every
  signature. The checks a real server run would cover (joining, lighting, collision,
  save/restart) are still open.
- **Every upstream feature is ported**, with these differences:
  - HTTPS goes through the system `curl` (`network.https_command`), since no TLS library is
    available offline. Without curl, `https://` sources must be mirrored, served over plain HTTP
    (`network.plain_http_hosts`) or turned off, or the configuration is refused at startup (see
    [Network](#network));
  - TIFFs compressed with JPEG or CCITT fax, and CIELab or LogLuv TIFFs, are refused, where
    upstream decodes them. No upstream configuration uses them;
  - `parse_tiff` also reads TIFFs that Commons Imaging refuses (unsigned or 8-bit samples, a
    missing `SampleFormat`). It also reads big-endian BigTIFFs with a single strip correctly,
    where Commons Imaging misreads the strip offset.

The per-feature status is in [docs/feature-matrix.md](docs/feature-matrix.md).

## Security and robustness

Everything that comes from outside the server is treated as hostile: tile servers, mirrors, the
release feed, proxy messages, player commands and the configuration file. None of it can crash
the server, make it allocate without bound, block it forever or run a command. The rules:

- **Decoders** (PNG, JPEG, TIFF/BigTIFF, Deflate/gzip/zlib, relaxed JSON, GeoJSON) bound every
  allocation before making it. PNG and JPEG images are at most 4096 × 4096 pixels. TIFFs are
  capped in pixels, strips, field sizes and decoded bytes (`TiffLimits`). Tiles are at most
  16 MiB (`MAX_TILE_BYTES`). Inflated output and JSON nesting are capped. Any arithmetic on decoded samples
  wraps the way Java's `int` does instead of overflowing.
- **Network.** The plain HTTP client accepts only `http://host[:port]/path` with a strictly
  checked host, port and path, so no URL can inject a header or split the request line. Every
  request has a total deadline, which a slow ("dripping") server cannot extend, and caps on
  header and body size. `https://` goes through `curl` with `-q` (no `.curlrc`), no shell,
  `--proto =https --proto-redir =https`, at most 5 redirects, `--max-time` and `--max-filesize`.
  curl itself runs under a watchdog that kills it on timeout or when its output passes the cap
  (`run_bounded`). The `User-Agent` must be printable ASCII.
- **Disk cache.** Entries are size-capped on read and on write. Symlinks and anything that is not
  a regular file are deleted as corrupt, and partial files are created exclusively.
- **OpenStreetMap.** Reference keys are whitelisted (no `..`, no absolute paths), reference
  chains are cut at 16 levels and on cycles. A resource holds at most
  `MAX_ELEMENTS_PER_RESOURCE` elements, its references included, so a tile that references the
  same file thousands of times cannot exhaust memory.
- **Proxy bridge (linked worlds).** A teleport delivered by the proxy runs only when it arrives
  through the target player's own connection. For a player not online yet it is parked, at most
  16 per carrying connection. Coordinates are at most 256 characters with no control characters
  and must parse. A server-name answer counts only from a connection that was asked, only the
  first one, at most 128 characters; a configured `linked_worlds.server_name` always wins.
- **`/tpll`.** A non-finite height is a usage error. A point that projects to NaN or farther than
  30,000,000 blocks is "outside the projection". Upstream's acceptance of a presumed swapped
  pair (latitude up to 180) is kept, and the projection then rejects it.
- **Auto-teleport.** Non-finite positions, rotations and velocities never start or carry over a
  transition. Speed is capped.
- **Update check.** URLs with whitespace or control characters are refused. `file://` reads are
  size-capped, and the tag shown in chat must be at most 64 characters of `[A-Za-z0-9._+-]`.
- **Code.** `unsafe` is forbidden, and `unwrap`, `expect` and `panic!` are denied outside tests,
  in the mod as well as the crates.

The tests that hold these rules:

- `crates/terra-codec/tests/fuzz_decoders.rs` mutates valid seeds through every decoder.
  `TERRA_FUZZ_SCALE=<n>` multiplies the case counts (the default runs in seconds; `400` runs
  millions of cases). `TERRA_FUZZ_DUMP=<dir>` writes each crashing input there.
- Every input that ever crashed a decoder is kept in `crates/terra-codec/tests/regressions/`
  (listed in its `README.md`) and replayed through every decoder by `tests/regressions.rs`.
- `crates/terra-server/tests/hostile_inputs.rs` covers bridge payloads, `/tpll` arguments,
  release documents and configuration text.
- `crates/terra-elevation/tests/hostile_inputs.rs` covers dataset configurations and tiles through
  every parser chain.
- `crates/terra-osm/tests/fuzz_pipeline.rs` covers the GeoJSON and mapping pipeline.
- The HTTP and curl response parsers are tested in-module.
- `crates/terra-acquisition/tests/loopback_http.rs` runs against misbehaving local servers.

## Repository layout

| Path | Contents |
|---|---|
| `crates/terra-codec` | Bounded DEFLATE, gzip, zlib, PNG, JPEG, TIFF and relaxed-JSON decoding |
| `crates/terra-projection` | Geographic projections and their Java-compatible numerics |
| `crates/terra-elevation` | Elevation dataset configuration, tiles, blending, per-chunk sampling |
| `crates/terra-acquisition` | HTTP fetching, retry and rate-limit policy, mirrors, disk cache |
| `crates/terra-osm` | OpenStreetMap GeoJSON tiles, `osm.json5` mapper, geometry drawing |
| `crates/terra-chunk-generation` | Chunk terrain, block columns, biomes, chunk loader, fingerprint |
| `crates/terra-server` | Configuration, `/tpll`, command messages, teleports, the engine bridge |
| `crates/terra-cli` | `terraplusminus`, an operator tool: config checks, column queries, benchmarks |
| `horizon-mod/` | The Horizon native mod (outside the workspace; staged before building) |
| `tools/oracle/` | Java reference harness, patches and stubs (development only) |
| `tools/horizon-api-stub/` | Signature stub of the Horizon mod API (development only) |
| `fixtures/` | Oracle goldens and the elevation fixture, each with a `PROVENANCE` |
| `examples/` | Example configurations |
| `scripts/` | Verification, release, fixture and mod staging scripts |
| `docs/` | Feature matrix, upstream pins, benchmarks |

The workspace crates have no third-party dependencies, because crates.io was unreachable while
this was built.

## Prerequisites

- The Rust toolchain pinned in `rust-toolchain.toml` (nightly-2025-09-25, rustc 1.92), with
  clippy. Where rustup is unreachable, `scripts/install-npm-toolchain.sh` installs that exact
  build from an npm mirror after checking each archive's pinned sha512.
- `python3`, `git`, `gzip` and `sha256sum` for the scripts.
- For the oracle only: network access to PyPI, npm and GitHub. `tools/oracle/build-oracle.sh`
  fetches a pinned JDK, ECJ, Guava and Gson into `tools/oracle/build/`.
- For the Horizon mod: the Horizon launcher and `cargo horizon-mod`, which bring their own pinned
  toolchain (Rust 1.95) and SDK.

## Verify

```sh
scripts/fetch-upstream.sh       # pinned upstream checkouts into .upstream/ (needed for --oracle)
scripts/verify.sh               # format (if rustfmt exists), lockfile, clippy, tests, fixture manifest
                                # and its negative tests, licenses, staging safety, mod type-check
scripts/verify.sh --oracle      # also rebuild the goldens with the Java oracle and require no change
scripts/verify.sh --live        # also run the `live_*` tests against real providers; fails unless
                                # every one of them runs and passes
```

Each step prints PASS, FAIL or NOT RUN, and the script exits non-zero on any failure. Logs,
commands, toolchain versions and upstream revisions go to `artifacts/verify/<timestamp>/`, which
git ignores. Set `CARGO_COMMAND` to run cargo through a wrapper.

`tools/bench/run.sh [warm-up s] [measured s]` times the same tile decodes and projections in
upstream's Java classes (from the oracle build) and in this port. Measured on 2 cores (JDK 25 vs
rustc 1.92 release), median per call:

| Workload | Java | Rust |
|---|---|---|
| Terrarium PNG 256² (`parse_png_terrarium`) | 5.0 ms | 3.2 ms |
| JPEG 256² (`parse_jpg`) | 2.8 ms | 2.3 ms |
| GeoTIFF float32 Deflate 1024² (`parse_tiff`) | 35.7 ms | 45.2 ms |
| GeoTIFF int16 Deflate + predictor 512² | 3.5 ms | 2.7 ms |
| BTE `fromGeo` ×100k | 45.0 ms | 43.4 ms |
| BTE `toGeo` ×100k | 33.8 ms | 31.0 ms |

The port's Deflate decoder is pure Rust and about 1.8× slower than the native zlib Java calls,
which is why the large Deflate TIFF is slower.

## Release build

```sh
scripts/release-build.sh
```

This builds the host release binary (ThinLTO, without target-cpu=native, panic=abort, static
linking or PGO), runs a smoke test and writes the binary and `SHA256SUMS` to
`artifacts/release/<timestamp>/`. Only x86_64 Linux with glibc has been built and tested.

## Operator tool

```sh
terraplusminus check-config examples/offline-grand-canyon/terraplusminus.json5
terraplusminus column examples/offline-grand-canyon/terraplusminus.json5 35.995 -113.247
terraplusminus tpll examples/offline-grand-canyon/terraplusminus.json5 35°59\'42\"N 113°14\'49\"W
terraplusminus benchmark examples/offline-grand-canyon/terraplusminus.json5 out.csv 10 64
```

The offline example reads the repository's single terrarium tile through a mirror, so it works
without network access. Tiles outside that one are reported missing, and missing tiles generate
shallow ocean, as upstream does.

## Building and running the Horizon mod

The mod sources live in `horizon-mod/` and depend on the workspace crates by path. Horizon refuses
path dependencies that leave the mod folder, so stage a self-contained copy first. Building
fetches the SDK pinned in `horizon-mod.toml` into `.horizon/sdk/`.

```sh
python3 scripts/stage-horizon-mod.py artifacts/horizon-mod/terraplusminus
cd artifacts/horizon-mod/terraplusminus
horizon-launcher native-build . --instance INSTANCE --trust-native-code
```

To run a dedicated server, export it instead:

```sh
horizon-launcher native-build . --server-output DIR --version TAG --trust-native-code
horizon-server --dir SERVER config set engine.path=/absolute/DIR/horizon-engine
horizon-server --dir SERVER restart
```

These steps follow Horizon's modding documentation at the pinned commit. They have not been run
here.

At startup the mod reads `<instance>/config/terraplusminus/terraplusminus.json5`. Without it, the
upstream defaults apply and a log line says so. An invalid file stops the dimension from
registering, and the error is logged. The mod registers:

- the dimension `terraplusminus:earth`, with the `earth` chunk generator and biome source;
- `/tpll <coordinates>` and its alias `/tpc`, `/where`, `/offset`, `/distortion`, and
  `/terraplusminus status` for diagnostics.

Permissions map upstream's permission nodes to operator levels, under the configuration's
`permissions` section.

Elevation data is fetched on Horizon's worldgen worker threads; the server tick only polls
finished work. The generation fingerprint is checked when the earth level opens, before any of
its chunks. Transient fetch failures (network errors, HTTP 5xx, 429) are retried with backoff up to
`generation.retry_attempts` times (default 8; 0 retries until the server stops). A chunk fails,
and is neither published nor saved, when its data is permanently unavailable (unreachable URL,
malformed tile), when the retries run out, when the server stops, when the world's fingerprint
does not match, or when a block state does not exist in the game. Generation never substitutes
flat terrain, an empty chunk or another block.

The failure works around a gap in Horizon's API: `ChunkGenerator::build_terrain` cannot report
failure, so the mod panics. Horizon's worldgen job runner catches the panic
(`horizon-threads/src/jobs.rs`), and the chunk stays pending in that server session without
being published or saved. It is generated again after a restart. This path has only been read in
the engine source, not run.

## Configuration

The format is upstream's `config.yml` expressed as JSON5, plus sections for this port: `world`,
`elevation` (`heights_config`, `ign_lidar_hd`, `tree_cover_config`, `unsupported_layers`), `network`,
`permissions`, `generation`, `prefix`, `openstreetmap` and `updates`. Unknown keys
are errors. See `examples/` and `crates/terra-server/src/config.rs`.

`elevation.unsupported_layers` (`reject` or `skip`) is still accepted, but every layer type of
upstream loads now, so nothing is ever skipped. An unknown type name is always an error.

### Elevation data and France (IGN LiDAR HD)

Without `elevation.heights_config`, the heights come from upstream's complete default
`heights.json5`, as in TerraPlusMinus. That is the AWS Terrain Tiles world layers plus about
1,960 regional DEMs on `cloud.daporkchop.net`. For France the regional layer is IGN RGE ALTI at
1 m (a June 2021 copy). The two AWS layers alone (about 13 m per pixel at French latitudes) are in
`examples/aws-terrain-tiles/heights.json5`.

On top of that, `elevation.ign_lidar_hd` (on by default) adds IGN's **LiDAR HD** terrain model
from the Géoplateforme WMS-Raster service (`data.geopf.fr`, no key). The layers are defined in
`crates/terra-elevation/resources/heights_ign_lidar_hd.json5`:

- They cover mainland France, Corsica, Guadeloupe, Martinique, La Réunion and Mayotte.
- The model has a 0.5 m grid and about 30 cm vertical accuracy.
- It is requested as raw float32 GeoTIFF on the exact pixels of web Mercator zoom 17 (about
  0.8 m), with `LINEAR` blending.
- The data is © IGN, under the Licence Ouverte Etalab 2.0, and must be credited as "IGN".

| Point (IGN altimetry service, November 2026) | LiDAR HD | RGE ALTI |
|---|---|---|
| Mont Blanc summit (surveyed 4805.59 m in 2021) | 4805.43 m | 4755.64 m |
| Paris, Hôtel de Ville | 35.19 m | 34.90 m |
| Pyrenees (0.0743, 42.9091) | 1709.50 m | 1707.99 m |

RGE ALTI is exact where it already came from LiDAR, but in the mountains and under forests it
falls back on radar and older 25 m data: it reads 2 to 8 m off, and on the Mont Blanc summit
50 m. LiDAR HD also shows embankments, ditches and riverbanks that 1 m interpolated data
smooths away.

How it degrades:

- **Not surveyed yet.** Where LiDAR HD is not published (French Guiana is not in the programme;
  Guadeloupe had nothing published in November 2026), the service answers -9999 or -99999. The
  `filter` parser turns these values into `NaN` before blending. Those columns, and only those,
  come from RGE ALTI, then from AWS.
- **Service failure.** If the service fails (unreachable, an error page instead of a TIFF), the
  layers are `"optional": true`: they are left out for 60 seconds and the chunk is generated from
  RGE ALTI instead of failing. Chunks made during an outage therefore differ from their
  neighbours. Set `ign_lidar_hd` to `false` if every chunk must come from one source.
- **No route.** Without curl or another route to `data.geopf.fr` (for example an offline setup with
  mirrors), the layers are left out at startup with a message.
- **Load on the service.** The service allows 40 requests per second per IP. The default
  `max_concurrent_requests` keeps 6 requests in flight to `data.geopf.fr`, and 429 answers wait for
  their `Retry-After`. Use `network.cache_directory` so that each tile is fetched once.
- **Fingerprint.** The heights config and the IGN file are part of the generation fingerprint.

**Existing worlds.** Before this change the built-in default was the two AWS layers alone. A world
created with that default is refused after upgrading, because its heights changed. To keep
extending such a world, copy `examples/aws-terrain-tiles/heights.json5` next to the config. It is
byte-identical to the old default, so the fingerprint matches. Then set
`"elevation": {"heights_config": "heights.json5", "ign_lidar_hd": false}`.

The extensions behind this are general. They are not upstream syntax, so such configs do not
load in TerraPlusMinus:

- `${bbox_3857}` in a URL template is the tile's EPSG:3857 extent, as a WMS `BBOX`. It is
  allowed only for a plain `web_mercator` projection. For `LINEAR` and `CUBIC` it is shifted half
  a pixel, so that the value at each pixel centre lands exactly where interpolation reads it.
- `{"filter": {"condition": ..., "delegate": ...}}` turns values that fail the condition into
  `NaN`.
- `"optional": true` on a layer enables the fallback described above.

### Network

```json5
"network": {
    // Program that fetches https:// URLs, with hard limits on time and size. "off" disables
    // HTTPS: only mirrors, plain http:// and plain_http_hosts remain.
    "https_command": "curl",
    // Hosts whose https:// URLs are fetched over plain HTTP instead.
    "plain_http_hosts": [],
    // URL prefix -> local directory, served without any request.
    "mirrors": {},
    // Requests in flight per host ("<n>: <url>"). Default: upstream's list (8 for
    // cloud.daporkchop.net and s3.amazonaws.com, 1 for gis-treecover.wri.org and Overpass),
    // plus 6 for data.geopf.fr (IGN LiDAR HD).
    "max_concurrent_requests": ["8: https://cloud.daporkchop.net/", "8: https://s3.amazonaws.com/"],
    // Sent on every request. Default: "Terraplusminus/<version> (Terra--/rust; +<repository>)".
    "user_agent": "...",
    // Cached responses expire after this many minutes (upstream's http.cacheTTL); 0 keeps them.
    "cache_ttl_minutes": 1440,
}
```

An expired tile is fetched again and served stale if that fetch fails.

### OpenStreetMap

OSM features are on by default, as upstream. They draw roads (gray concrete), paths (dirt path),
building outlines (bricks), rivers, lakes and coastlines, and they clear trees under buildings and
water. The section accepts:

```json5
"openstreetmap": {
    "enabled": true,
    // URL prefixes tried in order; each must end in `/`. Default: upstream's server.
    "servers": ["https://cloud.daporkchop.net/gis/osm/0/"],
    // Mapping file, relative to the config directory. Default: osm.json5, extracted next to
    // the config on first start (byte-identical to upstream's) so it can be edited.
    "mapping": "osm.json5",
    "memory_cache_tiles": 512,
}
```

The default server is HTTPS, fetched through `network.https_command`. Without it, serve a copy
of the tiles over plain HTTP and list its host in `network.plain_http_hosts`, map the server
prefix to a local directory with `network.mirrors`, or set `enabled` to `false`; otherwise the
server refuses the configuration at startup. A chunk whose OSM tile cannot be fetched is retried like
elevation data. An invalid mapping file stops the server
from starting rather than generating without features.

Upstream's deprecated `building_outlines_material`, `road_material` and `path_material` keys
still work as they do upstream: each replaces the block that `osm.json5` draws for building
outlines (`minecraft:bricks`), roads (`minecraft:gray_concrete`) or paths (`minecraft:dirt_path`),
and startup logs upstream's deprecation warning. Editing `osm.json5` is the better way.

### Trees

`generate_trees` is on by default, as upstream. Tree cover comes from upstream's
`tree_cover.json5` (Hansen tree cover 2000 tiles at
`https://cloud.daporkchop.net/gis/treecover2000/`), or from the file named by
`elevation.tree_cover_config`, which uses the same schema. That server is HTTPS-only: it is
fetched through `network.https_command`, and without it you can copy `tree_cover.json5` from
terraminusminus, point its URL at a plain-HTTP or mirrored copy and set `tree_cover_config`, or
set `generate_trees` to `false`; otherwise the server refuses the configuration at startup. With trees off, tree cover is never
fetched (upstream fetched it anyway, TerraPlusMinus issue #77).

How it matches upstream:

- The tree cover is baked as `TreeCoverBaker` bakes it: `ceil(cover / 4 * 255 * 1.5)`, 0 where data
  is missing, cleared under OSM buildings and water and on null island.
- Tree positions follow `TreePopulator` and Paper: the decoration seed of the world seed and chunk
  position, 64 tries compared against the first column's density, `nextBytes` and `nextInt(15)`.
  The first try that lands below water ends the chunk. Tries outside
  `min_y < ground + y_offset < max_y - 35` or on an OSM surface block are skipped.
- The tree families by Köppen class (savanna 4, 6 and 17; spruce 27; none for 28 to 30; oak and
  birch otherwise, including when the biome dataset is off) and the 16 schematics of
  `customTrees.json` (byte-identical) are upstream's.

Where it differs:

- Horizon has no populator stage, so each chunk replays its neighbours' populators and keeps the
  blocks that fall inside it (a tree reaches at most 5 blocks from its trunk, so only the 8
  neighbours matter). The result is the same as upstream placing whole trees, without the
  generation-order effects.
- Upstream picks the schematic and its rotation with an unseeded `new Random()`, and its
  per-thread density buffer carries over between chunks. Both make upstream's trees differ on
  every regeneration. Here the schematic and rotation are seeded by the world seed and chunk, and
  the buffer starts at zero for every chunk, so regeneration is repeatable.
- The climate class is read at the tree's own column. Upstream reads whichever biome its biome
  provider computed last, on any thread.
- Leaves are placed persistent (`persistent=true`). The schematics use fences as trunks, so
  upstream's default leaves decay unless the server disables leaf decay.

Trees change the generation fingerprint. A world created before trees existed in this port is
refused while trees are on; set `generate_trees` to `false` to keep extending it.

## Attribution and license

This project is MIT-licensed ([LICENSE](LICENSE)). It ports code and bundles data from:

- TerraPlusMinus, MIT, Copyright (c) 2022-2026 BTE Germany;
- terraminusminus, MIT, Copyright (c) 2020 BuildTheEarth.net;
- a MapLibre test tile (BSD-3-Clause; terrain data attribution per tilezen/joerd).

Bundled data files carry their own `NOTICE` files. Horizon is LGPL-3.0; the mod uses its public
mod API and links nothing else. Revisions and details are in
[docs/upstream-pins.md](docs/upstream-pins.md).
