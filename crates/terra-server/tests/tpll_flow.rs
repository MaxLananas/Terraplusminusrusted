//! `/tpll` end to end on the offline fixture: config, stack assembly, argument parsing,
//! authorization, bounds, asynchronous surface lookup and linked-world height routing.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use terra_server::{Authorization, LevelHeight, PendingTeleports, TeleportOutcome, TeleportWorld, TerraStack, TpllRequest, TpllStep, parse_server_config};

const CANYON: &str = "35.995, -113.247";

fn scratch(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!("terra-tpll-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

fn stack(name: &str, extra: &str) -> (TerraStack, PathBuf) {
    let directory = scratch(name);
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/elevation/terrarium");
    std::fs::write(
        directory.join("heights.json5"),
        r#"[{"dataset": {"urls": ["mirror://terrarium/12/${x}/${z}.png"], "projection": {"web_mercator": {"zoom": 12}},
            "resolution": 256, "blend": "CUBIC", "parse": {"parse_png_terrarium": {}}},
            "bounds": {"minX": -180, "maxX": 180, "minZ": -85, "maxZ": 85}, "zooms": {"min": 0, "max": 3}}]"#,
    )
    .unwrap();
    let config = parse_server_config(&format!(
        r#"{{"elevation": {{"heights_config": "heights.json5"}}, "biomes": {{"use_dataset": false}},
            "network": {{"mirrors": {{"mirror://terrarium/": "{}"}}, "cache_directory": "tile-cache"}},
            "world": {{"min_y": -64, "height": 2096}}, "generation": {{"workers": 2}} {extra}}}"#,
        fixtures.display()
    ))
    .unwrap();
    (TerraStack::build(config, &directory).unwrap(), directory)
}

#[derive(Default)]
struct World {
    generated: HashSet<(String, i32, i32)>,
    heights: Vec<(String, LevelHeight)>,
}

impl TeleportWorld for World {
    fn level_height(&self, world: &str) -> Option<LevelHeight> {
        self.heights.iter().find(|(name, _)| name == world).map(|(_, height)| *height)
    }
    fn is_chunk_generated(&self, world: &str, chunk_x: i32, chunk_z: i32) -> bool {
        self.generated.contains(&(world.to_owned(), chunk_x, chunk_z))
    }
    fn highest_block_y(&self, _: &str, _: i32, _: i32) -> Option<i32> {
        None
    }
}

const PLAYER: Authorization = Authorization { sender_is_target: true, admin: false, may_generate_chunks: true };

fn world(levels: &[(&str, i32, i32)]) -> World {
    World { heights: levels.iter().map(|(name, min_y, max_y)| ((*name).to_owned(), LevelHeight { min_y: *min_y, max_y: *max_y })).collect(), ..World::default() }
}

fn start(stack: &TerraStack, arguments: &str, authorization: Authorization, world: &World) -> (Option<TpllRequest>, TpllStep) {
    let config = stack.config();
    TpllRequest::start(arguments, "earth", stack.generator().projection().as_ref(), &config.team_bounds, authorization, &config.linked_worlds, config.generator.y_offset, world)
}

