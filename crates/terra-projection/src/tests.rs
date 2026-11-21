use super::*;

fn bte() -> SharedProjection {
    bte_world_projection().expect("built-in projection data")
}

#[test]
fn round_trip_known_landmarks() {
    let projection = bte();
    for (longitude, latitude) in [(13.4050, 52.5200), (-74.0445, 40.6892), (151.2153, -33.8568), (-61.53, 16.24), (0.0, 0.0)] {
        let projected = projection.from_geo(GeoCoordinate::new(longitude, latitude)).expect("in bounds");
        let geo = projection.to_geo(projected).expect("in bounds");
        assert!((geo.longitude - longitude).abs() < 1e-7, "{longitude} -> {}", geo.longitude);
        assert!((geo.latitude - latitude).abs() < 1e-7, "{latitude} -> {}", geo.latitude);
    }
}

#[test]
fn rejects_out_of_range_geographic_input() {
    let projection = bte();
    assert_eq!(projection.from_geo(GeoCoordinate::new(181.0, 0.0)), Err(OutOfProjectionBounds));
    assert_eq!(projection.from_geo(GeoCoordinate::new(0.0, -90.5)), Err(OutOfProjectionBounds));
    assert!(projection.to_geo(ProjectedCoordinate::new(1e9, 1e9)).is_err());
}

#[test]
fn offset_shifts_projected_coordinates_exactly() {
    let base = bte();
    let shifted = terraplusminus_projection(1000, -2000).expect("valid offsets");
    let geo = GeoCoordinate::new(2.35, 48.85);
    let a = base.from_geo(geo).expect("in bounds");
    let b = shifted.from_geo(geo).expect("in bounds");
    assert_eq!(b.x, a.x + 1000.0);
    assert_eq!(b.y, a.y - 2000.0);
}

#[test]
fn web_mercator_tile_corners() {
    let projection = WebMercatorProjection::new(0).expect("valid zoom");
    let origin = projection.from_geo(GeoCoordinate::new(-180.0, 0.0)).expect("in bounds");
    assert_eq!(origin.x, 0.0);
    assert!((origin.y - 128.0).abs() < 1e-9);
    assert!(projection.from_geo(GeoCoordinate::new(0.0, 86.0)).is_err());
    assert!(WebMercatorProjection::new(23).is_err());
}

#[test]
fn projection_is_safe_to_share_between_threads() {
    let projection = bte();
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let projection = projection.clone();
            std::thread::spawn(move || projection.from_geo(GeoCoordinate::new(10.0 + f64::from(i), 50.0)))
        })
        .collect();
    for handle in handles {
        assert!(handle.join().expect("thread completes").is_ok());
    }
}
