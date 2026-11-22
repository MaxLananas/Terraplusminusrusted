//! The rest of terraminusminus's projection registry (`GlobalParseRegistries.PROJECTIONS`):
//! `centered_mercator`, `transverse_mercator`, `equirectangular`, `sinusoidal`, `equal_earth`,
//! `lambert_azimuthal`, `azimuthal_equidistant`, `stereographic`, and the `clamp` and `rotate`
//! transforms. Each is a line-by-line port of the Java class of the same name; the formulas,
//! bounds checks and constants are upstream's, including their quirks (noted where they matter).

use crate::java_math::{acos, asin, atan, atan2, cos, cosh, exp, ln, sin, sinh, tan, to_degrees, to_radians};
use crate::mercator::WebMercatorProjection;
use crate::{
    EARTH_CIRCUMFERENCE, GeoCoordinate, GeographicProjection, OutOfProjectionBounds, ProjectedBounds, ProjectedCoordinate, ProjectionResult, SharedProjection,
    check_in_range, check_longitude_latitude,
};
use std::f64::consts::PI;

/// `MathUtils.ROOT3`.
const ROOT3: f64 = 1.732_050_807_568_877_2;

/// The `modDegrees` helper the azimuthal projections share: wraps into `[-bound/2, bound/2)`,
/// keeping exactly `bound/2`. Java's `%` on doubles is `fmod`, as Rust's.
fn mod_degrees(value: f64, bound: f64) -> f64 {
    if value == bound * 0.5 {
        return value;
    }
    let mut value = value + bound * 0.5;
    if value < 0.0 {
        value = value % bound + bound;
    }
    value % bound - bound * 0.5
}

fn square_bounds(r: f64) -> ProjectedBounds {
    ProjectedBounds { min_x: -r, min_y: -r, max_x: r, max_y: r }
}

/// `CenteredMercatorProjection`: Mercator scaled to `[-1, 1]`, north up.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CenteredMercatorProjection;

impl GeographicProjection for CenteredMercatorProjection {
    fn to_geo(&self, point: ProjectedCoordinate) -> ProjectionResult<GeoCoordinate> {
        check_in_range(point.x, point.y, 1.0, 1.0)?;
        Ok(GeoCoordinate::new(point.x * 180.0, to_degrees(atan(exp(-point.y * PI)) * 2.0 - PI / 2.0)))
    }

    fn from_geo(&self, point: GeoCoordinate) -> ProjectionResult<ProjectedCoordinate> {
        check_in_range(point.longitude, point.latitude, 180.0, WebMercatorProjection::limit_latitude())?;
        Ok(ProjectedCoordinate::new(point.longitude / 180.0, -ln(tan((PI / 2.0 + to_radians(point.latitude)) / 2.0)) / PI))
    }

    fn meters_per_unit(&self) -> f64 {
        cos(to_radians(30.0)) * EARTH_CIRCUMFERENCE / 2.0
    }

    fn bounds(&self) -> ProjectedBounds {
        square_bounds(1.0)
    }

    fn upright(&self) -> bool {
        true
    }
}

/// `TransverseMercatorProjection`: spherical transverse Mercator in 6° zones, each zone's central
/// meridian added back to x (so zones sit side by side, as upstream).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TransverseMercatorProjection;

impl TransverseMercatorProjection {
    /// `ZONE_WIDTH`, in radians.
    pub fn zone_width() -> f64 {
        to_radians(6.0)
    }

    /// `getCentralMeridian`, radians in and out.
    pub fn central_meridian(longitude: f64) -> f64 {
        ((longitude / Self::zone_width()).floor() + 0.5) * Self::zone_width()
    }
}

impl GeographicProjection for TransverseMercatorProjection {
    fn to_geo(&self, point: ProjectedCoordinate) -> ProjectionResult<GeoCoordinate> {
        check_in_range(point.x, point.y, PI, PI / 2.0)?;
        let central = Self::central_meridian(point.x);
        let x = point.x - central;
        let y = point.y;
        let lambda = atan2(sinh(x), cos(y)) + central;
        let phi = asin(sin(y) / cosh(x));
        Ok(GeoCoordinate::new(to_degrees(lambda), to_degrees(phi)))
    }

