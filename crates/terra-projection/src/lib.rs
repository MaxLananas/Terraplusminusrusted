//! Geographic projections ported from TerraMinusMinus (`net.buildtheearth.terraminusminus.projection`).
//!
//! Axis conventions follow upstream exactly: geographic coordinates are `(longitude, latitude)` in
//! degrees on a sphere; projected coordinates are `(x, y)` in projection units. For the
//! BuildTheEarth world projection, projected `x` is Minecraft block X and projected `y` is
//! Minecraft block Z (the vertical flip makes Z grow southward, as in Minecraft).
//!
//! Floating-point evaluation order mirrors the Java sources so that results stay within the
//! last-place differences permitted by `java.lang.Math` (see `docs/numerics.md`).

mod dymaxion;
mod java_math;
mod mercator;
mod transform;

use std::fmt;
use std::sync::Arc;

pub use dymaxion::{BteDymaxionProjection, ConformalDymaxionProjection, DymaxionProjection, VectorFieldError};
pub use mercator::WebMercatorProjection;
pub use transform::{
    FlipHorizontalTransform, FlipVerticalTransform, OffsetTransform, ScaleTransform, SwapAxesTransform,
};

/// Scale applied by `bte_generator_settings.json5` to the flipped BTE conformal Dymaxion.
pub const BTE_PROJECTION_SCALE: f64 = 7_318_261.522_857_145;

/// Equatorial circumference used by `TerraConstants.EARTH_CIRCUMFERENCE`.
pub const EARTH_CIRCUMFERENCE: f64 = 40_075_017.0;
/// Polar circumference used by `TerraConstants.EARTH_POLAR_CIRCUMFERENCE`.
pub const EARTH_POLAR_CIRCUMFERENCE: f64 = 40_008_000.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeoCoordinate {
    pub longitude: f64,
    pub latitude: f64,
}

impl GeoCoordinate {
    pub const fn new(longitude: f64, latitude: f64) -> Self {
        Self { longitude, latitude }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProjectedCoordinate {
    pub x: f64,
    pub y: f64,
}

impl ProjectedCoordinate {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// Axis-aligned bounds in projected units: `[min_x, min_y, max_x, max_y]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProjectedBounds {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutOfProjectionBounds;

impl fmt::Display for OutOfProjectionBounds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("coordinates are outside the projection's valid area")
    }
}

impl std::error::Error for OutOfProjectionBounds {}

pub type ProjectionResult<T> = Result<T, OutOfProjectionBounds>;

/// `OutOfProjectionBoundsException.checkInRange`.
pub(crate) fn check_in_range(x: f64, y: f64, max_x: f64, max_y: f64) -> ProjectionResult<()> {
    if x.abs() > max_x || y.abs() > max_y { Err(OutOfProjectionBounds) } else { Ok(()) }
}

pub(crate) fn check_longitude_latitude(longitude: f64, latitude: f64) -> ProjectionResult<()> {
    check_in_range(longitude, latitude, 180.0, 90.0)
}

pub trait GeographicProjection: Send + Sync + fmt::Debug {
    fn to_geo(&self, point: ProjectedCoordinate) -> ProjectionResult<GeoCoordinate>;

    fn from_geo(&self, point: GeoCoordinate) -> ProjectionResult<ProjectedCoordinate>;

    fn meters_per_unit(&self) -> f64;

    /// `GeographicProjection.bounds()` default implementation.
    fn bounds(&self) -> ProjectedBounds {
        let extremes = (|| -> ProjectionResult<[f64; 4]> {
            Ok([
                self.from_geo(GeoCoordinate::new(-180.0, 0.0))?.x,
                self.from_geo(GeoCoordinate::new(0.0, -90.0))?.y,
                self.from_geo(GeoCoordinate::new(180.0, 0.0))?.x,
                self.from_geo(GeoCoordinate::new(0.0, 90.0))?.y,
            ])
        })();
        match extremes {
            Ok([mut min_x, mut min_y, mut max_x, mut max_y]) => {
                if min_x > max_x {
                    std::mem::swap(&mut min_x, &mut max_x);
                }
                if min_y > max_y {
                    std::mem::swap(&mut min_y, &mut max_y);
                }
                ProjectedBounds { min_x, min_y, max_x, max_y }
            }
            Err(_) => ProjectedBounds { min_x: 0.0, min_y: 0.0, max_x: 1.0, max_y: 1.0 },
        }
    }

