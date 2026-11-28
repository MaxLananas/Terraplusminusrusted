//! Hostile OpenStreetMap tiles through the whole pipeline (GeoJSON parsing, references, the
//! mapping, projection and rasterisation): no panic, no write outside the chunk, no runaway time.
//! Generated features use extreme coordinates and tag values; byte mutations of a recorded scene
//! cover malformed input. Seeds are fixed, so failures reproduce.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::{Duration, Instant};
use terra_elevation::{ElevationError, FetchOutcome, TileFetcher};
use terra_osm::{BlockState, ChunkElements, DrawTarget, OsmDataset, OsmMapper, OsmSettings, apply_elements};
use terra_projection::{GeoCoordinate, GeographicProjection, OutOfProjectionBounds, ProjectedCoordinate, ProjectionResult};

#[derive(Debug)]
struct TileScale;

impl GeographicProjection for TileScale {
    fn to_geo(&self, point: ProjectedCoordinate) -> ProjectionResult<GeoCoordinate> {
        let geo = GeoCoordinate::new(point.x / 1024.0, -point.y / 1024.0);
        if geo.longitude.abs() <= 180.0 && geo.latitude.abs() <= 90.0 { Ok(geo) } else { Err(OutOfProjectionBounds) }
    }

    fn from_geo(&self, point: GeoCoordinate) -> ProjectionResult<ProjectedCoordinate> {
        if point.longitude.abs() <= 180.0 && point.latitude.abs() <= 90.0 {
            return Ok(ProjectedCoordinate::new(point.longitude * 1024.0, -point.latitude * 1024.0));
        }
        Err(OutOfProjectionBounds)
    }

    fn meters_per_unit(&self) -> f64 {
        1.0
    }
}

/// The recorded scenes' stand-in projection (`fixtures/osm/scenes.tsv`).
#[derive(Debug)]
struct Linear {
    lon0: f64,
    lat0: f64,
    scale: f64,
}

impl GeographicProjection for Linear {
    fn to_geo(&self, point: ProjectedCoordinate) -> ProjectionResult<GeoCoordinate> {
        Ok(GeoCoordinate::new(point.x / self.scale + self.lon0, self.lat0 - point.y / self.scale))
    }

    fn from_geo(&self, point: GeoCoordinate) -> ProjectionResult<ProjectedCoordinate> {
        if point.longitude.abs() <= 180.0 {
            return Ok(ProjectedCoordinate::new((point.longitude - self.lon0) * self.scale, (self.lat0 - point.latitude) * self.scale));
        }
        Err(OutOfProjectionBounds)
    }

    fn meters_per_unit(&self) -> f64 {
        1.0
    }
}

const SERVER: &str = "https://osm.test/";

/// Every `tile/x/z.json` is `tile`; `shared/a.json` is `shared`; everything else is missing.
struct Server {
    tile: Vec<u8>,
    shared: Option<Vec<u8>>,
}

impl TileFetcher for Server {
    fn fetch(&self, url: &str) -> Result<FetchOutcome, ElevationError> {
        let key = url.strip_prefix(SERVER).unwrap_or(url);
        Ok(match (key, &self.shared) {
            (key, _) if key.starts_with("tile/") => FetchOutcome::Found(self.tile.clone()),
            ("shared/a.json", Some(shared)) => FetchOutcome::Found(shared.clone()),
            _ => FetchOutcome::Missing,
        })
    }
}

#[derive(Default)]
struct Bounded {
    writes: usize,
}

impl Bounded {
    fn check(&mut self, x: usize, z: usize) {
        assert!(x < 16 && z < 16, "write outside the chunk at ({x}, {z})");
        self.writes += 1;
    }
}