    fn from_geo(&self, point: GeoCoordinate) -> ProjectionResult<ProjectedCoordinate> {
        check_longitude_latitude(point.longitude, point.latitude)?;
        let mut lambda = to_radians(point.longitude);
        let phi = to_radians(point.latitude);
        let central = Self::central_meridian(lambda);
        lambda -= central;
        let b = cos(phi) * sin(lambda);
        let x = ln((1.0 + b) / (1.0 - b)) / 2.0 + central;
        let y = atan2(tan(phi), cos(lambda));
        Ok(ProjectedCoordinate::new(x, y))
    }

    fn meters_per_unit(&self) -> f64 {
        EARTH_CIRCUMFERENCE / (2.0 * PI)
    }
}

/// `EquirectangularProjection`: longitude and latitude as they are.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EquirectangularProjection;

impl GeographicProjection for EquirectangularProjection {
    fn to_geo(&self, point: ProjectedCoordinate) -> ProjectionResult<GeoCoordinate> {
        check_longitude_latitude(point.x, point.y)?;
        Ok(GeoCoordinate::new(point.x, point.y))
    }

    fn from_geo(&self, point: GeoCoordinate) -> ProjectionResult<ProjectedCoordinate> {
        check_longitude_latitude(point.longitude, point.latitude)?;
        Ok(ProjectedCoordinate::new(point.longitude, point.latitude))
    }

    fn meters_per_unit(&self) -> f64 {
        100_000.0
    }
}

/// `SinusoidalProjection`. Its `toGeo` has no bounds check upstream, so x past the outline maps
/// past ±180°; kept, as the elevation sampler only asks for points it projected itself.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SinusoidalProjection;

impl GeographicProjection for SinusoidalProjection {
    fn to_geo(&self, point: ProjectedCoordinate) -> ProjectionResult<GeoCoordinate> {
        Ok(GeoCoordinate::new(point.x / cos(to_radians(point.y)), point.y))
    }

    fn from_geo(&self, point: GeoCoordinate) -> ProjectionResult<ProjectedCoordinate> {
        check_longitude_latitude(point.longitude, point.latitude)?;
        Ok(ProjectedCoordinate::new(point.longitude * cos(to_radians(point.latitude)), point.latitude))
    }

    fn meters_per_unit(&self) -> f64 {
        EARTH_CIRCUMFERENCE / 360.0
    }
}

/// `EqualEarthProjection` (Šavrič, Patterson and Jenny 2018), the inverse by five Newton steps.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EqualEarthProjection;

impl EqualEarthProjection {
    const A1: f64 = 1.340264;
    const A2: f64 = -0.081106;
    const A3: f64 = 0.000893;
    const A4: f64 = 0.003796;
}

impl GeographicProjection for EqualEarthProjection {
    fn to_geo(&self, point: ProjectedCoordinate) -> ProjectionResult<GeoCoordinate> {
        let (x, y) = (point.x, point.y);
        let mut theta = y / Self::A1;
        for _ in 0..5 {
            // The same evaluation order as upstream's running power `tpow`.
            let mut tpow = theta;
            let mut pdy = Self::A1;
            let mut py = Self::A1 * tpow - y;
            tpow *= theta;
            pdy += 3.0 * Self::A2 * tpow;
            tpow *= theta;
            py += Self::A2 * tpow;
            tpow *= theta * theta * theta;
            pdy += 7.0 * Self::A3 * tpow;
            tpow *= theta;
            py += Self::A3 * tpow;
            tpow *= theta;
            pdy += 9.0 * Self::A4 * tpow;
            tpow *= theta;
            py += Self::A4 * tpow;
            theta -= py / pdy;
        }
        let theta_square = theta * theta;
        let mut tpow = theta_square;
        let mut dx = Self::A1;
        dx += 3.0 * Self::A2 * tpow;
        tpow *= theta_square * theta_square;
        dx += 7.0 * Self::A3 * tpow;
        tpow *= theta_square;
        dx += 9.0 * Self::A4 * tpow;
        Ok(GeoCoordinate::new(to_degrees(x * dx * 3.0 / (2.0 * ROOT3 * cos(theta))), to_degrees(asin(sin(theta) * 2.0 / ROOT3))))
    }

