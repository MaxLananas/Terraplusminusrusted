//! Live tests against the real providers, run by `scripts/verify.sh --live`
//! (`scripts/run-live-tests.sh`). Ignored by default; every test here is named `live_*` and
//! refuses to run without `TERRA_LIVE_TESTS=1`, so it can never pass vacuously offline.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use terra_server::{GenerationControl, TerraStack, parse_server_config, terrain_for_generation};

fn require_live() {
    assert_eq!(std::env::var("TERRA_LIVE_TESTS").as_deref(), Ok("1"), "live tests need TERRA_LIVE_TESTS=1 and network access");
}

#[test]
#[ignore = "live: fetches AWS Terrain Tiles from s3.amazonaws.com over plain HTTP"]
fn live_aws_terrain_tiles_give_real_grand_canyon_elevation() {
    require_live();
    let directory = common::scratch("live-aws");
    // The two AWS layers alone: the built-in default would take the USGS DEM over HTTPS here.
    let heights = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/aws-terrain-tiles/heights.json5");
    let config = parse_server_config(&format!(
        r#"{{"network": {{"plain_http_hosts": ["s3.amazonaws.com"], "cache_directory": "tile-cache"}}, "openstreetmap": {{"enabled": false}},
            "elevation": {{"heights_config": "{heights}", "ign_lidar_hd": false}},
            "generate_trees": false, "biomes": {{"use_dataset": false}}, "world": {{"min_y": -64, "height": 2096}}}}"#
    ))
    .unwrap();
    let stack = TerraStack::build(config, &directory).unwrap();
    let block = stack.generator().projection().from_geo(terra_projection::GeoCoordinate::new(-113.247, 35.995)).unwrap();
    let (chunk_x, chunk_z) = ((block.x.floor() as i32) >> 4, (block.y.floor() as i32) >> 4);
    let terrain = terrain_for_generation(&stack, &GenerationControl::default(), chunk_x, chunk_z, &mut |message| eprintln!("{message}")).unwrap();
    let ground = terrain.ground_height(8, 8);
    // The fixture tile of the same area gives ~1486 at the canyon rim column used by the smoke test.
    assert!((500..2032).contains(&ground), "ground {ground}");
}

/// The ground height of the column at `lon`, `lat`, through the whole stack (curl, the real
/// services, decoding, blending, the generator).
fn live_ground(stack: &TerraStack, lon: f64, lat: f64) -> i32 {
    let block = stack.generator().projection().from_geo(terra_projection::GeoCoordinate::new(lon, lat)).unwrap();
    let (block_x, block_z) = (block.x.floor() as i32, block.y.floor() as i32);
    let terrain = terrain_for_generation(stack, &GenerationControl::default(), block_x >> 4, block_z >> 4, &mut |message| eprintln!("{message}")).unwrap();
    terrain.ground_height((block_x & 15) as usize, (block_z & 15) as usize)
}

#[test]
#[ignore = "live: fetches IGN LiDAR HD from data.geopf.fr and RGE ALTI from cloud.daporkchop.net through curl"]
fn live_ign_lidar_hd_gives_the_surveyed_mont_blanc_and_falls_back_in_guadeloupe() {
    require_live();
    let directory = common::scratch("live-ign");
    let stack_at = |y_offset: i32| {
        let config = parse_server_config(&format!(
            r#"{{"network": {{"cache_directory": "tile-cache"}}, "openstreetmap": {{"enabled": false}}, "generate_trees": false, "biomes": {{"use_dataset": false}},
                "world": {{"min_y": -64, "height": 2096}}, "terrain_offset": {{"y": {y_offset}}}}}"#
        ))
        .unwrap();
        TerraStack::build(config, &directory).unwrap()
    };
    let stack = stack_at(-3000);
    assert!(stack.generator().heights().layers().iter().any(|layer| layer.dataset.id.starts_with("ign-lidar-hd-")), "IGN LiDAR HD was left out");
    // Mont Blanc summit: 4805.59 m surveyed in 2021, 4805.43 m in LiDAR HD, 4755.64 m in RGE
    // ALTI (IGN altimetry service). Only LiDAR HD lands in this window.
    let summit = live_ground(&stack, 6.865175, 45.832622) + 3000;
    assert!((4795..=4810).contains(&summit), "Mont Blanc summit at {summit} m");
    let stack = stack_at(0);
    // Paris, Hôtel de Ville: 35.19 m (LiDAR HD).
    let paris = live_ground(&stack, 2.3488, 48.8534);
    assert!((32..=38).contains(&paris), "Paris at {paris} m");
    // Guadeloupe, no LiDAR HD published (November 2026): RGE ALTI's 1442.53 m.
    let guadeloupe = live_ground(&stack, -61.6634, 16.0445);
    assert!((1430..=1455).contains(&guadeloupe), "Guadeloupe at {guadeloupe} m");
}
