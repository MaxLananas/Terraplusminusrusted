//! Same workloads as `tools/bench/Bench.java` (`tools/bench/run.sh` runs both), through the same entry points the server uses
//! (heights config -> `TileFormat::parse`, `bte_world_projection`). Run from the directory that
//! holds the inputs: `cargo run --release --example bench_vs_java -- <warm s> <run s>`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::hint::black_box;
use std::time::{Duration, Instant};
use terra_elevation::{TileFormat, UnsupportedLayerPolicy, load_heights_config};
use terra_projection::{GeoCoordinate, ProjectedCoordinate, bte_world_projection};

fn format(parse: &str, resolution: u32) -> TileFormat {
    let config = format!(
        r#"[{{"dataset": {{"urls": ["https://example.invalid/${{x}}/${{z}}.png"], "projection": {{"equirectangular": {{}}}},
        "resolution": {resolution}, "blend": "NEAR", "parse": {parse}}}, "bounds": [{{"minX": -1, "maxX": 1, "minZ": -1, "maxZ": 1}}], "zooms": {{"min": 0, "max": 3}}}}]"#
    );
    let loaded = load_heights_config(&config, UnsupportedLayerPolicy::Reject).unwrap();
    loaded.dataset.layers()[0].dataset.format.clone()
}

fn measure(name: &str, warm: f64, run: f64, mut op: impl FnMut() -> f64) {
    let end = Instant::now() + Duration::from_secs_f64(warm);
    while Instant::now() < end {
        black_box(op());
    }
    let mut samples = Vec::new();
    let end = Instant::now() + Duration::from_secs_f64(run);
    while Instant::now() < end && samples.len() < 4096 {
        let start = Instant::now();
        black_box(op());
        samples.push(start.elapsed().as_secs_f64() * 1e6);
    }
    samples.sort_by(f64::total_cmp);
    let n = samples.len();
    println!("{name}\t{n}\t{:.1}\t{:.1}", samples[n / 2], samples[(n as f64 * 0.99) as usize]);
}

fn main() {
    let args: Vec<f64> = std::env::args().skip(1).map(|a| a.parse().unwrap()).collect();
    let (warm, run) = (args[0], args[1]);
    let png = std::fs::read("terrarium_256.png").unwrap();
    let jpg = std::fs::read("tile_256.jpg").unwrap();
    let f32 = std::fs::read("dem_f32_deflate_1024.tif").unwrap();
    let i16 = std::fs::read("dem_i16_deflate_pred2_512.tif").unwrap();
    let terrarium = format(r#"{"parse_png_terrarium": {}}"#, 256);
    let jpeg = format(r#"{"from_int": {"delegate": {"parse_jpg": {}}}}"#, 256);
    let tiff_1024 = format(r#"{"parse_tiff": {}}"#, 1024);
    let tiff_512 = format(r#"{"parse_tiff": {}}"#, 512);
    println!("name\titerations\tmedian_us\tp99_us");
    measure("terrarium_png_256", warm, run, || terrarium.parse(&png, 256).unwrap()[12345]);
    measure("jpeg_256", warm, run, || jpeg.parse(&jpg, 256).unwrap()[12345]);
    measure("tiff_f32_deflate_1024", warm, run, || tiff_1024.parse(&f32, 1024).unwrap()[12345]);
    measure("tiff_i16_pred2_512", warm, run, || tiff_512.parse(&i16, 512).unwrap()[12345]);

    let bte = bte_world_projection().unwrap();
    // Points spread uniformly over the same ranges as the Java side (not the same sequence).
    let mut state = 42u64;
    let mut next = move || {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
    };
    let points: Vec<GeoCoordinate> = (0..100_000).map(|_| GeoCoordinate::new(next() * 360.0 - 180.0, next() * 170.0 - 85.0)).collect();
    measure("bte_fromGeo_100k", warm, run, || points.iter().map(|&p| bte.from_geo(p).unwrap().x).sum());
    let projected: Vec<ProjectedCoordinate> = points.iter().map(|&p| bte.from_geo(p).unwrap()).collect();
    measure("bte_toGeo_100k", warm, run, || projected.iter().map(|&p| bte.to_geo(p).unwrap().longitude).sum());
}
