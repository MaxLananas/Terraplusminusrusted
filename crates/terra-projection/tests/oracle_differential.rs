//! Differential tests against golden samples produced by the unmodified upstream Java projection
//! code (see `tools/oracle/build-oracle.sh` and `fixtures/projection/PROVENANCE`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;
use terra_projection::{
    AzimuthalEquidistantProjection, CenteredMercatorProjection, ClampTransform, ConformalDymaxionProjection, DymaxionProjection, EqualEarthProjection,
    EquirectangularProjection, GeoCoordinate, GeographicProjection, LambertAzimuthalProjection, ProjectedCoordinate, RotateTransform, SharedProjection,
    SinusoidalProjection, StereographicProjection, TransverseMercatorProjection, WebMercatorProjection, bte_world_projection, terraplusminus_projection,
};

/// Agreement is measured physically rather than per value in ulps: outputs near zero arise from
/// cancellation, so their ulp distance is meaningless while their absolute error stays at the
/// rounding level of the intermediate terms. Forward results are compared by absolute difference in
/// projected units; inverse results by great-circle distance, because longitude is degenerate at
/// the poles.
const INVERSE_TOLERANCE_METERS: f64 = 1e-6;
const EARTH_RADIUS_METERS: f64 = 40_075_017.0 / std::f64::consts::TAU;
/// See `compare`: identical NaN latitudes, longitudes within rounding.
const DEGENERATE_RELATIVE_TOLERANCE: f64 = 1e-12;
/// See `compare`: `asin` at ±1, with 4 ulp of argument (sqrt(8 * 2^-52) rad on the ground).
const POLE_TOLERANCE_METERS: f64 = 0.27;

enum Direction {
    FromGeo,
    ToGeo,
}

struct Sample {
    input: (f64, f64),
    expected: Option<(f64, f64)>,
}

fn load(name: &str) -> Vec<Sample> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/projection").join(name);
    let compressed = std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let text = terra_codec::gunzip(&compressed, 64 << 20).expect("valid gzip fixture");
    let text = String::from_utf8(text).expect("utf-8 fixture");
    text.lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            let value = |index: usize| f64::from_bits(u64::from_str_radix(fields[index], 16).expect("hex bits"));
            let expected = if fields[2] == "OUT_OF_BOUNDS" { None } else { Some((value(2), value(3))) };
            Sample { input: (value(0), value(1)), expected }
        })
        .collect()
}

fn same_value(a: f64, b: f64) -> bool {
    a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
}

