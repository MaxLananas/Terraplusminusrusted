//! Single-threaded micro-benchmarks mirroring `tools/bench/java/.../ElevationBench.java`
//! workload for workload, so the port can be compared with upstream on the same machine:
//!   decode     - Terrarium PNG decode of the fixture tile
//!   from / to  - BTE projection on 1,000,000 fixed random points
//!   chunk      - heights of 625 chunks (radius 12) from the decoded tile: corner bounds, plan,
//!                cubic sampling (the work ElevationGoldenDump's glue does)
//!   chunk_full - the same chunks through `sample_chunk_heights` with the in-memory tile cache
//!                (layers, cache lookups: the port's real per-chunk path)
//! Usage: cargo run --release -p terra-elevation --example microbench -- <warmup> <rounds>
//! Output: one "name<TAB>unit<TAB>median<TAB>min<TAB>max" line per benchmark.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::hint::black_box;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use terra_elevation::terrarium::decode_terrarium_png;
use terra_elevation::{
    AxisBounds, BlendMode, CachingTileProvider, LayeredScalarDataset, LoadedTiles, MirrorFetcher, ScalarLayer, TileFormat, TilePosition, TiledScalarDataset, chunk_geo_bounds,
    sample_chunk_heights,
};
use terra_projection::{GeoCoordinate, ProjectedCoordinate, SharedProjection, WebMercatorProjection, bte_world_projection};

const TILE: (i32, i32) = (759, 1608);

fn measure(warmup: usize, rounds: usize, mut round: impl FnMut() -> f64) -> Vec<f64> {
    for _ in 0..warmup {
        round();
    }
    let mut results: Vec<f64> = (0..rounds).map(|_| round()).collect();
    results.sort_by(f64::total_cmp);
    results
}

fn report(name: &str, unit: &str, sorted: &[f64]) {
    println!("{name}\t{unit}\t{:.3}\t{:.3}\t{:.3}", sorted[sorted.len() / 2], sorted[0], sorted[sorted.len() - 1]);
}

/// The fixed points of the Java benchmark are not reproduced bit for bit (different RNG); the
/// distribution (uniform over [-180, 180] x [-85, 85]) is the same.
fn points(count: usize) -> Vec<(f64, f64)> {
    let mut state = 0x5EEDu64;
    let mut next = move || {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
    };
    (0..count).map(|_| (next() * 360.0 - 180.0, next() * 170.0 - 85.0)).collect()
}

fn main() {
    let arguments: Vec<usize> = std::env::args().skip(1).map(|a| a.parse().unwrap()).collect();
    let (warmup, rounds) = (arguments.first().copied().unwrap_or(3), arguments.get(1).copied().unwrap_or(9));
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/elevation/terrarium");
    let png = std::fs::read(root.join(format!("12/{}/{}.png", TILE.0, TILE.1))).unwrap();

    report(
        "decode",
        "us/tile",
        &measure(warmup, rounds, || {
            let started = Instant::now();
            for _ in 0..20 {
                black_box(decode_terrarium_png(black_box(&png), 256).unwrap()[1234]);
            }
            started.elapsed().as_secs_f64() * 1e6 / 20.0
        }),
    );

    let bte = bte_world_projection().unwrap();
    let geo = points(1_000_000);
    let mut projected = vec![ProjectedCoordinate::new(0.0, 0.0); geo.len()];
    report(
        "from",
        "ns/call",
        &measure(warmup, rounds, || {
            let started = Instant::now();
            for (out, &(lon, lat)) in projected.iter_mut().zip(&geo) {
                *out = bte.from_geo(GeoCoordinate::new(lon, lat)).unwrap_or(ProjectedCoordinate::new(f64::NAN, f64::NAN));
            }
            started.elapsed().as_secs_f64() * 1e9 / geo.len() as f64
        }),
    );
    report(
        "to",
        "ns/call",
        &measure(warmup, rounds, || {
            let started = Instant::now();
            let mut acc = 0.0;
            for point in &projected {
                acc += bte.to_geo(*point).map_or(0.0, |g| g.longitude);
            }
            black_box(acc);
            started.elapsed().as_secs_f64() * 1e9 / projected.len() as f64
        }),
    );

    let mercator: SharedProjection = Arc::new(WebMercatorProjection::new(12).unwrap());
    let prefix = "mirror://terrarium/";
    let dataset = TiledScalarDataset::new("bench".into(), vec![format!("{prefix}12/${{x}}/${{z}}.png")], Arc::clone(&mercator), 256, BlendMode::Cubic, TileFormat::TerrariumPng).unwrap();
    let mut tiles = LoadedTiles::new();
    tiles.insert(TilePosition { x: TILE.0, z: TILE.1 }, Some(Arc::from(decode_terrarium_png(&png, 256).unwrap())));
    let centre_geo = mercator.to_geo(ProjectedCoordinate::new((f64::from(TILE.0) + 0.5) * 256.0, (f64::from(TILE.1) + 0.5) * 256.0)).unwrap();
    let centre = bte.from_geo(centre_geo).unwrap();
    let (cx, cz) = ((centre.x.floor() as i32) >> 4, (centre.y.floor() as i32) >> 4);
    let radius = 12;
    report(
        "chunk",
        "us/chunk",
        &measure(warmup, rounds, || {
            let started = Instant::now();
            let mut chunks = 0;
            for chunk_x in cx - radius..=cx + radius {
                for chunk_z in cz - radius..=cz + radius {
                    let bounds = chunk_geo_bounds(bte.as_ref(), chunk_x, chunk_z).unwrap();
                    let plan = dataset.plan(&bounds).unwrap();
                    black_box(dataset.sample(&plan, 16, 16, &tiles)[17]);
                    chunks += 1;
                }
            }
            started.elapsed().as_secs_f64() * 1e6 / f64::from(chunks)
        }),
    );

    report(
        "chunk_bounds_plan",
        "us/chunk",
        &measure(warmup, rounds, || {
            let started = Instant::now();
            let mut chunks = 0;
            for chunk_x in cx - radius..=cx + radius {
                for chunk_z in cz - radius..=cz + radius {
                    let bounds = chunk_geo_bounds(bte.as_ref(), chunk_x, chunk_z).unwrap();
                    black_box(dataset.plan(&bounds).unwrap());
                    chunks += 1;
                }
            }
            started.elapsed().as_secs_f64() * 1e6 / f64::from(chunks)
        }),
    );

    let heights = LayeredScalarDataset::new(vec![ScalarLayer { dataset: Arc::new(dataset), bounds: AxisBounds::of(-180.0, 180.0, -85.0, 85.0), priority: 0.0, condition: None, optional: None }]);
    let provider = CachingTileProvider::new(MirrorFetcher::new().with_mapping(prefix, root), 16);
    report(
        "chunk_full",
        "us/chunk",
        &measure(warmup, rounds, || {
            let started = Instant::now();
            let mut chunks = 0;
            for chunk_x in cx - radius..=cx + radius {
                for chunk_z in cz - radius..=cz + radius {
                    black_box(sample_chunk_heights(bte.as_ref(), &heights, &provider, chunk_x, chunk_z).unwrap());
                    chunks += 1;
                }
            }
            started.elapsed().as_secs_f64() * 1e6 / f64::from(chunks)
        }),
    );
}
