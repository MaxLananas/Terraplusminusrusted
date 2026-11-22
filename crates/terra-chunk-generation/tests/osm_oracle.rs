//! Differential test of the OpenStreetMap path against TerraMinusMinus itself: the scenes in
//! `fixtures/osm` were mapped and drawn by upstream's osm.json5 parser, GeoJSON readers, vector
//! geometries, BVH and `OSMBaker` ordering (`tools/oracle/build-oracle.sh`, `OsmGoldenDump`).
//! Here the same tiles go through `OsmDataset` (served by a fake tile server where every tile
//! holds the scene) onto `ChunkDataBuilder`, and every element and every column must match.
//! Scenes use the bundled osm.json5 (byte-identical to upstream's) unless they name a mapping.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use terra_chunk_generation::ChunkDataBuilder;
use terra_elevation::{ElevationError, FetchOutcome, TileFetcher};
use terra_osm::{ChunkElements, OsmDataset, OsmMapper, OsmSettings, apply_elements};
use terra_projection::{GeoCoordinate, GeographicProjection, OutOfProjectionBounds, ProjectedCoordinate, ProjectionResult};

const SERVER: &str = "https://osm.oracle/";
/// `OsmGoldenDump.TREE_COVER`: the chance every column starts with before `no_trees` draws.
const TREE_COVER: &str = "5a";

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/osm")
}

/// The scenes' stand-in projection, written with the same operations as the oracle's lambda.
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
        if point.longitude.abs() > 180.0 {
            return Err(OutOfProjectionBounds);
        }
        Ok(ProjectedCoordinate::new((point.longitude - self.lon0) * self.scale, (self.lat0 - point.latitude) * self.scale))
    }

    fn meters_per_unit(&self) -> f64 {
        1.0
    }
}

/// Every `tile/x/z.json` is the scene's tile file; other keys are files relative to it.
struct SceneServer {
    tile: PathBuf,
}

impl TileFetcher for SceneServer {
    fn fetch(&self, url: &str) -> Result<FetchOutcome, ElevationError> {
        let key = url.strip_prefix(SERVER).unwrap();
        let path = if key.starts_with("tile/") { self.tile.clone() } else { self.tile.parent().unwrap().join(key) };
        Ok(std::fs::read(path).map_or(FetchOutcome::Missing, FetchOutcome::Found))
    }
}

fn golden(name: &str) -> Vec<Vec<String>> {
    let compressed = std::fs::read(fixtures().join("golden").join(name)).unwrap();
    let text = String::from_utf8(terra_codec::gunzip(&compressed, 64 << 20).unwrap()).unwrap();
    text.lines().skip(1).map(|line| line.split('\t').map(str::to_owned).collect()).collect()
}

fn bits(value: f64) -> String {
    format!("{:016x}", value.to_bits())
}

struct Scene {
    name: String,
    dataset: OsmDataset,
    chunks: (i32, i32, i32, i32),
}

fn scenes() -> Vec<Scene> {
    let manifest = std::fs::read_to_string(fixtures().join("scenes.tsv")).unwrap();
    let default_mapper = Arc::new(OsmMapper::default_config().unwrap());
    let settings = OsmSettings { servers: vec![SERVER.to_owned()], ..OsmSettings::default() };
    manifest
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            let number = |index: usize| fields[index].parse::<f64>().unwrap();
            let chunk = |index: usize| fields[index].parse::<i32>().unwrap();
            let projection = Linear { lon0: number(2), lat0: number(3), scale: number(4) };
            let server = SceneServer { tile: fixtures().join(Path::new(fields[1])) };
            let mapper = match fields[9] {
                "default" => Arc::clone(&default_mapper),
                custom => Arc::new(OsmMapper::parse(&std::fs::read_to_string(fixtures().join(custom)).unwrap()).unwrap()),
            };
            let dataset = OsmDataset::new(mapper, Arc::new(projection), Arc::new(server), &settings);
            Scene { name: fields[0].to_owned(), dataset, chunks: (chunk(5), chunk(6), chunk(7), chunk(8)) }
        })
        .collect()
}