    fn from_geo(&self, point: GeoCoordinate) -> ProjectionResult<ProjectedCoordinate> {
        check_longitude_latitude(point.longitude, point.latitude)?;
        let sin_theta = ROOT3 * sin(to_radians(point.latitude)) / 2.0;
        let theta = asin(sin_theta);
        let mut tpow = theta;
        let mut x = Self::A1;
        let mut y = Self::A1 * tpow;
        tpow *= theta;
        x += 3.0 * Self::A2 * tpow;
        tpow *= theta;
        y += Self::A2 * tpow;
        tpow *= theta * theta * theta;
        x += 7.0 * Self::A3 * tpow;
        tpow *= theta;
        y += Self::A3 * tpow;
        tpow *= theta;
        x += 9.0 * Self::A4 * tpow;
        tpow *= theta;
        y += Self::A4 * tpow;
        let cos_theta = (1.0 - sin_theta * sin_theta).sqrt();
        Ok(ProjectedCoordinate::new((2.0 * ROOT3 * to_radians(point.longitude) * cos_theta / 3.0) / x, y))
    }

    fn meters_per_unit(&self) -> f64 {
        EARTH_CIRCUMFERENCE / (2.0 * self.bounds().max_x)
    }
}

/// The `centerX`/`centerY` pair the azimuthal projections take, normalised as upstream.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Center {
    x: f64,
    y: f64,
}

impl Center {
    fn new(center_x: Option<f64>, center_y: Option<f64>) -> Result<Self, &'static str> {
        let (x, y) = (center_x.unwrap_or(0.0), center_y.unwrap_or(0.0));
        if !x.is_finite() || !y.is_finite() {
            return Err("projection centre must be finite");
        }
        Ok(Self { x: mod_degrees(x, 360.0), y: mod_degrees(y, 180.0) })
    }
}

/// `LambertAzimuthalProjection` (equal-area), centred on `centerX`, `centerY`. Longitude is
/// mirrored (`-toRadians(longitude)`) as upstream.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LambertAzimuthalProjection {
    center: Center,
}

impl LambertAzimuthalProjection {
    pub fn new(center_x: Option<f64>, center_y: Option<f64>) -> Result<Self, &'static str> {
        Ok(Self { center: Center::new(center_x, center_y)? })
    }

    pub fn center(&self) -> (f64, f64) {
        (self.center.x, self.center.y)
    }
}

impl GeographicProjection for LambertAzimuthalProjection {
    fn to_geo(&self, point: ProjectedCoordinate) -> ProjectionResult<GeoCoordinate> {
        let (x, y) = (point.x, point.y);
        let phi1 = to_radians(self.center.y);
        let lambda0 = to_radians(self.center.x);
        let p = (x * x + y * y).sqrt();
        let c = 2.0 * asin(0.5 * p);
        if c.is_nan() {
            return Err(OutOfProjectionBounds);
        }
        let phi = asin(cos(c) * sin(phi1) + (y * sin(c) * cos(phi1)) / p);
        let lambda = lambda0 + atan2(x * sin(c), p * cos(phi1) * cos(c) - y * sin(phi1) * sin(c));
        Ok(GeoCoordinate::new(-mod_degrees(to_degrees(lambda), 360.0), to_degrees(phi)))
    }

    fn from_geo(&self, point: GeoCoordinate) -> ProjectionResult<ProjectedCoordinate> {
        check_longitude_latitude(point.longitude, point.latitude)?;
        let phi1 = to_radians(self.center.y);
        let lambda0 = to_radians(self.center.x);
        let phi = to_radians(point.latitude);
        let lambda = -to_radians(point.longitude);
        let k = (2.0 / (1.0 + sin(phi1) * sin(phi) + cos(phi1) * cos(phi) * cos(lambda - lambda0))).sqrt();
        Ok(ProjectedCoordinate::new(k * cos(phi) * sin(lambda - lambda0), k * (cos(phi1) * sin(phi) - sin(phi1) * cos(phi) * cos(lambda - lambda0))))
    }