fn great_circle_meters(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (lon1, lat1) = (a.0.to_radians(), a.1.to_radians());
    let (lon2, lat2) = (b.0.to_radians(), b.1.to_radians());
    let h = ((lat2 - lat1) / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * ((lon2 - lon1) / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_METERS * h.sqrt().min(1.0).asin()
}

#[derive(Default, Debug)]
struct Report {
    samples: usize,
    bit_exact: usize,
    max_error: f64,
    status_mismatches: Vec<String>,
    value_mismatches: Vec<String>,
}

/// `forward_tolerance` is in projected units and only applies to `Direction::FromGeo`.
fn compare(name: &str, projection: &dyn GeographicProjection, direction: Direction, forward_tolerance: f64) -> Report {
    let mut report = Report::default();
    for sample in load(name) {
        report.samples += 1;
        let (a, b) = sample.input;
        let actual = match direction {
            Direction::FromGeo => projection.from_geo(GeoCoordinate::new(a, b)).ok().map(|p| (p.x, p.y)),
            Direction::ToGeo => projection.to_geo(ProjectedCoordinate::new(a, b)).ok().map(|g| (g.longitude, g.latitude)),
        };
        match (sample.expected, actual) {
            (Some(expected), Some(actual)) => {
                if same_value(expected.0, actual.0) && same_value(expected.1, actual.1) {
                    report.bit_exact += 1;
                    continue;
                }
                let (error, tolerance) = match direction {
                    Direction::FromGeo => {
                        ((expected.0 - actual.0).abs().max((expected.1 - actual.1).abs()), forward_tolerance)
                    }
                    // Both sides degenerate the same way (upstream's formulas give NaN latitude
                    // outside the projection's outline): the finite component is compared
                    // relatively instead of as a distance.
                    Direction::ToGeo if expected.1.is_nan() && actual.1.is_nan() => {
                        ((expected.0 - actual.0).abs() / expected.0.abs().max(1.0), DEGENERATE_RELATIVE_TOLERANCE)
                    }
                    // At a pole `asin` is evaluated at ±1, where one ulp of its argument moves the
                    // result by sqrt(2 ulp) ~ 2e-8 rad (12 cm); longitude means nothing there.
                    Direction::ToGeo if expected.1.abs() == 90.0 || actual.1.abs() == 90.0 => {
                        ((expected.1 - actual.1).abs().to_radians() * EARTH_RADIUS_METERS, POLE_TOLERANCE_METERS)
                    }
                    Direction::ToGeo => (great_circle_meters(expected, actual), INVERSE_TOLERANCE_METERS),
                };
                if error.is_nan() || error > tolerance {
                    report.value_mismatches.push(format!("{a:?},{b:?}: java {expected:?} rust {actual:?} (error {error:e})"));
                } else {
                    report.max_error = report.max_error.max(error);
                }
            }
            (None, None) => report.bit_exact += 1,
            (expected, actual) => {
                report.status_mismatches.push(format!("{a:?},{b:?}: java {expected:?} rust {actual:?}"));
            }
        }
    }
    eprintln!(
        "{name}: {} samples, {} bit-exact, max in-tolerance error {:e}, {} status mismatches, {} value mismatches",
        report.samples,
        report.bit_exact,
        report.max_error,
        report.status_mismatches.len(),
        report.value_mismatches.len()
    );
    report
}

fn assert_matches(report: &Report) {
    assert!(report.samples > 0);
    let shown = |list: &[String]| list[..list.len().min(20)].to_vec();
    assert!(report.status_mismatches.is_empty(), "status mismatches: {:#?}", shown(&report.status_mismatches));
    assert!(report.value_mismatches.is_empty(), "value mismatches: {:#?}", shown(&report.value_mismatches));
}

fn bte() -> SharedProjection {
    bte_world_projection().expect("projection data")
}

/// One BTE block is one metre at the projection origin; 1e-6 is a micrometre.
const BTE_TOLERANCE_BLOCKS: f64 = 1e-6;
/// Raw dymaxion units span the icosahedron edge (about 7.3e6 m per unit): 1e-13 is below a micrometre.
const DYMAXION_TOLERANCE_UNITS: f64 = 1e-13;
/// A zoom-13 pixel is at most 19 m; 1e-8 pixels is below a micrometre.
const WEB_MERCATOR_TOLERANCE_PIXELS: f64 = 1e-8;

#[test]
fn dymaxion_from_geo_matches_upstream() {
    let report = compare("dymaxion_from_geo.tsv.gz", &DymaxionProjection::default(), Direction::FromGeo, DYMAXION_TOLERANCE_UNITS);
    assert_matches(&report);
}

#[test]
fn conformal_dymaxion_from_geo_matches_upstream() {
    let projection = ConformalDymaxionProjection::new().expect("projection data");
    let report = compare("conformal_dymaxion_from_geo.tsv.gz", &projection, Direction::FromGeo, DYMAXION_TOLERANCE_UNITS);
    assert_matches(&report);
}

#[test]
fn bte_from_geo_matches_upstream() {
    assert_matches(&compare("bte_from_geo.tsv.gz", bte().as_ref(), Direction::FromGeo, BTE_TOLERANCE_BLOCKS));
}

#[test]
fn bte_to_geo_matches_upstream() {
    assert_matches(&compare("bte_to_geo.tsv.gz", bte().as_ref(), Direction::ToGeo, 0.0));
}

#[test]
fn offset_bte_matches_upstream() {
    let projection = terraplusminus_projection(1_234_567, -7_654_321).expect("valid offsets");
    assert_matches(&compare("bte_offset_from_geo.tsv.gz", projection.as_ref(), Direction::FromGeo, BTE_TOLERANCE_BLOCKS));
    assert_matches(&compare("bte_offset_to_geo.tsv.gz", projection.as_ref(), Direction::ToGeo, 0.0));
}

#[test]
fn web_mercator_matches_upstream() {
    let projection: SharedProjection = Arc::new(WebMercatorProjection::new(13).expect("valid zoom"));
    let tolerance = WEB_MERCATOR_TOLERANCE_PIXELS;
    assert_matches(&compare("web_mercator_z13_from_geo.tsv.gz", projection.as_ref(), Direction::FromGeo, tolerance));
    assert_matches(&compare("web_mercator_z13_to_geo.tsv.gz", projection.as_ref(), Direction::ToGeo, 0.0));
}

/// Tissot's indicatrix differentiates `fromGeo` over 1e-7 rad (about 0.73 blocks), so the forward
/// agreement (below 1e-8 blocks) bounds the relative error of each derivative near 1e-8.
const TISSOT_RELATIVE_TOLERANCE: f64 = 1e-6;
/// The angular distortion `2 asin(b'/a')` comes from `b' = sqrt(h^2 + k^2 - 2hk sin t)`, which
/// cancels where the projection is nearly conformal, so it is compared as an absolute angle: 1e-6
/// rad is below the 1e-4 degree that /distortion prints.
const TISSOT_ANGLE_TOLERANCE_RADIANS: f64 = 1e-6;

#[test]
fn bte_tissot_matches_upstream() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/commands/golden/tissot_bte.tsv.gz");
    let compressed = std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let text = String::from_utf8(terra_codec::gunzip(&compressed, 16 << 20).expect("valid gzip fixture")).expect("utf-8 fixture");
    let projection = bte();
    let bits = |field: &str| f64::from_bits(u64::from_str_radix(field, 16).expect("hex bits"));
    let (mut compared, mut worst, mut worst_angle) = (0, 0.0_f64, 0.0_f64);
    for line in text.lines().skip(1) {
        let fields: Vec<&str> = line.split('\t').collect();
        let (longitude, latitude) = (bits(fields[0]), bits(fields[1]));
        let actual = projection.tissot(longitude, latitude);
        if fields[2] != "ok" {
            assert!(actual.is_err(), "{line}");
            continue;
        }
        let actual = actual.unwrap_or_else(|error| panic!("{line}: {error:?}"));
        for (index, (component, field)) in actual.iter().zip(&fields[3..7]).enumerate() {
            let expected = bits(field);
            if index == 1 {
                worst_angle = worst_angle.max((component - expected).abs());
                assert!((component - expected).abs() <= TISSOT_ANGLE_TOLERANCE_RADIANS, "{line}: angle {component} vs {expected}");
            } else {
                let error = (component - expected).abs() / expected.abs();
                worst = worst.max(error);
                assert!(error <= TISSOT_RELATIVE_TOLERANCE, "{line}: {component} vs {expected}");
            }
        }
        compared += 1;
    }
    assert_eq!(compared, 400);
    eprintln!("tissot: {compared} points, worst relative error {worst:e}, worst angle error {worst_angle:e} rad");
}

/// The rest of terraminusminus's projection registry, each in both directions, with the
/// parameters `ProjectionGoldenDump.writeBoth` used. Forward results must agree within a
/// micrometre on the ground (`1e-6 / meters_per_unit` projected units), inverse results within a
/// micrometre of great-circle distance, and every out-of-bounds verdict must match.
#[test]
fn every_other_registered_projection_matches_upstream() {
    let projections: Vec<(&str, SharedProjection)> = vec![
        ("centered_mercator", Arc::new(CenteredMercatorProjection)),
        ("transverse_mercator", Arc::new(TransverseMercatorProjection)),
        ("equirectangular", Arc::new(EquirectangularProjection)),
        ("sinusoidal", Arc::new(SinusoidalProjection)),
        ("equal_earth", Arc::new(EqualEarthProjection)),
        ("lambert_azimuthal", Arc::new(LambertAzimuthalProjection::new(Some(12.5), Some(48.0)).unwrap())),
        ("azimuthal_equidistant", Arc::new(AzimuthalEquidistantProjection::new(Some(-61.5), Some(16.25)).unwrap())),
        ("azimuthal_equidistant_north", Arc::new(AzimuthalEquidistantProjection::new(Some(0.0), Some(90.0)).unwrap())),
        ("stereographic", Arc::new(StereographicProjection::new(Some(10.0), Some(20.0), Some(60.0)).unwrap())),
        ("clamp", Arc::new(ClampTransform::new(Arc::new(EquirectangularProjection), -10.0, 20.0, -5.0, 50.0).unwrap())),
        ("rotate", Arc::new(RotateTransform::new(Arc::new(EqualEarthProjection), 33.0).unwrap())),
    ];
    let mut failures = Vec::new();
    for (name, projection) in &projections {
        let tolerance = 1e-6 / projection.meters_per_unit();
        for (file, direction) in [(format!("{name}_from_geo.tsv.gz"), Direction::FromGeo), (format!("{name}_to_geo.tsv.gz"), Direction::ToGeo)] {
            let report = compare(&file, projection.as_ref(), direction, tolerance);
            if report.samples == 0 || !report.status_mismatches.is_empty() || !report.value_mismatches.is_empty() {
                let shown: Vec<_> = report.status_mismatches.iter().chain(&report.value_mismatches).take(5).cloned().collect();
                failures.push(format!("{file}: {} status, {} value mismatches: {shown:#?}", report.status_mismatches.len(), report.value_mismatches.len()));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
