//! `OsmDataset` against an in-memory tile server and a projection where one 1/64-degree tile is
//! exactly one chunk: reference resolution, caching, deduplication, draw order and failures.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use terra_elevation::{ElevationError, FetchOutcome, TileFetcher};
use terra_osm::{BlockState, ChunkElements, DrawTarget, OsmDataset, OsmError, OsmMapper, OsmSettings, apply_elements};
use terra_projection::{GeoCoordinate, GeographicProjection, OutOfProjectionBounds, ProjectedCoordinate, ProjectionResult};

/// x = longitude * 1024, z = -latitude * 1024: 16 blocks per 1/64 degree.
#[derive(Debug)]
struct TileScale;

impl GeographicProjection for TileScale {
    fn to_geo(&self, point: ProjectedCoordinate) -> ProjectionResult<GeoCoordinate> {
        let geo = GeoCoordinate::new(point.x / 1024.0, -point.y / 1024.0);
        if geo.longitude.abs() > 180.0 || geo.latitude.abs() > 90.0 { Err(OutOfProjectionBounds) } else { Ok(geo) }
    }

    fn from_geo(&self, point: GeoCoordinate) -> ProjectionResult<ProjectedCoordinate> {
        if point.longitude.abs() > 180.0 || point.latitude.abs() > 90.0 {
            return Err(OutOfProjectionBounds);
        }
        Ok(ProjectedCoordinate::new(point.longitude * 1024.0, -point.latitude * 1024.0))
    }

    fn meters_per_unit(&self) -> f64 {
        1.0
    }
}

const SERVER: &str = "https://osm.test/";

#[derive(Default)]
struct FakeServer {
    resources: Mutex<HashMap<String, String>>,
    /// Tiles not listed are empty files; with this set they are 404s instead.
    unlisted_tiles_missing: bool,
    fetches: Mutex<HashMap<String, usize>>,
    total: AtomicUsize,
}

impl FakeServer {
    fn put(&self, key: &str, lines: &[String]) {
        self.resources.lock().unwrap().insert(key.to_owned(), lines.join("\n"));
    }

    fn fetches(&self, key: &str) -> usize {
        self.fetches.lock().unwrap().get(&format!("{SERVER}{key}")).copied().unwrap_or(0)
    }
}

impl TileFetcher for FakeServer {
    fn fetch(&self, url: &str) -> Result<FetchOutcome, ElevationError> {
        self.total.fetch_add(1, Ordering::SeqCst);
        *self.fetches.lock().unwrap().entry(url.to_owned()).or_default() += 1;
        let key = url.strip_prefix(SERVER).unwrap();
        match self.resources.lock().unwrap().get(key) {
            Some(body) => Ok(FetchOutcome::Found(body.clone().into_bytes())),
            None if key.starts_with("tile/") && !self.unlisted_tiles_missing => Ok(FetchOutcome::Found(Vec::new())),
            None => Ok(FetchOutcome::Missing),
        }
    }
}

fn dataset(server: &Arc<FakeServer>) -> OsmDataset {
    let settings = OsmSettings { servers: vec![SERVER.to_owned()], ..OsmSettings::default() };
    OsmDataset::new(Arc::new(OsmMapper::default_config().unwrap()), Arc::new(TileScale), Arc::clone(server) as Arc<dyn TileFetcher>, &settings)
}

/// Block coordinates to a GeoJSON position.
fn position(x: f64, z: f64) -> String {
    format!("[{},{}]", x / 1024.0, -z / 1024.0)
}

