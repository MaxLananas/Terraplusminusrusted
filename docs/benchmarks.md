# Benchmarks

Raw data: [`docs/benchmarks/2026-11-21/`](benchmarks/2026-11-21/). Each CSV has columns
`phase,chunk_x,chunk_z,micros`, one row per request.

## Method

```sh
target/release/terraplusminus benchmark examples/offline-grand-canyon/terraplusminus.json5 out.csv 10 64
```

- **cold:** 441 chunk loads (a 21 by 21 square around 35.995 N, 113.247 W) on an empty RAM
  cache, one after another. Each load projects the chunk, reads and decodes the terrarium tile
  through the mirror, blends with CUBIC interpolation, and bakes the column data.
- **warm:** the same 441 chunks again, served from the loader's RAM cache.
- **burst:** 64 threads each request a distinct uncached chunk at once, the way a teleport burst
  would. `burst_total` is the wall time of the whole burst.

The host had 2 cores, about 3.9 GiB of RAM and no swap. The elevation source is the single
repository fixture tile, read from local disk, so the numbers leave out network latency
entirely. They measure the CPU path only, not provider behavior.

## Results

Three runs per build. "off" is `lto = false`; "thin" is the committed release profile
(`lto = "thin"`, 16 codegen units). Times are microseconds unless marked.

| Run | cold sum (ms) | cold median | cold p95 | warm median | burst median | burst p95 | burst total (ms) |
|---|---|---|---|---|---|---|---|
| off 1 | 53.7 | 119 | 134 | 0 | 3904 | 6225 | 10.2 |
| off 2 | 58.2 | 130 | 148 | 0 | 3944 | 5292 | 10.6 |
| off 3 | 50.9 | 105 | 141 | 0 | 4105 | 5940 | 11.0 |
| thin 1 | 49.2 | 103 | 138 | 0 | 1588 | 2659 | 9.0 |
| thin 2 | 39.7 | 85 | 103 | 0 | 1890 | 3244 | 8.6 |
| thin 3 | 39.0 | 88 | 105 | 0 | 2885 | 3779 | 8.6 |

The first cold request in every run (about 3.1 to 3.7 ms) includes reading and decoding the tile.
Binary size: 14,261,984 bytes with ThinLTO, 15,487,280 bytes without it.

ThinLTO is kept: across all three runs it lowers the cold median and the burst latency, and the
binary is smaller. Three runs on a shared 2-core host are too few for a precise effect size.

## Not measured

- Generation inside Horizon, chunk throughput under a real server tick, and teleport bursts by
  real players: Horizon cannot run here.
- Network-backed provider latency and the disk cache: provider hosts are unreachable from this
  environment.