fn wait_for(pending: &mut PendingTeleports<u32>, stack: &TerraStack, world: &World) -> TeleportOutcome {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some((_, outcome)) = pending.poll(&stack.config().linked_worlds, world).pop() {
            return outcome;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn teleports_onto_the_generated_surface() {
    let (stack, directory) = stack("surface", "");
    let world = world(&[("earth", -64, 2032)]);
    let (request, step) = start(&stack, CANYON, PLAYER, &world);
    let TpllStep::NeedsSurface { chunk_x, chunk_z, local_x, local_z } = step.clone() else { panic!("{step:?}") };
    let mut pending = PendingTeleports::new(4, Duration::from_secs(30));
    pending.submit(7, request.unwrap(), &step, PLAYER, stack.loader()).unwrap();
    let TeleportOutcome::Step(TpllStep::Teleport { world: level, y, reported_height, .. }) = wait_for(&mut pending, &stack, &world) else { panic!() };
    let terrain = stack.loader().load_blocking(chunk_x, chunk_z, Duration::from_secs(1)).unwrap();
    let surface = terrain.surface_height(local_x, local_z);
    assert_eq!((level.as_str(), y as i32, reported_height), ("earth", surface + 1, surface + 1));
    assert!((600..2200).contains(&surface));
    assert!(directory.join("tile-cache/v1").is_dir());
    assert_eq!(stack.verify_world(&directory.join("world")).unwrap(), terra_chunk_generation::FingerprintStatus::Created);
}

#[test]
fn enforces_authorization_and_bounds() {
    let (stack, _) = stack("authorization", r#", "min_latitude": 46.9, "max_latitude": 55.3, "min_longitude": 1.9, "max_longitude": 15.6"#);
    let world = world(&[("earth", -64, 2032)]);
    assert_eq!(start(&stack, "not coordinates", PLAYER, &world).1, TpllStep::Usage);
    assert_eq!(start(&stack, CANYON, PLAYER, &world).1, TpllStep::OutsideTeamBounds);
    let admin = Authorization { admin: true, ..PLAYER };
    assert!(matches!(start(&stack, CANYON, admin, &world).1, TpllStep::NeedsSurface { .. }));
    let other_player = Authorization { sender_is_target: false, ..PLAYER };
    assert!(matches!(start(&stack, CANYON, other_player, &world).1, TpllStep::NeedsSurface { .. }));
    let restricted = Authorization { may_generate_chunks: false, admin: true, ..PLAYER };
    assert_eq!(start(&stack, CANYON, restricted, &world).1, TpllStep::UngeneratedChunkDenied);
    // Upstream accepts a latitude above 90 when the pair would fit swapped, but does not swap it.
    assert_eq!(start(&stack, "100 50", admin, &world).1, TpllStep::OutsideProjection);
}

#[test]
fn explicit_heights_route_through_linked_worlds() {
    let linked = r#", "linked_worlds": {"enabled": true, "method": "MULTIVERSE", "worlds": [{"name": "earth", "offset": 0}, {"name": "sky", "offset": -2032}]}"#;
    let (stack, _) = stack("linked", linked);
    let world = world(&[("earth", -64, 2032), ("sky", -64, 2032)]);
    let TpllStep::Teleport { world: level, y, .. } = start(&stack, "35.995 -113.247 100", PLAYER, &world).1 else { panic!() };
    assert_eq!((level.as_str(), y), ("earth", 100.0));
    let TpllStep::Teleport { world: level, y, reported_height, .. } = start(&stack, "35.995 -113.247 3000", PLAYER, &world).1 else { panic!() };
    assert_eq!((level.as_str(), y, reported_height), ("sky", 3000.0 - 2032.0 + 1.0, 3000));
    assert_eq!(start(&stack, "35.995 -113.247 9000", PLAYER, &world).1, TpllStep::Unreachable);
}

#[test]
fn pending_teleports_are_bounded_and_replaceable() {
    let (stack, _) = stack("bounded", "");
    let world = world(&[("earth", -64, 2032)]);
    let mut pending = PendingTeleports::new(1, Duration::from_secs(30));
    let (request, step) = start(&stack, CANYON, PLAYER, &world);
    pending.submit(1, request.clone().unwrap(), &step, PLAYER, stack.loader()).unwrap();
    pending.submit(1, request.clone().unwrap(), &step, PLAYER, stack.loader()).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending.submit(2, request.unwrap(), &step, PLAYER, stack.loader()), Err(terra_server::TeleportRefusal::TooManyPending));
    assert!(matches!(wait_for(&mut pending, &stack, &world), TeleportOutcome::Step(TpllStep::Teleport { .. })));
}