#[test]
fn mapped_elements_match_upstream() {
    let mut expected: HashMap<String, Vec<String>> = HashMap::new();
    for row in golden("elements.tsv.gz") {
        expected.entry(row[0].clone()).or_default().push(row[2..].join("\t"));
    }
    let scenes = scenes();
    assert_eq!(scenes.len(), expected.len());
    for scene in &scenes {
        let actual: Vec<String> = scene
            .dataset
            .tile_elements(0, 0)
            .unwrap()
            .iter()
            .map(|element| {
                let bounds = terra_osm::bounds::Bounded::bounds(element.as_ref());
                [
                    element.kind_name().to_owned(),
                    element.id().to_owned(),
                    bits(element.layer()),
                    bits(bounds.min_x),
                    bits(bounds.max_x),
                    bits(bounds.min_z),
                    bits(bounds.max_z),
                ]
                .join("\t")
            })
            .collect();
        assert_eq!(&actual, &expected[&scene.name], "elements of scene {}", scene.name);
    }
}

#[test]
fn drawn_chunks_match_upstream_column_for_column() {
    let rows = golden("chunks.tsv.gz");
    let scenes = scenes();
    let mut compared = 0;
    let mut mismatches = Vec::new();
    for scene in &scenes {
        let (min_x, max_x, min_z, max_z) = scene.chunks;
        let mut scene_rows = rows.iter().filter(|row| row[0] == scene.name);
        for chunk_x in min_x..=max_x {
            for chunk_z in min_z..=max_z {
                let row = scene_rows.next().unwrap();
                assert_eq!((row[1].parse::<i32>().unwrap(), row[2].parse::<i32>().unwrap()), (chunk_x, chunk_z));
                let ChunkElements::Elements(elements) = scene.dataset.chunk_elements(chunk_x, chunk_z).unwrap() else {
                    panic!("{} chunk {chunk_x},{chunk_z} is outside the projection", scene.name);
                };
                let mut builder = ChunkDataBuilder::default();
                apply_elements(&elements, &mut builder, chunk_x, chunk_z);

                let expected_blocks: Vec<&str> = row[5].split(' ').collect();
                for x in 0..16 {
                    for z in 0..16 {
                        let column = x * 16 + z;
                        let water = format!("{:02x}", builder.water_depth(x, z) as u8);
                        let trees = if builder.trees_cleared(x, z) { "00" } else { TREE_COVER };
                        let block = builder.surface_block(x, z).map_or("-", |block| block.0.as_str());
                        let expected = (&row[3][column * 2..column * 2 + 2], &row[4][column * 2..column * 2 + 2], expected_blocks[column]);
                        if (water.as_str(), trees, block) != expected {
                            mismatches.push(format!(
                                "{} chunk {chunk_x},{chunk_z} column {x},{z}: expected {expected:?}, got {:?}",
                                scene.name,
                                (water, trees, block)
                            ));
                        }
                    }
                }
                compared += 1;
            }
        }
        assert!(scene_rows.next().is_none(), "golden has more chunks for {}", scene.name);
    }
    assert_eq!(compared, rows.len());
    let shown: Vec<&String> = mismatches.iter().take(40).collect();
    assert!(mismatches.is_empty(), "{} of {} columns differ:\n{shown:#?}", mismatches.len(), compared * 256);
}

/// The scenes exercise what they claim to: every kind of draw shows up somewhere.
#[test]
fn scenes_cover_every_draw_outcome() {
    let rows = golden("chunks.tsv.gz");
    let water_types: std::collections::BTreeSet<u8> =
        rows.iter().flat_map(|row| (0..256).map(move |i| u8::from_str_radix(&row[3][i * 2..i * 2 + 2], 16).unwrap() & 0xC0)).collect();
    assert_eq!(water_types, [0x00, 0x40, 0x80].into_iter().collect(), "water, ocean and untouched columns");
    let blocks: std::collections::BTreeSet<&str> = rows.iter().flat_map(|row| row[5].split(' ')).collect();
    for block in ["-", "minecraft:bricks", "minecraft:dirt_path", "minecraft:gray_concrete"] {
        assert!(blocks.contains(block), "{block} never drawn");
    }
    assert!(rows.iter().any(|row| row[4].contains("00")), "no_trees never drawn");
    for block in ["minecraft:oak_slab[type=top,waterlogged=false]", "minecraft:farmland[moisture=7]", "minecraft:stone_bricks", "minecraft:sand"] {
        assert!(blocks.contains(block), "{block} never drawn");
    }
    let depths: std::collections::BTreeSet<u8> =
        rows.iter().flat_map(|row| (0..256).map(move |i| u8::from_str_radix(&row[3][i * 2..i * 2 + 2], 16).unwrap())).collect();
    assert!(depths.contains(&0x3F) && depths.contains(&0x00), "water depth saturates at both ends");
}