    fn meters_per_unit(&self) -> f64 {
        EARTH_CIRCUMFERENCE / (2.0 * self.bounds().max_x)
    }

    fn bounds(&self) -> ProjectedBounds {
        square_bounds(2.0)
    }
}

/// `AzimuthalEquidistantProjection`, centred on `centerX`, `centerY`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AzimuthalEquidistantProjection {
    center: Center,
}

impl AzimuthalEquidistantProjection {
    pub fn new(center_x: Option<f64>, center_y: Option<f64>) -> Result<Self, &'static str> {
        Ok(Self { center: Center::new(center_x, center_y)? })
    }

    pub fn center(&self) -> (f64, f64) {
        (self.center.x, self.center.y)
    }
}

impl GeographicProjection for AzimuthalEquidistantProjection {
    fn to_geo(&self, point: ProjectedCoordinate) -> ProjectionResult<GeoCoordinate> {
        let (x, y) = (point.x, point.y);
        let phi1 = to_radians(self.center.y);
        let lambda0 = to_radians(self.center.x);
        let c = (x * x + y * y).sqrt();
        if c >= PI {
            return Err(OutOfProjectionBounds);
        }
        let phi = asin(cos(c) * sin(phi1) + (y * sin(c) * cos(phi1) / c));
        let lambda = if self.center.y == -90.0 {
            lambda0 + atan2(x, y)
        } else if self.center.y == 90.0 {
            lambda0 + PI + atan2(-x, y)
        } else {
            lambda0 + atan2(x * sin(c), c * cos(phi1) * cos(c) - y * sin(phi1) * sin(c))
        };
        Ok(GeoCoordinate::new(-mod_degrees(to_degrees(lambda), 360.0), to_degrees(phi)))
    }

    fn from_geo(&self, point: GeoCoordinate) -> ProjectionResult<ProjectedCoordinate> {
        check_longitude_latitude(point.longitude, point.latitude)?;
        let phi1 = to_radians(self.center.y);
        let lambda0 = to_radians(self.center.x);
        let phi = to_radians(point.latitude);
        let lambda = -to_radians(point.longitude);
        let p = acos(sin(phi1) * sin(phi) + cos(phi1) * cos(phi) * cos(lambda - lambda0));
        let t = atan2(cos(phi) * sin(lambda - lambda0), cos(phi1) * sin(phi) - sin(phi1) * cos(phi) * cos(lambda - lambda0));
        Ok(ProjectedCoordinate::new(p * sin(t), p * cos(t)))
    }

    fn meters_per_unit(&self) -> f64 {
        EARTH_CIRCUMFERENCE / (2.0 * self.bounds().max_x)
    }

    fn bounds(&self) -> ProjectedBounds {
        square_bounds(PI)
    }
}

/// `StereographicProjection`, centred on `centerX`, `centerY`, its bounds set by `radius`
/// (degrees, exclusive 0..180, default 90). Upstream's `fromGeo` shifts by the centre directly
/// rather than through the oblique formula its `toGeo` uses; ported as written.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StereographicProjection {
    center: Center,
    radius: f64,
}

impl StereographicProjection {
    pub fn new(center_x: Option<f64>, center_y: Option<f64>, radius: Option<f64>) -> Result<Self, &'static str> {
        let radius = radius.unwrap_or(90.0);
        if radius.is_nan() || radius <= 0.0 {
            return Err("radius must be positive!");
        }
        if radius >= 180.0 {
            return Err("radius must be less than 180!");
        }
        Ok(Self { center: Center::new(center_x, center_y)?, radius })
    }

    pub fn center(&self) -> (f64, f64) {
        (self.center.x, self.center.y)
    }

    pub fn radius(&self) -> f64 {
        self.radius
    }
}