impl DrawTarget for Bounded {
    fn update_water_depth(&mut self, x: usize, z: usize, _: i32) {
        self.check(x, z);
    }
    fn update_ocean_depth(&mut self, x: usize, z: usize, _: i32) {
        self.check(x, z);
    }
    fn set_surface_block(&mut self, x: usize, z: usize, _: &BlockState) {
        self.check(x, z);
    }
    fn clear_tree_cover(&mut self, x: usize, z: usize) {
        self.check(x, z);
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }
    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

fn coordinate(rng: &mut Rng) -> String {
    let extreme = ["0", "-0", "1e300", "-1e300", "179.9999999", "-180", "180.0000001", "90", "-90.5", "1e-320", "5e-324", "123456789012345678901234567890"];
    let (lon, lat) = if rng.below(8) == 0 {
        (rng.pick(&extreme).to_owned(), rng.pick(&extreme).to_owned())
    } else {
        // Mostly inside tile 0/0 and its neighbours: longitude [-1/64, 2/64], latitude [-2/64, 1/64].
        let lon = (rng.below(3000) as f64 / 1000.0 - 1.0) / 64.0;
        let lat = -(rng.below(3000) as f64 / 1000.0 - 1.0) / 64.0;
        (format!("{lon}"), format!("{lat}"))
    };
    format!("[{lon},{lat}]")
}

fn ring(rng: &mut Rng, closed: bool) -> String {
    let count = match rng.below(10) {
        0 => 0,
        1 => 1,
        2 => 2000,
        _ => 2 + rng.below(8),
    };
    let mut points: Vec<String> = (0..count).map(|_| coordinate(rng)).collect();
    if closed && let Some(first) = points.first().cloned() {
        points.push(first);
    }
    format!("[{}]", points.join(","))
}

fn geometry(rng: &mut Rng, depth: usize) -> String {
    match rng.below(if depth > 2 { 5 } else { 6 }) {
        0 => format!(r#"{{"type":"Point","coordinates":{}}}"#, coordinate(rng)),
        1 => format!(r#"{{"type":"LineString","coordinates":{}}}"#, ring(rng, false)),
        2 => format!(r#"{{"type":"MultiLineString","coordinates":[{},{}]}}"#, ring(rng, false), ring(rng, false)),
        3 => format!(r#"{{"type":"Polygon","coordinates":[{},{}]}}"#, ring(rng, true), ring(rng, true)),
        4 => format!(r#"{{"type":"MultiPolygon","coordinates":[[{}],[{},{}]]}}"#, ring(rng, true), ring(rng, true), ring(rng, true)),
        _ => format!(r#"{{"type":"GeometryCollection","geometries":[{},{}]}}"#, geometry(rng, depth + 1), geometry(rng, depth + 1)),
    }
}

fn tags(rng: &mut Rng) -> String {
    let keys = ["highway", "building", "natural", "waterway", "water", "landuse", "layer", "lanes", "width", "bridge", "tunnel", "leisure", "amenity", "area"];
    let values = [
        "motorway", "residential", "footway", "path", "primary", "yes", "no", "water", "coastline", "river", "stream", "riverbank", "lake", "1", "-1", "5", "-5", "0", "1e309",
        "-1e309", "NaN", "Infinity", "-Infinity", "1e300", "abc", "", "3.5", "2147483648", "-2147483649", "0x10",
    ];
    let primary = [r#""highway":"primary""#, r#""building":"yes""#, r#""natural":"water""#, r#""waterway":"river""#, r#""natural":"coastline""#, r#""highway":"footway""#];
    let count = rng.below(5);
    let pairs: Vec<String> = std::iter::once(rng.pick(&primary).to_owned())
        .chain((0..count).map(|_| format!(r#""{}":"{}""#, rng.pick(&keys), rng.pick(&values))))
        .collect();
    format!("{{{}}}", pairs.join(","))
}

fn feature(rng: &mut Rng, index: usize) -> String {
    let id = match rng.below(12) {
        0 => String::new(),
        1 => r#","id":"relation/1""#.to_owned(),
        _ => format!(r#","id":"way/{index}""#),
    };
    format!(r#"{{"type":"Feature","geometry":{},"properties":{}{id}}}"#, geometry(rng, 0), tags(rng))
}

fn mappings() -> Vec<Arc<OsmMapper>> {
    let exhaustive = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/osm/mappings/exhaustive.json5")).unwrap();
    vec![Arc::new(OsmMapper::default_config().unwrap()), Arc::new(OsmMapper::parse(&exhaustive).unwrap())]
}

/// Serves `tile` as every tile (and `shared` as the target of references) and draws `chunks`;
/// returns the number of columns written.
fn run_case(mapper: &Arc<OsmMapper>, projection: Arc<dyn GeographicProjection>, tile: Vec<u8>, shared: Option<Vec<u8>>, chunks: &[(i32, i32)]) -> usize {
    let server = Arc::new(Server { tile, shared });
    let settings = OsmSettings { servers: vec![SERVER.to_owned()], ..OsmSettings::default() };
    let dataset = OsmDataset::new(Arc::clone(mapper), projection, server as Arc<dyn TileFetcher>, &settings);
    let mut writes = 0;
    for &(chunk_x, chunk_z) in chunks {
        let started = Instant::now();
        if let Ok(ChunkElements::Elements(elements)) = dataset.chunk_elements(chunk_x, chunk_z) {
            let mut target = Bounded::default();
            apply_elements(&elements, &mut target, chunk_x, chunk_z);
            writes += target.writes;
        }
        assert!(started.elapsed() < Duration::from_secs(5), "chunk ({chunk_x}, {chunk_z}) took {:?}", started.elapsed());
    }
    writes
}

fn block(range: std::ops::RangeInclusive<i32>) -> Vec<(i32, i32)> {
    range.clone().flat_map(|x| range.clone().map(move |z| (x, z))).collect()
}

struct Scene {
    tile: Vec<u8>,
    mapper: Arc<OsmMapper>,
    projection: Arc<dyn GeographicProjection>,
    chunks: Vec<(i32, i32)>,
}

fn scenes() -> Vec<Scene> {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/osm");
    let manifest = std::fs::read_to_string(format!("{root}/scenes.tsv")).unwrap();
    let mappers = mappings();
    manifest
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            let number = |i: usize| f[i].parse::<f64>().unwrap();
            let chunk = |i: usize| f[i].parse::<i32>().unwrap();
            let chunks = (chunk(5)..=chunk(6)).flat_map(|x| (chunk(7)..=chunk(8)).map(move |z| (x, z))).collect();
            Scene {
                tile: std::fs::read(format!("{root}/{}", f[1])).unwrap(),
                mapper: Arc::clone(&mappers[usize::from(f[9] != "default")]),
                projection: Arc::new(Linear { lon0: number(2), lat0: number(3), scale: number(4) }),
                chunks,
            }
        })
        .collect()
}

fn failing_input(case: usize, run: impl FnOnce()) -> Option<String> {
    catch_unwind(AssertUnwindSafe(run)).err().map(|payload| {
        let message = payload.downcast_ref::<String>().cloned().or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned())).unwrap_or_default();
        format!("case {case}: {message}")
    })
}

#[test]
fn generated_hostile_features_are_handled() {
    let mappers = mappings();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut failures = Vec::new();
    let mut total_writes = 0;
    for case in 0..600 {
        let count = 1 + rng.below(6);
        let lines: Vec<String> = (0..count).map(|index| feature(&mut rng, index)).collect();
        let tile = lines.join("\n").into_bytes();
        let mapper = &mappers[case % mappers.len()];
        if let Some(failure) = failing_input(case, || total_writes += run_case(mapper, Arc::new(TileScale), tile.clone(), None, &block(-1..=1))) {
            failures.push(format!("{failure}\n  input: {}", String::from_utf8_lossy(&tile).chars().take(600).collect::<String>()));
        }
    }
    eprintln!("generated cases drew {total_writes} columns");
    assert!(total_writes > 10_000, "the generated features barely reach the chunks ({total_writes} writes): the fuzzing would be vacuous");
    assert!(failures.is_empty(), "{} of 600 cases failed:\n{}", failures.len(), failures.iter().take(5).cloned().collect::<Vec<_>>().join("\n"));
}

#[test]
fn mutated_recorded_scenes_are_handled() {
    let scenes = scenes();
    let reference = br#"{"type":"Reference","location":"shared/a.json"}"#.to_vec();
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    let mut failures = Vec::new();
    let mut total_writes = 0;
    for case in 0..1500 {
        let scene = &scenes[case % scenes.len()];
        let mut tile = scene.tile.clone();
        for _ in 0..1 + rng.below(8) {
            let at = rng.below(tile.len().max(1));
            match rng.below(4) {
                0 if !tile.is_empty() => tile[at] = rng.next() as u8,
                1 if !tile.is_empty() => {
                    tile.remove(at);
                }
                2 => tile.insert(at.min(tile.len()), b"{}[],:\"0e9-.\n"[rng.below(13)]),
                _ => {
                    let end = (at + rng.below(64)).min(tile.len());
                    let copy = tile[at.min(end)..end].to_vec();
                    tile.splice(at.min(tile.len())..at.min(tile.len()), copy);
                }
            }
        }
        let shared = (case % 5 == 0).then(|| tile.clone());
        let served = if case % 5 == 0 { reference.clone() } else { tile.clone() };
        let projection = Arc::clone(&scene.projection);
        if let Some(failure) = failing_input(case, || total_writes += run_case(&scene.mapper, projection, served, shared, &scene.chunks)) {
            failures.push(failure);
        }
    }
    eprintln!("mutated scenes drew {total_writes} columns");
    assert!(total_writes > 10_000, "the mutated scenes barely reach the chunks ({total_writes} writes)");
    assert!(failures.is_empty(), "{} of 1500 mutations failed:\n{}", failures.len(), failures.iter().take(5).cloned().collect::<Vec<_>>().join("\n"));
}