    fn upright(&self) -> bool {
        match (self.from_geo(GeoCoordinate::new(0.0, 90.0)), self.from_geo(GeoCoordinate::new(0.0, -90.0))) {
            (Ok(north), Ok(south)) => north.y <= south.y,
            _ => false,
        }
    }

    /// `GeographicProjection.vector`: projected displacement of moving `north`/`east` metres.
    fn vector(&self, point: ProjectedCoordinate, north: f64, east: f64) -> ProjectionResult<[f64; 2]> {
        let geo = self.to_geo(point)?;
        let offset = self.from_geo(GeoCoordinate::new(
            geo.longitude
                + east * 360.0 / (java_math::cos(java_math::to_radians(geo.latitude)) * EARTH_CIRCUMFERENCE),
            geo.latitude + north * 360.0 / EARTH_POLAR_CIRCUMFERENCE,
        ))?;
        Ok([offset.x - point.x, offset.y - point.y])
    }

    /// `GeographicProjection.azimuth(x, y, angle)` with the upstream default step of `1E-5`.
    fn azimuth(&self, point: ProjectedCoordinate, angle: f32) -> ProjectionResult<f32> {
        let step = 1e-5;
        let angle_radians = java_math::to_radians(f64::from(angle));
        let x2 = point.x - step * java_math::sin(angle_radians);
        let y2 = point.y + step * java_math::cos(angle_radians);
        let first = self.to_geo(point)?;
        let second = self.to_geo(ProjectedCoordinate::new(x2, y2))?;
        let (lon1, lat1) = (java_math::to_radians(first.longitude), java_math::to_radians(first.latitude));
        let (lon2, lat2) = (java_math::to_radians(second.longitude), java_math::to_radians(second.latitude));
        let mut azimuth = java_math::to_degrees(java_math::atan2(lat2 - lat1, (lon2 - lon1) * java_math::cos(lat1)));
        azimuth = 90.0 - azimuth;
        if azimuth < 0.0 {
            azimuth += 360.0;
        }
        Ok(azimuth as f32)
    }
}

pub type SharedProjection = Arc<dyn GeographicProjection>;

/// `EarthGeneratorSettings.BTE_DEFAULT_SETTINGS.projection()`:
/// `scale(flip_vertical(bte_conformal_dymaxion), 7318261.522857145, 7318261.522857145)`.
pub fn bte_world_projection() -> Result<SharedProjection, ProjectionBuildError> {
    let dymaxion = BteDymaxionProjection::new().map_err(ProjectionBuildError::VectorField)?;
    let flipped = FlipVerticalTransform::new(Arc::new(dymaxion));
    Ok(Arc::new(ScaleTransform::new(Arc::new(flipped), BTE_PROJECTION_SCALE, BTE_PROJECTION_SCALE)?))
}

/// The projection TerraPlusMinus' `RealWorldGenerator` builds: the BTE world projection wrapped
/// in `OffsetProjectionTransform(terrain_offset.x, terrain_offset.z)`.
pub fn terraplusminus_projection(x_offset: i32, z_offset: i32) -> Result<SharedProjection, ProjectionBuildError> {
    let base = bte_world_projection()?;
    let offset = OffsetTransform::new(base, f64::from(x_offset), f64::from(z_offset))?;
    Ok(Arc::new(offset))
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProjectionBuildError {
    VectorField(VectorFieldError),
    InvalidParameter(&'static str),
}

impl fmt::Display for ProjectionBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::VectorField(error) => write!(f, "{error}"),
            Self::InvalidParameter(reason) => write!(f, "invalid projection parameter: {reason}"),
        }
    }
}

impl std::error::Error for ProjectionBuildError {}

impl From<&'static str> for ProjectionBuildError {
    fn from(reason: &'static str) -> Self {
        Self::InvalidParameter(reason)
    }
}

#[cfg(test)]
mod tests;
