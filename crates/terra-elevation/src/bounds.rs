//! Ports of `Bounds2d` and `CornerBoundingBox2d`.

use crate::numeric::{ceil_i, floor_i, java_max, java_min, lerp};
use terra_projection::{GeoCoordinate, GeographicProjection, OutOfProjectionBounds, ProjectedCoordinate, ProjectionResult};

/// Integer tile coordinates in a dataset's tile grid (`ChunkPos` in upstream tile contexts).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TilePosition {
    pub x: i32,
    pub z: i32,
}

/// Axis-aligned bounds; `Bounds2d.of` normalises with Java min/max semantics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AxisBounds {
    pub min_x: f64,
    pub max_x: f64,
    pub min_z: f64,
    pub max_z: f64,
}

impl AxisBounds {
    pub fn of(x0: f64, x1: f64, z0: f64, z1: f64) -> Self {
        Self { min_x: java_min(x0, x1), max_x: java_max(x0, x1), min_z: java_min(z0, z1), max_z: java_max(z0, z1) }
    }

    pub fn intersects(&self, other: &Self) -> bool {
        self.min_x <= other.max_x && self.max_x >= other.min_x && self.min_z <= other.max_z && self.max_z >= other.min_z
    }

    pub fn expand(&self, offset: f64) -> Self {
        Self::of(self.min_x - offset, self.max_x + offset, self.min_z - offset, self.max_z + offset)
    }

    /// `Bounds2d.toTiles(size)`, in upstream iteration order (x outer, z inner).
    pub fn to_tiles(&self, size: f64) -> Vec<TilePosition> {
        let inverse = 1.0 / size;
        let min_x = floor_i(self.min_x * inverse);
        let max_x = ceil_i(self.max_x * inverse);
        let min_z = floor_i(self.min_z * inverse);
        let max_z = ceil_i(self.max_z * inverse);
        let mut tiles = Vec::new();
        for x in min_x..=max_x {
            for z in min_z..=max_z {
                tiles.push(TilePosition { x, z });
            }
        }
        tiles
    }

    /// Number of tiles `to_tiles` would return, computed without allocating.
    pub fn tile_count(&self, size: f64) -> u64 {
        let inverse = 1.0 / size;
        let span = |min: f64, max: f64| (i64::from(ceil_i(max * inverse)) - i64::from(floor_i(min * inverse)) + 1).max(0);
        span(self.min_x, self.max_x).unsigned_abs() * span(self.min_z, self.max_z).unsigned_abs()
    }

    /// `Bounds2d.validate(proj, false)`: every corner must invert through the projection.
    pub fn validate_projected(&self, projection: &dyn GeographicProjection) -> ProjectionResult<Self> {
        for (x, z) in self.corners() {
            projection.to_geo(ProjectedCoordinate::new(x, z))?;
        }
        Ok(*self)
    }

    /// `Bounds2d.validate(proj, true)`.
    pub fn validate_geographic(&self, projection: &dyn GeographicProjection) -> ProjectionResult<Self> {
        for (x, z) in self.corners() {
            projection.from_geo(GeoCoordinate::new(x, z))?;
        }
        Ok(*self)
    }

    fn corners(&self) -> [(f64, f64); 4] {
        [(self.min_x, self.min_z), (self.min_x, self.max_z), (self.max_x, self.min_z), (self.max_x, self.max_z)]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoordinateSpace {
    Projected,
    Geographic,
}

/// A quadrilateral given by four corners: `p00` at (min x, min z), `p01` at (min x, max z),
/// `p10` at (max x, min z), `p11` at (max x, max z) of the source bounds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CornerBounds {
    corners: [(f64, f64); 4],
    space: CoordinateSpace,
}

impl CornerBounds {
    /// `Bounds2d.toCornerBB(proj, geo)`.
    pub fn from_axis(bounds: &AxisBounds, projection: &dyn GeographicProjection, space: CoordinateSpace) -> ProjectionResult<Self> {
        let (x, z) = (bounds.min_x, bounds.min_z);
        let size_x = bounds.max_x - x;
        let size_z = bounds.max_z - z;
        let corners = [(x, z), (x, z + size_z), (x + size_x, z), (x + size_x, z + size_z)];
        Self::validated(corners, projection, space)
    }

