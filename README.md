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
- Elevation datasets, terrarium decoding and all four blend modes, matching upstream bit for bit
  on the recorded fixtures.
- Chunk terrain, Köppen biomes and the generation fingerprint.
- Provider acquisition with retries, rate limiting and a bounded disk cache.
- `/tpll` coordinate parsing and flow, and the chat text of all four commands.

What is not done:

- **Horizon has never run this code.** Horizon needs Rust 1.95 and crates.io, which this
  environment cannot reach. The mod in `horizon-mod/` is type-checked against a hand-written
  stub of the API (`tools/horizon-api-stub`), whose README cites the SDK source of every
  signature. The checks a real server run would cover (joining, lighting, collision,
  save/restart) are still open.
- **Not ported:**
  - the OpenStreetMap pipeline, so there are no OSM water polygons, coastlines, roads or
    buildings;
  - trees and tree cover;
  - GeoTIFF elevation layers;
  - HTTPS (use a plain-HTTP mirror);
  - the SERVER linked-worlds method;
  - auto-teleport on movement;
  - `/tpll -p`.

The per-feature status is in [docs/feature-matrix.md](docs/feature-matrix.md).

## Repository layout

| Path | Contents |
|---|---|
| `crates/terra-codec` | Bounded DEFLATE, gzip, zlib, PNG and relaxed-JSON decoding |
| `crates/terra-projection` | Geographic projections and their Java-compatible numerics |
| `crates/terra-elevation` | Elevation dataset configuration, tiles, blending, per-chunk sampling |
| `crates/terra-acquisition` | HTTP fetching, retry and rate-limit policy, mirrors, disk cache |
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
  fetches a pinned JDK, ECJ and Guava into `tools/oracle/build/`.
- For the Horizon mod: the Horizon launcher and `cargo horizon-mod`, which bring their own pinned
  toolchain (Rust 1.95) and SDK.

## Verify

```sh
scripts/fetch-upstream.sh       # pinned upstream checkouts into .upstream/ (needed for --oracle)
scripts/verify.sh               # lockfile, clippy, tests, fixture digests, licenses, mod type-check
scripts/verify.sh --oracle      # also rebuild the goldens with the Java oracle and require no change
scripts/verify.sh --live        # also run tests against real elevation providers
```

Each step prints PASS, FAIL or NOT RUN, and the script exits non-zero on any failure. Logs,
commands, toolchain versions and upstream revisions go to `artifacts/verify/<timestamp>/`, which
git ignores. Set `CARGO_COMMAND` to run cargo through a wrapper.

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
finished work. When data cannot be fetched, generation waits and retries rather than producing
flat terrain. A world whose stored generation fingerprint does not match the configuration stays
closed to generation, and the server log says why.

## Configuration

The format is upstream's `config.yml` expressed as JSON5, plus sections for this port: `world`,
`elevation`, `network`, `permissions`, `generation` and `prefix`. Unknown keys are errors. See
`examples/` and `crates/terra-server/src/config.rs`.

## Attribution and license

This project is MIT-licensed ([LICENSE](LICENSE)). It ports code and bundles data from:

- TerraPlusMinus, MIT, Copyright (c) 2022-2026 BTE Germany;
- terraminusminus, MIT, Copyright (c) 2020 BuildTheEarth.net;
- a MapLibre test tile (BSD-3-Clause; terrain data attribution per tilezen/joerd).

Bundled data files carry their own `NOTICE` files. Horizon is LGPL-3.0; the mod uses its public
mod API and links nothing else. Revisions and details are in
[docs/upstream-pins.md](docs/upstream-pins.md).