impl GeographicProjection for StereographicProjection {
    fn to_geo(&self, point: ProjectedCoordinate) -> ProjectionResult<GeoCoordinate> {
        let (x, y) = (point.x, point.y);
        let phi1 = to_radians(self.center.y);
        let lambda0 = to_radians(self.center.x);
        let p = (x * x + y * y).sqrt();
        let c = 2.0 * atan2(p, 2.0);
        if c.is_nan() {
            return Err(OutOfProjectionBounds);
        }
        let phi = asin(cos(c) * sin(phi1) + (y * sin(c) * cos(phi1)) / p);
        let lambda = lambda0 + atan2(x * sin(c), p * cos(phi1) * cos(c) - y * sin(phi1) * sin(c));
        Ok(GeoCoordinate::new(-mod_degrees(to_degrees(lambda), 360.0), to_degrees(phi)))
    }

    fn from_geo(&self, point: GeoCoordinate) -> ProjectionResult<ProjectedCoordinate> {
        check_longitude_latitude(point.longitude, point.latitude)?;
        let phi = to_radians(point.latitude - self.center.y);
        let lambda = -to_radians(point.longitude + self.center.x);
        let k = 2.0 / (1.0 + cos(phi) * cos(lambda));
        Ok(ProjectedCoordinate::new(k * cos(phi) * sin(lambda), k * sin(phi)))
    }

    fn meters_per_unit(&self) -> f64 {
        EARTH_CIRCUMFERENCE / (2.0 * self.bounds().max_x)
    }

    fn bounds(&self) -> ProjectedBounds {
        let lambda = to_radians(self.radius);
        let k = 2.0 / (1.0 + cos(lambda));
        square_bounds((k * sin(lambda)).abs())
    }
}

/// `ClampProjectionTransform`: the delegate restricted to a rectangle (inclusive).
#[derive(Debug, Clone)]
pub struct ClampTransform {
    delegate: SharedProjection,
    bounds: ProjectedBounds,
}

impl ClampTransform {
    pub fn new(delegate: SharedProjection, min_x: f64, max_x: f64, min_y: f64, max_y: f64) -> Result<Self, &'static str> {
        if ![min_x, max_x, min_y, max_y].iter().all(|value| value.is_finite()) {
            return Err("Projection bounds must be finite doubles");
        }
        Ok(Self { delegate, bounds: ProjectedBounds { min_x, min_y, max_x, max_y } })
    }

    fn outside(&self, x: f64, y: f64) -> bool {
        // Written as upstream's comparisons so NaN passes through to the delegate the same way.
        x < self.bounds.min_x || x > self.bounds.max_x || y < self.bounds.min_y || y > self.bounds.max_y
    }
}

impl GeographicProjection for ClampTransform {
    fn to_geo(&self, point: ProjectedCoordinate) -> ProjectionResult<GeoCoordinate> {
        if self.outside(point.x, point.y) {
            return Err(OutOfProjectionBounds);
        }
        self.delegate.to_geo(point)
    }

    fn from_geo(&self, point: GeoCoordinate) -> ProjectionResult<ProjectedCoordinate> {
        let projected = self.delegate.from_geo(point)?;
        if self.outside(projected.x, projected.y) {
            return Err(OutOfProjectionBounds);
        }
        Ok(projected)
    }

    fn meters_per_unit(&self) -> f64 {
        self.delegate.meters_per_unit()
    }

    fn bounds(&self) -> ProjectedBounds {
        self.bounds
    }

    fn upright(&self) -> bool {
        self.delegate.upright()
    }
}

/// `RotateProjectionTransform`: the projected plane rotated by `by` degrees.
#[derive(Debug, Clone)]
pub struct RotateTransform {
    delegate: SharedProjection,
    by: f64,
    sin: f64,
    cos: f64,
    sin_backwards: f64,
    cos_backwards: f64,
}

impl RotateTransform {
    pub fn new(delegate: SharedProjection, by: f64) -> Result<Self, &'static str> {
        if !by.is_finite() {
            return Err("Projection rotation must be a finite double");
        }
        Ok(Self { delegate, by, sin: sin(to_radians(by)), cos: cos(to_radians(by)), sin_backwards: sin(to_radians(-by)), cos_backwards: cos(to_radians(-by)) })
    }

    pub fn by(&self) -> f64 {
        self.by
    }
}