fn line_feature(id: &str, tags: &str, points: &[(f64, f64)]) -> String {
    let coordinates: Vec<String> = points.iter().map(|&(x, z)| position(x, z)).collect();
    format!(r#"{{"type":"Feature","id":"{id}","properties":{{{tags}}},"geometry":{{"type":"LineString","coordinates":[{}]}}}}"#, coordinates.join(","))
}

fn square_feature(id: &str, tags: &str, min: (f64, f64), max: (f64, f64)) -> String {
    let ring = [min, (max.0, min.1), max, (min.0, max.1), min].map(|(x, z)| position(x, z)).join(",");
    format!(r#"{{"type":"Feature","id":"{id}","properties":{{{tags}}},"geometry":{{"type":"Polygon","coordinates":[[{ring}]]}}}}"#)
}

fn reference(location: &str) -> String {
    format!(r#"{{"type":"Reference","location":"{location}"}}"#)
}

fn elements(dataset: &OsmDataset, chunk_x: i32, chunk_z: i32) -> Vec<Arc<terra_osm::VectorGeometry>> {
    match dataset.chunk_elements(chunk_x, chunk_z).unwrap() {
        ChunkElements::Elements(elements) => elements,
        ChunkElements::OutsideProjection => panic!("chunk ({chunk_x}, {chunk_z}) unexpectedly outside the projection"),
    }
}

#[derive(Default)]
struct Recorder {
    water: HashMap<(usize, usize), i32>,
    ocean: HashMap<(usize, usize), i32>,
    blocks: HashMap<(usize, usize), String>,
    no_trees: std::collections::HashSet<(usize, usize)>,
}

impl DrawTarget for Recorder {
    fn update_water_depth(&mut self, x: usize, z: usize, depth: i32) {
        let entry = self.water.entry((x, z)).or_insert(i32::MIN);
        *entry = (*entry).max(depth);
    }

    fn update_ocean_depth(&mut self, x: usize, z: usize, depth: i32) {
        let entry = self.ocean.entry((x, z)).or_insert(i32::MIN);
        *entry = (*entry).max(depth);
    }

    fn set_surface_block(&mut self, x: usize, z: usize, block: &BlockState) {
        self.blocks.insert((x, z), block.to_string());
    }

    fn clear_tree_cover(&mut self, x: usize, z: usize) {
        self.no_trees.insert((x, z));
    }
}

#[test]
fn shared_resources_resolve_once_and_their_elements_are_drawn_once() {
    let server = Arc::new(FakeServer::default());
    let river = line_feature("way/7", r#""waterway":"river""#, &[(-40.0, 8.0), (56.0, 8.0)]);
    server.put("shared/river.json", std::slice::from_ref(&river));
    for x in -3..=3 {
        for z in -3..=3 {
            server.put(&format!("tile/{x}/{z}.json"), &[reference("shared/river.json")]);
        }
    }
    let dataset = dataset(&server);
    let found = elements(&dataset, 0, 0);
    assert_eq!(found.len(), 1, "every tile references the river; it is drawn once");
    assert_eq!(found[0].id(), "way/7");
    assert_eq!(server.fetches("shared/river.json"), 1);
    let mut target = Recorder::default();
    apply_elements(&found, &mut target, 0, 0);
    for x in 0..16 {
        // radius 9: weight floor(9 - distance); water = weight - 4 where weight > 3.
        assert_eq!(target.water.get(&(x, 8)), Some(&5), "column ({x}, 8)");
        assert_eq!(target.water.get(&(x, 0)), None, "column ({x}, 0) is too far for water");
        assert!(target.no_trees.contains(&(x, 0)), "but close enough to clear trees");
    }
    let before = server.total.load(Ordering::SeqCst);
    elements(&dataset, 0, 0);
    assert_eq!(server.total.load(Ordering::SeqCst), before, "tiles are cached");
    assert!(dataset.statistics().tiles.hits > 0);
}

#[test]
fn elements_are_ordered_by_layer_then_id_and_later_layers_win() {
    let server = Arc::new(FakeServer::default());
    server.put(
        "tile/0/0.json",
        &[
            line_feature("way/b", r#""highway":"residential","layer":"1""#, &[(-8.0, 8.5), (24.0, 8.5)]),
            line_feature("way/a", r#""highway":"footway""#, &[(8.2, -8.0), (8.8, 24.0)]),
            line_feature("way/c", r#""highway":"residential","layer":"1""#, &[(-8.0, 3.5), (24.0, 3.5)]),
        ],
    );
    let dataset = dataset(&server);
    let found = elements(&dataset, 0, 0);
    let order: Vec<(f64, &str)> = found.iter().map(|element| (element.layer(), element.id())).collect();
    assert_eq!(order, vec![(0.0, "way/a"), (1.0, "way/b"), (1.0, "way/c")]);
    let mut target = Recorder::default();
    apply_elements(&found, &mut target, 0, 0);
    assert_eq!(target.blocks.get(&(8, 8)).map(String::as_str), Some("minecraft:gray_concrete"), "the road is drawn over the footpath");
    assert_eq!(target.blocks.get(&(8, 15)).map(String::as_str), Some("minecraft:dirt_path"));
}

#[test]
fn exactly_vertical_narrow_lines_only_mark_their_start_as_upstream_does() {
    let server = Arc::new(FakeServer::default());
    server.put("tile/0/0.json", &[line_feature("way/1", r#""highway":"footway""#, &[(5.5, 2.0), (5.5, 14.0)])]);
    let mut target = Recorder::default();
    apply_elements(&elements(&dataset(&server), 0, 0), &mut target, 0, 0);
    assert_eq!(target.blocks.keys().copied().collect::<Vec<_>>(), vec![(5, 2)]);
}

#[test]
fn buildings_get_brick_outlines_and_clear_trees_inside() {
    let server = Arc::new(FakeServer::default());
    server.put("tile/0/0.json", &[square_feature("way/9", r#""building":"yes""#, (4.5, 4.5), (11.5, 11.5))]);
    let dataset = dataset(&server);
    let mut target = Recorder::default();
    apply_elements(&elements(&dataset, 0, 0), &mut target, 0, 0);
    for (x, z) in [(4, 8), (11, 8), (8, 4), (8, 11), (4, 4)] {
        assert_eq!(target.blocks.get(&(x, z)).map(String::as_str), Some("minecraft:bricks"), "outline at ({x}, {z})");
    }
    assert_eq!(target.blocks.get(&(8, 8)), None);
    assert!(target.no_trees.contains(&(8, 8)));
    assert!(!target.no_trees.contains(&(0, 0)));
}

#[test]
fn coastlines_are_ocean_except_around_the_caspian_sea() {
    for (chunk_x, chunk_z, ocean) in [(64, -64, true), (64 * 50, -64 * 40, false)] {
        let server = Arc::new(FakeServer::default());
        let (base_x, base_z) = (f64::from(chunk_x * 16), f64::from(chunk_z * 16));
        let coast = square_feature("way/1", r#""natural":"coastline""#, (base_x - 64.0, base_z - 64.0), (base_x + 8.5, base_z + 80.0));
        for x in chunk_x - 2..=chunk_x + 2 {
            for z in -chunk_z - 2..=-chunk_z + 2 {
                server.put(&format!("tile/{x}/{z}.json"), std::slice::from_ref(&coast));
            }
        }
        let dataset = dataset(&server);
        let mut target = Recorder::default();
        apply_elements(&elements(&dataset, chunk_x, chunk_z), &mut target, chunk_x, chunk_z);
        let (drawn, other) = if ocean { (&target.ocean, &target.water) } else { (&target.water, &target.ocean) };
        assert!(drawn.contains_key(&(0, 8)), "chunk ({chunk_x}, {chunk_z})");
        assert!(other.is_empty());
    }
}

#[test]
fn failures_fail_the_chunk() {
    let server = Arc::new(FakeServer { unlisted_tiles_missing: true, ..FakeServer::default() });
    assert!(matches!(dataset(&server).chunk_elements(0, 0), Err(OsmError::Missing { .. })));

    let server = Arc::new(FakeServer::default());
    server.put("tile/0/0.json", &[reference("a.json")]);
    server.put("a.json", &[reference("b.json")]);
    server.put("b.json", &[reference("a.json")]);
    assert!(matches!(dataset(&server).chunk_elements(0, 0), Err(OsmError::ReferenceCycle { .. })));

    let server = Arc::new(FakeServer::default());
    server.put("tile/0/0.json", &[reference("../../etc/passwd")]);
    assert!(matches!(dataset(&server).chunk_elements(0, 0), Err(OsmError::InvalidReference { .. })));

    let server = Arc::new(FakeServer::default());
    server.put("tile/0/0.json", &["{not json".to_owned()]);
    assert!(matches!(dataset(&server).chunk_elements(0, 0), Err(OsmError::Unavailable { .. })));

    let server = Arc::new(FakeServer::default());
    server.put("tile/0/0.json", &[r#"{"type":"LineString","coordinates":[[0,0],[0.01,0.01]]}"#.replace("LineString\"", "LineString\",\"_\":0")]);
    assert!(dataset(&server).chunk_elements(0, 0).is_err(), "unknown members are rejected");

    let server = Arc::new(FakeServer::default());
    server.put("tile/0/0.json", &[r#"{"type":"Feature","properties":{"building":"yes"},"geometry":{"type":"LineString","coordinates":[[0,0],[0.01,-0.01]]}}"#.to_owned()]);
    assert!(matches!(dataset(&server).chunk_elements(0, 0), Err(OsmError::Malformed { .. })), "an element without an id cannot be ordered");
}

#[test]
fn features_without_matching_rules_and_points_are_ignored() {
    let server = Arc::new(FakeServer::default());
    server.put(
        "tile/0/0.json",
        &[
            line_feature("way/1", r#""amenity":"bench""#, &[(0.0, 0.0), (16.0, 16.0)]),
            line_feature("way/2", r#""highway":"primary","tunnel":"yes""#, &[(0.0, 0.0), (16.0, 16.0)]),
            r#"{"type":"Feature","id":"node/1","properties":{"natural":"water"},"geometry":{"type":"Point","coordinates":[0.005,-0.005]}}"#.to_owned(),
        ],
    );
    assert!(elements(&dataset(&server), 0, 0).is_empty());
}

#[test]
fn chunks_beyond_the_projection_are_reported() {
    let server = Arc::new(FakeServer::default());
    assert!(matches!(dataset(&server).chunk_elements(64 * 180, 0).unwrap(), ChunkElements::OutsideProjection));
    assert_eq!(server.total.load(Ordering::SeqCst), 0);
}

#[test]
fn negative_chunks_use_floor_tile_coordinates() {
    let server = Arc::new(FakeServer::default());
    let river = line_feature("way/3", r#""waterway":"stream""#, &[(-40.0, -7.5), (-1.0, -7.5)]);
    server.put("tile/-1/0.json", &[river]);
    let dataset = dataset(&server);
    let found = elements(&dataset, -1, -1);
    assert_eq!(found.len(), 1);
    let mut target = Recorder::default();
    apply_elements(&found, &mut target, -1, -1);
    // radius 1.5 around z = -7.5, i.e. local row 8 of chunk -1.
    assert!(target.water.contains_key(&(0, 8)));
    assert!(!target.water.contains_key(&(0, 5)));
    assert!(server.fetches("tile/-2/-1.json") > 0 && server.fetches("tile/1/2.json") > 0, "tiles -2..=1 by -1..=2");
}

/// A hostile server multiplying elements through references (each resource referencing the next
/// one 8 times, 8 levels deep: 8^8 copies of one element) is refused once a resource would hold
/// more than `MAX_ELEMENTS_PER_RESOURCE`, quickly and without exhausting memory.
#[test]
fn reference_amplification_is_bounded() {
    let server = Arc::new(FakeServer::default());
    server.put("level/8.json", &[line_feature("way/1", r#""highway":"primary""#, &[(0.0, 0.0), (8.0, 8.0)])]);
    for level in 0..8 {
        server.put(&format!("level/{level}.json"), &vec![reference(&format!("level/{}.json", level + 1)); 8]);
    }
    server.put("tile/0/0.json", &[reference("level/0.json")]);
    let started = std::time::Instant::now();
    match dataset(&server).chunk_elements(0, 0) {
        Err(OsmError::Malformed { reason, .. }) => assert!(reason.contains("elements"), "{reason}"),
        other => panic!("{:?}", other.map(|_| ())),
    }
    assert!(started.elapsed() < std::time::Duration::from_secs(10), "{:?}", started.elapsed());
    // Below the cap, shared references still work (8^5 = 32768 copies of the line).
    let server = Arc::new(FakeServer::default());
    server.put("level/5.json", &[line_feature("way/1", r#""highway":"primary""#, &[(0.0, 0.0), (8.0, 8.0)])]);
    for level in 0..5 {
        server.put(&format!("level/{level}.json"), &vec![reference(&format!("level/{}.json", level + 1)); 8]);
    }
    server.put("tile/0/0.json", &[reference("level/0.json")]);
    assert!(dataset(&server).chunk_elements(0, 0).is_ok());
}
