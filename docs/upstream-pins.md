# Upstream pins

All revisions are recorded in [`scripts/upstream-revisions.env`](../scripts/upstream-revisions.env)
and fetched by `scripts/fetch-upstream.sh` into `.upstream/` (ignored by git). Nothing upstream is
linked or executed in production. The Java sources serve only as the reference for the oracle
under `tools/oracle`.

| Project | Revision | License | Used for |
|---|---|---|---|
| [TerraPlusMinus](https://github.com/BTE-Germany/TerraPlusMinus) | `5af87efafb0930c454da1ee49f098531018acca9` | MIT, (c) 2022-2026 BTE Germany | Generator, commands, configuration, `koppen_map.gz` |
| [terraminusminus](https://github.com/SmylerMC/terraminusminus) | `a1432a9beabd79025e62c8de0d0b1a99e7ea9943` | MIT, (c) 2020 BuildTheEarth.net | Projections, `conformal.gz`, elevation datasets, `heights.json5` |
| [Horizon](https://github.com/horizon-rust/horizon) | `ffd59c4e0cebd16c0982016f040b3a9b70e8b83b` | LGPL-3.0 | Native mod API (`horizon-mod-vocab`), manifest format |
| horizon-mixins | `e59da9094f0c2fdda6c627fe9eff22dec6a906f0` | (Horizon project) | Recorded in the mod manifest's `[native]` table |
| [Paper](https://github.com/PaperMC/Paper) | `d81ca50aeb956bccd912c124638f556b564ea8af` | GPL-3.0 (read only) | Behavior reference for the surface RNG seed; no code copied |
| [PorkLib](https://github.com/PorkStudios/PorkLib) | `836334a9655fe9c5fae088db74a7830af108a2bd` | (read only) | Semantics of `floorI`, `ceilI`, `lerp` and `clamp` |

## Bundled data

| File | Source | License notice |
|---|---|---|
| `crates/terra-projection/resources/conformal.gz` | terraminusminus | `crates/terra-projection/resources/NOTICE` |
| `crates/terra-elevation/resources/heights_world.json5` | terraminusminus `heights.json5` (first two layers) | `crates/terra-elevation/resources/NOTICE` |
| `crates/terra-chunk-generation/resources/koppen_map.gz` | TerraPlusMinus (byte-identical) | `crates/terra-chunk-generation/resources/NOTICE` |
| `fixtures/elevation/terrarium/12/759/1608.png` | maplibre-gl-js `763245d93043f479f6bf3f653b33d6368b68a886`, sha256 `0028d855e2def76dc8b5c7f484b4842f6f22185d8040718252a1142ce22bcceb` | BSD-3-Clause (MapLibre contributors, Mapbox); terrain data attribution per tilezen/joerd `docs/attribution.md` |

## Oracle toolchain (development only)

| Component | Version | Integrity |
|---|---|---|
| OpenJDK (PyPI `jdk4py` wheel) | 25.0.5.1 | sha256 `b989b35d24f704cb40b402a82003d131056bf2b9794c33acb84bf42b5ebffd34` |
| ECJ (from npm `@vscjava/java-language-server` 0.1.2) | 3.45.0 | npm integrity `sha512-5PrYFqYmqFbPuhXqFIiyIQWvyFisZ7KUZV0B4F0DaDvlxDSrwP2nTbNE2pmsjSApT5S3BX0VrMkeKKzX5SMAfA==` |
| Guava | 33.5.0 | jar sha256 recorded in each fixture `PROVENANCE` |

Each `fixtures/**/PROVENANCE` records the upstream commit, the JDK and the jar digests, and
digests of the harness, stubs and patches that produced the goldens.