impl GeographicProjection for RotateTransform {
    fn to_geo(&self, point: ProjectedCoordinate) -> ProjectionResult<GeoCoordinate> {
        self.delegate.to_geo(ProjectedCoordinate::new(point.x * self.cos - point.y * self.sin, point.x * self.sin + point.y * self.cos))
    }

    fn from_geo(&self, point: GeoCoordinate) -> ProjectionResult<ProjectedCoordinate> {
        let p = self.delegate.from_geo(point)?;
        Ok(ProjectedCoordinate::new(p.x * self.cos_backwards - p.y * self.sin_backwards, p.x * self.sin_backwards + p.y * self.cos_backwards))
    }

    fn meters_per_unit(&self) -> f64 {
        self.delegate.meters_per_unit()
    }

    /// Upstream rotates the corners forwards (`cos`, `sin`) here although `fromGeo` rotates
    /// backwards; ported as written.
    fn bounds(&self) -> ProjectedBounds {
        let b = self.delegate.bounds();
        let xs = [b.min_x * self.cos - b.min_y * self.sin, b.min_x * self.cos - b.max_y * self.sin, b.max_x * self.cos - b.min_y * self.sin, b.max_x * self.cos - b.max_y * self.sin];
        let ys = [b.min_x * self.sin + b.min_y * self.cos, b.min_x * self.sin + b.max_y * self.cos, b.max_x * self.sin + b.min_y * self.cos, b.max_x * self.sin + b.max_y * self.cos];
        let min = |v: [f64; 4]| v[0].min(v[1]).min(v[2].min(v[3]));
        let max = |v: [f64; 4]| v[0].max(v[1]).max(v[2].max(v[3]));
        ProjectedBounds { min_x: min(xs), min_y: min(ys), max_x: max(xs), max_y: max(ys) }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use std::sync::Arc;

    fn round_trip(projection: &dyn GeographicProjection, points: &[(f64, f64)], tolerance: f64) {
        for &(longitude, latitude) in points {
            let projected = projection.from_geo(GeoCoordinate::new(longitude, latitude)).unwrap_or_else(|_| panic!("{projection:?} from_geo({longitude}, {latitude})"));
            let back = projection.to_geo(projected).unwrap_or_else(|_| panic!("{projection:?} to_geo({projected:?})"));
            assert!((back.longitude - longitude).abs() < tolerance && (back.latitude - latitude).abs() < tolerance, "{projection:?}: ({longitude}, {latitude}) -> {projected:?} -> {back:?}");
        }
    }

    const POINTS: [(f64, f64); 6] = [(0.0, 0.0), (8.5, 47.3), (-61.5, 16.25), (139.7, 35.7), (-122.4, -37.8), (2.35, 48.85)];

    #[test]
    fn every_projection_round_trips() {
        round_trip(&CenteredMercatorProjection, &POINTS, 1e-9);
        // Upstream picks the zone from the projected x, so a point on a zone edge (longitude 0
        // here) comes back through the neighbouring zone; the edges are left out.
        round_trip(&TransverseMercatorProjection, &POINTS[1..], 1e-9);
        let edge = TransverseMercatorProjection.from_geo(GeoCoordinate::new(0.0, 0.0)).unwrap();
        assert!(edge.x < 0.0, "upstream's zone-edge behaviour: {edge:?}");
        round_trip(&EquirectangularProjection, &POINTS, 0.0 + f64::EPSILON);
        round_trip(&SinusoidalProjection, &POINTS, 1e-9);
        round_trip(&EqualEarthProjection, &POINTS, 1e-9);
        round_trip(&AzimuthalEquidistantProjection::new(Some(10.0), Some(40.0)).unwrap(), &POINTS, 1e-9);
        // Mirrored longitudes (upstream's sign convention) still round-trip.
        round_trip(&LambertAzimuthalProjection::new(None, None).unwrap(), &POINTS[1..], 1e-9);
        round_trip(&StereographicProjection::new(None, None, None).unwrap(), &POINTS[1..3], 1e-9);
        // The exact centre divides 0 by 0 in upstream's inverse formulas too.
        let lambert = LambertAzimuthalProjection::new(None, None).unwrap();
        assert!(lambert.to_geo(ProjectedCoordinate::new(0.0, 0.0)).unwrap().latitude.is_nan());
        let rotated = RotateTransform::new(Arc::new(EquirectangularProjection), 30.0).unwrap();
        round_trip(&rotated, &POINTS, 1e-9);
    }

    #[test]
    fn upstream_values_and_bounds() {
        // EqualEarth at (180, 0): x = 2 * sqrt(3) * pi / 3 / A1.
        let edge = EqualEarthProjection.from_geo(GeoCoordinate::new(180.0, 0.0)).unwrap();
        assert!((edge.x - 2.0 * ROOT3 * PI / 3.0 / EqualEarthProjection::A1).abs() < 1e-12);
        assert!((EqualEarthProjection.bounds().max_x - edge.x).abs() < 1e-12);
        // Transverse Mercator: the zone's central meridian maps to its own x, the equator to 0.
        let zone = TransverseMercatorProjection.from_geo(GeoCoordinate::new(3.0, 0.0)).unwrap();
        assert!((zone.x - to_radians(3.0)).abs() < 1e-12 && zone.y.abs() < 1e-12);
        let origin = CenteredMercatorProjection.from_geo(GeoCoordinate::new(0.0, 0.0)).unwrap();
        assert!(origin.x == 0.0 && origin.y.abs() < 1e-15, "{origin:?}");
        assert!(CenteredMercatorProjection.from_geo(GeoCoordinate::new(0.0, 89.0)).is_err());
        assert!(EquirectangularProjection.to_geo(ProjectedCoordinate::new(181.0, 0.0)).is_err());
        assert!(LambertAzimuthalProjection::new(None, None).unwrap().to_geo(ProjectedCoordinate::new(2.5, 0.0)).is_err());
        assert!(AzimuthalEquidistantProjection::new(None, None).unwrap().to_geo(ProjectedCoordinate::new(PI, 0.0)).is_err());
        assert!((StereographicProjection::new(None, None, None).unwrap().bounds().max_x - 2.0).abs() < 1e-12);
        assert!(StereographicProjection::new(None, None, Some(180.0)).is_err());
        assert!(StereographicProjection::new(None, None, Some(0.0)).is_err());
        assert_eq!(mod_degrees(190.0, 360.0), -170.0);
        assert_eq!(mod_degrees(180.0, 360.0), 180.0);
        assert_eq!(mod_degrees(-200.0, 360.0), 160.0);
        assert_eq!(LambertAzimuthalProjection::new(Some(370.0), Some(100.0)).unwrap().center(), (10.0, -80.0));
    }

    #[test]
    fn clamp_and_rotate() {
        let clamp = ClampTransform::new(Arc::new(EquirectangularProjection), -10.0, 10.0, -5.0, 5.0).unwrap();
        assert!(clamp.from_geo(GeoCoordinate::new(11.0, 0.0)).is_err());
        assert!(clamp.to_geo(ProjectedCoordinate::new(0.0, 6.0)).is_err());
        assert_eq!(clamp.from_geo(GeoCoordinate::new(10.0, 5.0)).unwrap(), ProjectedCoordinate::new(10.0, 5.0));
        assert!(ClampTransform::new(Arc::new(EquirectangularProjection), f64::NAN, 1.0, 0.0, 1.0).is_err());
        let rotate = RotateTransform::new(Arc::new(EquirectangularProjection), 90.0).unwrap();
        let p = rotate.from_geo(GeoCoordinate::new(10.0, 0.0)).unwrap();
        assert!(p.x.abs() < 1e-12 && (p.y + 10.0).abs() < 1e-12, "{p:?}");
        assert!(RotateTransform::new(Arc::new(EquirectangularProjection), f64::INFINITY).is_err());
    }
}
