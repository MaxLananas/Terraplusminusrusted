//! Differential tests against goldens produced by the upstream Java elevation code over the
//! offline Terrarium tile (see `tools/oracle/build-oracle.sh`, `fixtures/elevation/golden/PROVENANCE`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;
use terra_elevation::{
    AxisBounds, BlendMode, CachingTileProvider, LayeredScalarDataset, MirrorFetcher, ScalarLayer, TileFormat,
    TiledScalarDataset, decode_terrarium_png, numeric::floor_i, sample_chunk_heights,
};
use terra_projection::{SharedProjection, WebMercatorProjection, bte_world_projection, terraplusminus_projection};

/// Heights derive from projected coordinates that agree with upstream to ~1e-8 blocks; a
/// micrometre of elevation is far below the 1 m vertical resolution of the generated terrain.
const HEIGHT_TOLERANCE_METRES: f64 = 1e-6;
const MIRROR_PREFIX: &str = "mirror://terrarium/";

fn repository_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join(relative)
}

fn golden_lines(name: &str) -> Vec<String> {
    let compressed = std::fs::read(repository_path("fixtures/elevation/golden").join(name)).unwrap();
    let text = String::from_utf8(terra_codec::gunzip(&compressed, 256 << 20).unwrap()).unwrap();
    text.lines().filter(|line| !line.starts_with('#')).map(str::to_owned).collect()
}

fn bits(field: &str) -> f64 {
    f64::from_bits(u64::from_str_radix(field, 16).unwrap())
}

fn same(a: f64, b: f64) -> bool {
    a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
}

fn offline_heights() -> LayeredScalarDataset {
    let projection: SharedProjection = Arc::new(WebMercatorProjection::new(12).unwrap());
    let dataset = TiledScalarDataset::new(
        "offline-terrarium-z12".into(),
        vec![format!("{MIRROR_PREFIX}12/${{x}}/${{z}}.png")],
        projection,
        256,
        BlendMode::Cubic,
        TileFormat::TerrariumPng,
    )
    .unwrap();
    LayeredScalarDataset::new(vec![ScalarLayer {
        dataset: Arc::new(dataset),
        bounds: AxisBounds::of(-180.0, 180.0, -85.0, 85.0),
        priority: 0.0,
        condition: None,
    }])
}

fn offline_provider() -> CachingTileProvider<MirrorFetcher> {
    let fetcher = MirrorFetcher::new().with_mapping(MIRROR_PREFIX, repository_path("fixtures/elevation/terrarium"));
    CachingTileProvider::new(fetcher, 16)
}

#[test]
fn terrarium_decode_is_bit_exact() {
    let png = std::fs::read(repository_path("fixtures/elevation/terrarium/12/759/1608.png")).unwrap();
    let decoded = decode_terrarium_png(&png, 256).unwrap();
    let expected: Vec<f64> = golden_lines("terrarium_decode_z12_759_1608.tsv.gz").iter().map(|line| bits(line)).collect();
    assert_eq!(decoded.len(), expected.len());
    let mismatches = decoded.iter().zip(&expected).filter(|(a, b)| !same(**a, **b)).count();
    assert_eq!(mismatches, 0);
}

#[test]
fn blend_modes_are_bit_exact() {
    let png = std::fs::read(repository_path("fixtures/elevation/terrarium/12/759/1608.png")).unwrap();
    let tile = decode_terrarium_png(&png, 256).unwrap();
    let sampler = |x: i32, z: i32| {
        if x >> 8 == 759 && z >> 8 == 1608 { tile[(((z & 255) << 8) | (x & 255)) as usize] } else { f64::NAN }
    };
    let mut compared = 0;
    let mut mismatches = Vec::new();
    for line in golden_lines("blend_samples.tsv.gz") {
        let fields: Vec<&str> = line.split('\t').collect();
        let mode = BlendMode::from_name(fields[0]).unwrap();
        let (x, z, expected) = (bits(fields[1]), bits(fields[2]), bits(fields[3]));
        let actual = mode.get(x, z, sampler);
        compared += 1;
        if !same(actual, expected) {
            mismatches.push(format!("{mode:?} {x} {z}: java {expected} rust {actual}"));
        }
    }
    eprintln!("blend samples: {compared} compared, {} mismatches", mismatches.len());
    assert!(compared >= 18_000);
    assert!(mismatches.is_empty(), "{:#?}", &mismatches[..mismatches.len().min(20)]);
}

fn compare_chunks(golden: &str, projection: &SharedProjection) {
    let heights = offline_heights();
    let provider = offline_provider();
    let (mut chunks, mut bit_exact, mut max_error, mut floor_mismatches) = (0, 0usize, 0.0f64, 0usize);
    let mut failures = Vec::new();
    for line in golden_lines(golden) {
        let fields: Vec<&str> = line.split('\t').collect();
        let (chunk_x, chunk_z): (i32, i32) = (fields[0].parse().unwrap(), fields[1].parse().unwrap());
        let actual = sample_chunk_heights(projection.as_ref(), &heights, &provider, chunk_x, chunk_z).unwrap();
        chunks += 1;
        if fields[2] == "OUT_OF_BOUNDS" {
            assert!(actual.is_none(), "chunk {chunk_x},{chunk_z} should have no data");
            continue;
        }
        let actual = actual.unwrap_or_else(|| panic!("chunk {chunk_x},{chunk_z} produced no data"));
        assert_eq!(actual.len(), 256);
        for (index, (field, value)) in fields[2..].iter().zip(&actual).enumerate() {
            let expected = bits(field);
            if same(expected, *value) {
                bit_exact += 1;
                continue;
            }
            let error = (expected - value).abs();
            if error.is_nan() || error > HEIGHT_TOLERANCE_METRES {
                failures.push(format!("chunk {chunk_x},{chunk_z} column {index}: java {expected} rust {value}"));
            } else {
                max_error = max_error.max(error);
            }
            if floor_i(expected) != floor_i(*value) {
                floor_mismatches += 1;
            }
        }
    }
    eprintln!(
        "{golden}: {chunks} chunks, {bit_exact} bit-exact columns, max in-tolerance error {max_error:e} m, {floor_mismatches} block-height mismatches"
    );
    assert!(chunks > 0);
    assert!(failures.is_empty(), "{:#?}", &failures[..failures.len().min(20)]);
    assert_eq!(floor_mismatches, 0);
}

#[test]
fn chunk_heights_match_upstream() {
    compare_chunks("chunk_heights_bte.tsv.gz", &bte_world_projection().unwrap());
}

#[test]
fn offset_chunk_heights_match_upstream() {
    compare_chunks("chunk_heights_bte_offset.tsv.gz", &terraplusminus_projection(10_000_000, -3_000_000).unwrap());
}
