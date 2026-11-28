//! Projection transforms (`net.buildtheearth.terraminusminus.projection.transform`).

use crate::{
    GeoCoordinate, GeographicProjection, ProjectedBounds, ProjectedCoordinate, ProjectionResult,
    SharedProjection,
};

#[derive(Debug, Clone)]
pub struct ScaleTransform {
    delegate: SharedProjection,
    x: f64,
    y: f64,
}

impl ScaleTransform {
    pub fn new(delegate: SharedProjection, x: f64, y: f64) -> Result<Self, &'static str> {
        if !x.is_finite() || !y.is_finite() {
            return Err("Projection scales should be finite");
        }
        if x == 0.0 || y == 0.0 {
            return Err("Projection scale cannot be 0!");
        }
        Ok(Self { delegate, x, y })
    }
}

impl GeographicProjection for ScaleTransform {
    fn to_geo(&self, point: ProjectedCoordinate) -> ProjectionResult<GeoCoordinate> {
        self.delegate.to_geo(ProjectedCoordinate::new(point.x / self.x, point.y / self.y))
    }

    fn from_geo(&self, point: GeoCoordinate) -> ProjectionResult<ProjectedCoordinate> {
        let p = self.delegate.from_geo(point)?;
        Ok(ProjectedCoordinate::new(p.x * self.x, p.y * self.y))
    }

    fn meters_per_unit(&self) -> f64 {
        self.delegate.meters_per_unit() / ((self.x * self.x + self.y * self.y) / 2.0).sqrt()
    }

    fn bounds(&self) -> ProjectedBounds {
        let b = self.delegate.bounds();
        ProjectedBounds { min_x: b.min_x * self.x, min_y: b.min_y * self.y, max_x: b.max_x * self.x, max_y: b.max_y * self.y }
    }

    fn upright(&self) -> bool {
        (self.y < 0.0) ^ self.delegate.upright()
    }
}

#[derive(Debug, Clone)]
pub struct FlipVerticalTransform {
    delegate: SharedProjection,
}

impl FlipVerticalTransform {
    pub fn new(delegate: SharedProjection) -> Self {
        Self { delegate }
    }
}

impl GeographicProjection for FlipVerticalTransform {
    fn to_geo(&self, point: ProjectedCoordinate) -> ProjectionResult<GeoCoordinate> {
        self.delegate.to_geo(ProjectedCoordinate::new(point.x, -point.y))
    }

    fn from_geo(&self, point: GeoCoordinate) -> ProjectionResult<ProjectedCoordinate> {
        let p = self.delegate.from_geo(point)?;
        Ok(ProjectedCoordinate::new(p.x, -p.y))
    }

    fn meters_per_unit(&self) -> f64 {
        self.delegate.meters_per_unit()
    }

    fn bounds(&self) -> ProjectedBounds {
        let b = self.delegate.bounds();
        ProjectedBounds { min_x: b.min_x, min_y: -b.max_y, max_x: b.max_x, max_y: -b.min_y }
    }

    fn upright(&self) -> bool {
        !self.delegate.upright()
    }
}

#[derive(Debug, Clone)]
pub struct FlipHorizontalTransform {
    delegate: SharedProjection,
}

impl FlipHorizontalTransform {
    pub fn new(delegate: SharedProjection) -> Self {
        Self { delegate }
    }
}

impl GeographicProjection for FlipHorizontalTransform {
    fn to_geo(&self, point: ProjectedCoordinate) -> ProjectionResult<GeoCoordinate> {
        self.delegate.to_geo(ProjectedCoordinate::new(-point.x, point.y))
    }

    fn from_geo(&self, point: GeoCoordinate) -> ProjectionResult<ProjectedCoordinate> {
        let p = self.delegate.from_geo(point)?;
        Ok(ProjectedCoordinate::new(-p.x, p.y))
    }

    fn meters_per_unit(&self) -> f64 {
        self.delegate.meters_per_unit()
    }

    /// Mirrors upstream verbatim, including its unusual corner order: `{-b[0], b[3], -b[2], b[1]}`.
    fn bounds(&self) -> ProjectedBounds {
        let b = self.delegate.bounds();
        ProjectedBounds { min_x: -b.min_x, min_y: b.max_y, max_x: -b.max_x, max_y: b.min_y }
    }

    fn upright(&self) -> bool {
        !self.delegate.upright()
    }
}

#[derive(Debug, Clone)]
pub struct SwapAxesTransform {
    delegate: SharedProjection,
}

impl SwapAxesTransform {
    pub fn new(delegate: SharedProjection) -> Self {
        Self { delegate }
    }
}

impl GeographicProjection for SwapAxesTransform {
    fn to_geo(&self, point: ProjectedCoordinate) -> ProjectionResult<GeoCoordinate> {
        self.delegate.to_geo(ProjectedCoordinate::new(point.y, point.x))
    }

    fn from_geo(&self, point: GeoCoordinate) -> ProjectionResult<ProjectedCoordinate> {
        let p = self.delegate.from_geo(point)?;
        Ok(ProjectedCoordinate::new(p.y, p.x))
    }

    fn meters_per_unit(&self) -> f64 {
        self.delegate.meters_per_unit()
    }

    fn bounds(&self) -> ProjectedBounds {
        let b = self.delegate.bounds();
        ProjectedBounds { min_x: b.min_y, min_y: b.min_x, max_x: b.max_y, max_y: b.max_x }
    }

    fn upright(&self) -> bool {
        self.delegate.upright()
    }
}

#[derive(Debug, Clone)]
pub struct OffsetTransform {
    delegate: SharedProjection,
    dx: f64,
    dy: f64,
}

impl OffsetTransform {
    pub fn new(delegate: SharedProjection, dx: f64, dy: f64) -> Result<Self, &'static str> {
        if !dx.is_finite() || !dy.is_finite() {
            return Err("Projection offsets have to be finite doubles");
        }
        Ok(Self { delegate, dx, dy })
    }
}

impl GeographicProjection for OffsetTransform {
    fn to_geo(&self, point: ProjectedCoordinate) -> ProjectionResult<GeoCoordinate> {
        self.delegate.to_geo(ProjectedCoordinate::new(point.x - self.dx, point.y - self.dy))
    }

    fn from_geo(&self, point: GeoCoordinate) -> ProjectionResult<ProjectedCoordinate> {
        let p = self.delegate.from_geo(point)?;
        Ok(ProjectedCoordinate::new(p.x + self.dx, p.y + self.dy))
    }

    fn meters_per_unit(&self) -> f64 {
        self.delegate.meters_per_unit()
    }

    fn bounds(&self) -> ProjectedBounds {
        let b = self.delegate.bounds();
        ProjectedBounds { min_x: b.min_x + self.dx, min_y: b.min_y + self.dy, max_x: b.max_x + self.dx, max_y: b.max_y + self.dy }
    }

    fn upright(&self) -> bool {
        self.delegate.upright()
    }
}
