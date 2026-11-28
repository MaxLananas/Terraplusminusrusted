# Performance: upstream (Java) vs this port (Rust)

Measured on 2026-11-22. Host: Linux 6.1.158+ x86_64, 2 cores, 3939 MiB.
Java: OpenJDK 25.0.5.1 (Temurin), JIT warmed. Rust: 1.92.0-nightly (2025-09-24), release build, default target features (SSE2).

The numbers are medians over 15 rounds after 5 warm-up rounds, from two separate runs. The single-thread figures come from the same machine and the same fixture tile (`fixtures/elevation/terrarium/12/759/1608.png`).

## Measured

| Benchmark | Upstream Java | Port, before tuning | Port, now |
|---|---|---|---|
| Terrarium PNG decode, µs/tile | 5614–6067 | 3148 | 3218–3253 |
| BTE `fromGeo`, ns/call | 458–478 | 465 | 439–445 |
| BTE `toGeo`, ns/call | 340–350 | 315 | 320 |
| 16×16 chunk heights, cubic blend, µs/chunk | 15.2–20.6 | 60.4 | 18.5–19.1 |
| Köppen raster load at startup, s | 3.67–4.67 | 5.80 | 0.92 |
| Peak RSS of the micro-benchmark process, MiB | 200–220 | — | 36 |

Full Rust stack (`terraplusminus benchmark examples/offline-grand-canyon/terraplusminus.json5 out.csv 6 16`):
- startup to ready: 1.0 s, down from 5.8 s;
- cold chunk: median 62 µs, p95 ~100 µs, max ~4 ms (first tile decode);
- warm chunk: ≤ 1 µs;
- burst of 16 chunks: 2.4–2.6 ms;
- peak RSS: 38.5 MiB.

There is no Java equivalent for the full stack, because it requires a running Paper server.

## Tuning applied (output unchanged)

- **`TiledScalarDataset::sample`.** A HashMap lookup per blend tap made up about 3/4 of the time. It is replaced by a dense tile grid over the plan plus a last-tile memo. Elevation stays bit-exact against the upstream oracle (625 chunks).
- **CRC-32.** Slicing-by-16 instead of byte-wise (2.6 → 0.5 ns/byte).
- **DEFLATE overlapping matches.** Copied in doubling blocks instead of byte by byte.
- **Köppen run-length build.** Skips run bodies 8 bytes at a time. The result is identical to the byte-wise builder over the full 933,120,000-sample raster (4,945,075 runs).

## Reproduce

```sh
cargo build --release -p terra-elevation --example microbench -p terra-chunk-generation --example koppen_startup -p terra-cli
target/release/examples/microbench 5 15
target/release/examples/koppen_startup
# Java (oracle toolchain, see tools/oracle):
java -jar ecj.jar -21 -proc:none -classpath "tools/oracle/build/classes:<guava,failureaccess,gson>" -d /tmp/benchclasses tools/bench/java/terraplusminus/bench/*.java
java -Djava.awt.headless=true -cp "/tmp/benchclasses:tools/oracle/build/classes:<jars>" terraplusminus.bench.ElevationBench fixtures/elevation/terrarium/12/759/1608.png 759 1608 5 15
java -cp /tmp/benchclasses terraplusminus.bench.KoppenLoadBench crates/terra-chunk-generation/resources/koppen_map.gz 4
```

## Not measured

- Behaviour inside a real server: Paper's threading against Horizon's job system, tick impact, lighting and saving.
- Network latency and HTTPS providers.
- GC pauses under a real player load.

Horizon itself has never run here (see review finding F04).
