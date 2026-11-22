//! Every projection on extreme and non-finite input (what a hostile tpll argument, a corrupt
//! GeoJSON coordinate or a far-out chunk can produce): no panic, and a successful result never
//! carries a NaN or infinity into the generator.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use terra_projection::{
    BteDymaxionProjection, ConformalDymaxionProjection, DymaxionProjection, GeoCoordinate, ProjectedCoordinate, SharedProjection, WebMercatorProjection,
    bte_world_projection, terraplusminus_projection,
};

fn projections() -> Vec<(&'static str, SharedProjection)> {
    vec![
        ("dymaxion", Arc::new(DymaxionProjection::default())),
        ("conformal", Arc::new(ConformalDymaxionProjection::new().unwrap())),
        ("bte dymaxion", Arc::new(BteDymaxionProjection::new().unwrap())),
        ("bte world", bte_world_projection().unwrap()),
        ("terraplusminus", terraplusminus_projection(i32::MAX, i32::MIN).unwrap()),
        ("web mercator z0", Arc::new(WebMercatorProjection::new(0).unwrap())),
        ("web mercator z12", Arc::new(WebMercatorProjection::new(12).unwrap())),
    ]
}

fn values() -> Vec<f64> {
    let mut values = vec![
        0.0,
        -0.0,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::MAX,
        f64::MIN,
        f64::MIN_POSITIVE,
        f64::EPSILON,
        5e-324,
        1e300,
        -1e300,
        180.0,
        -180.0,
        180.000_000_000_1,
        90.0,
        -90.0,
        90.000_000_000_1,
        85.051_128_779_806_59,
        360.0,
        720.5,
        -1e9,
        1e9,
        2.147_483_647e9,
        3.0e7,
        -3.0e7,
        1e15,
    ];
    let mut state = 0x2545_f491_4f6c_dd1du64;
    for _ in 0..400 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        values.push(f64::from_bits(state));
        values.push((state % 4_000_000) as f64 / 10_000.0 - 200.0);
    }
    values
}

fn finite(values: &[f64]) -> bool {
    values.iter().all(|value| value.is_finite())
}

/// Block coordinates are Java `int`s: projected inputs beyond them never come from the server.
const BLOCK_RANGE: f64 = 2_147_483_648.0;

/// Upstream propagates NaN (its bounds checks are `Math.abs(x) > limit`, false for NaN) and very
/// large projected values overflow inside the Dymaxion maths; the port matches it bit for bit, so
/// those outputs are not asserted. What is asserted: no input panics, and every input the server
/// can actually produce (finite geographic coordinates, projected coordinates within the block
/// range) yields a finite result or an error.
#[test]
fn projections_survive_extreme_and_non_finite_input() {
    let values = values();
    let mut failures = Vec::new();
    for (name, projection) in projections() {
        let mut cases = 0usize;
        for (i, &a) in values.iter().enumerate() {
            // Pair each value with a sample of the others (all pairs would be 800^2 per call).
            for &b in values.iter().skip(i % 7).step_by(7) {
                cases += 1;
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    let mut problems = Vec::new();
                    let finite_input = a.is_finite() && b.is_finite();
                    if let Ok(projected) = projection.from_geo(GeoCoordinate::new(a, b))
                        && finite_input
                        && !finite(&[projected.x, projected.y])
                    {
                        problems.push(format!("from_geo({a:e}, {b:e}) = {projected:?}"));
                    }
                    if let Ok(geo) = projection.to_geo(ProjectedCoordinate::new(a, b))
                        && a.abs() <= BLOCK_RANGE
                        && b.abs() <= BLOCK_RANGE
                        && !finite(&[geo.longitude, geo.latitude])
                    {
                        problems.push(format!("to_geo({a:e}, {b:e}) = {geo:?}"));
                    }
                    let _ = projection.tissot(a, b);
                    let _ = projection.vector(ProjectedCoordinate::new(a, b), 1.0, 0.0);
                    let _ = projection.azimuth(ProjectedCoordinate::new(a, b), 45.0);
                    problems
                }));
                match outcome {
                    Ok(problems) => failures.extend(problems.into_iter().map(|p| format!("{name}: {p}"))),
                    Err(payload) => {
                        let message = payload.downcast_ref::<String>().cloned().or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned())).unwrap_or_default();
                        failures.push(format!("{name}: panic at ({a:e}, {b:e}): {message}"));
                    }
                }
            }
        }
        assert!(cases > 10_000, "{name}: {cases}");
    }
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.iter().take(12).cloned().collect::<Vec<_>>().join("\n"));
}
