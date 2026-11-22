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
    let config = parse_server_config(
        r#"{"network": {"plain_http_hosts": ["s3.amazonaws.com"], "cache_directory": "tile-cache"}, "openstreetmap": {"enabled": false},
            "generate_trees": false, "biomes": {"use_dataset": false}, "world": {"min_y": -64, "height": 2096}}"#,
    )
    .unwrap();
    let stack = TerraStack::build(config, &directory).unwrap();
    let block = stack.generator().projection().from_geo(terra_projection::GeoCoordinate::new(-113.247, 35.995)).unwrap();
    let (chunk_x, chunk_z) = ((block.x.floor() as i32) >> 4, (block.y.floor() as i32) >> 4);
    let terrain = terrain_for_generation(&stack, &GenerationControl::default(), chunk_x, chunk_z, &mut |message| eprintln!("{message}")).unwrap();
    let ground = terrain.ground_height(8, 8);
    // The fixture tile of the same area gives ~1486 at the canyon rim column used by the smoke test.
    assert!((500..2032).contains(&ground), "ground {ground}");
}
