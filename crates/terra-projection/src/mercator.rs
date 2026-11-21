//! `WebMercatorProjection`: pixel coordinates of a 256-pixel tile pyramid at a fixed zoom.

use crate::java_math::{atan, exp, ln, pow, tan, to_degrees, to_radians};
use crate::{
    GeoCoordinate, GeographicProjection, OutOfProjectionBounds, ProjectedBounds, ProjectedCoordinate,
    ProjectionResult, check_in_range,
};
use std::f64::consts::{E, PI, TAU};

#[derive(Debug, Clone, PartialEq)]
pub struct WebMercatorProjection {
    zoom: u32,
    scale_to: f64,
    scale_from: f64,
}

impl WebMercatorProjection {
    /// Zoom levels above 22 overflow the Java `int` expression `256 << zoom`; upstream silently
    /// wraps, which is rejected here instead.
    pub fn new(zoom: u32) -> Result<Self, &'static str> {
        if zoom > 22 {
            return Err("web_mercator zoom must be between 0 and 22");
        }
        let pixels = f64::from(256u32 << zoom);
        Ok(Self { zoom, scale_to: 1.0 / pixels, scale_from: pixels })
    }

    pub fn zoom(&self) -> u32 {
        self.zoom
    }

    /// `WebMercatorProjection.LIMIT_LATITUDE`.
    pub fn limit_latitude() -> f64 {
        to_degrees(2.0 * atan(pow(E, PI)) - PI / 2.0)
    }
}

impl GeographicProjection for WebMercatorProjection {
    fn to_geo(&self, point: ProjectedCoordinate) -> ProjectionResult<GeoCoordinate> {
        let (x, y) = (point.x, point.y);
        if x < 0.0 || y < 0.0 || x > self.scale_from || y > self.scale_from {
            return Err(OutOfProjectionBounds);
        }
        Ok(GeoCoordinate::new(
            to_degrees(self.scale_to * x * TAU - PI),
            to_degrees(atan(exp(PI - self.scale_to * y * TAU)) * 2.0 - PI / 2.0),
        ))
    }

    fn from_geo(&self, point: GeoCoordinate) -> ProjectionResult<ProjectedCoordinate> {
        check_in_range(point.longitude, point.latitude, 180.0, Self::limit_latitude())?;
        Ok(ProjectedCoordinate::new(
            self.scale_from * (to_radians(point.longitude) + PI) / TAU,
            self.scale_from * (PI - ln(tan((PI / 2.0 + to_radians(point.latitude)) / 2.0))) / TAU,
        ))
    }

    fn meters_per_unit(&self) -> f64 {
        100_000.0
    }

    fn bounds(&self) -> ProjectedBounds {
        ProjectedBounds { min_x: 0.0, min_y: 0.0, max_x: self.scale_from, max_y: self.scale_from }
    }

    fn upright(&self) -> bool {
        true
    }
}