    fn validated(corners: [(f64, f64); 4], projection: &dyn GeographicProjection, space: CoordinateSpace) -> ProjectionResult<Self> {
        for (a, b) in corners {
            match space {
                CoordinateSpace::Geographic => projection.from_geo(GeoCoordinate::new(a, b)).map(drop)?,
                CoordinateSpace::Projected => projection.to_geo(ProjectedCoordinate::new(a, b)).map(drop)?,
            }
        }
        Ok(Self { corners, space })
    }

    pub fn space(&self) -> CoordinateSpace {
        self.space
    }

    pub fn corners(&self) -> [(f64, f64); 4] {
        self.corners
    }

    /// `CornerBoundingBox2d.toGeo(proj)`.
    pub fn to_geo(&self, projection: &dyn GeographicProjection) -> ProjectionResult<Self> {
        if self.space != CoordinateSpace::Projected {
            return Err(OutOfProjectionBounds);
        }
        let mut corners = [(0.0, 0.0); 4];
        for (target, (x, z)) in corners.iter_mut().zip(self.corners) {
            let geo = projection.to_geo(ProjectedCoordinate::new(x, z))?;
            *target = (geo.longitude, geo.latitude);
        }
        Self::validated(corners, projection, CoordinateSpace::Geographic)
    }

    /// `CornerBoundingBox2d.fromGeo(proj)`.
    pub fn from_geo(&self, projection: &dyn GeographicProjection) -> ProjectionResult<Self> {
        if self.space != CoordinateSpace::Geographic {
            return Err(OutOfProjectionBounds);
        }
        let mut corners = [(0.0, 0.0); 4];
        for (target, (lon, lat)) in corners.iter_mut().zip(self.corners) {
            let projected = projection.from_geo(GeoCoordinate::new(lon, lat))?;
            *target = (projected.x, projected.y);
        }
        Self::validated(corners, projection, CoordinateSpace::Projected)
    }

    /// Bilinear interpolation between the corners.
    pub fn point(&self, fx: f64, fz: f64) -> (f64, f64) {
        let [p00, p01, p10, p11] = self.corners;
        (
            lerp(lerp(p00.0, p01.0, fz), lerp(p10.0, p11.0, fz), fx),
            lerp(lerp(p00.1, p01.1, fz), lerp(p10.1, p11.1, fz), fx),
        )
    }

    /// The axis-aligned hull, without the projection validation `axisAlign()` adds.
    pub fn hull(&self) -> AxisBounds {
        let [p00, p01, p10, p11] = self.corners;
        AxisBounds {
            min_x: java_min(java_min(p00.0, p01.0), java_min(p10.0, p11.0)),
            max_x: java_max(java_max(p00.0, p01.0), java_max(p10.0, p11.0)),
            min_z: java_min(java_min(p00.1, p01.1), java_min(p10.1, p11.1)),
            max_z: java_max(java_max(p00.1, p01.1), java_max(p10.1, p11.1)),
        }
    }

    /// `CornerBoundingBox2d.axisAlign()`, validated against `projection` in this box's space.
    pub fn axis_align(&self, projection: &dyn GeographicProjection) -> ProjectionResult<AxisBounds> {
        let hull = self.hull();
        let bounds = AxisBounds::of(hull.min_x, hull.max_x, hull.min_z, hull.max_z);
        match self.space {
            CoordinateSpace::Projected => bounds.validate_projected(projection),
            CoordinateSpace::Geographic => bounds.validate_geographic(projection),
        }
    }
}
