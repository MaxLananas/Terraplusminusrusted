//! `Bounds2d`: closed axis-aligned boxes with TerraMinusMinus' comparison semantics.

use terra_elevation::numeric::{java_max, java_min};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds2d {
    pub min_x: f64,
    pub max_x: f64,
    pub min_z: f64,
    pub max_z: f64,
}

impl Bounds2d {
    /// `Bounds2d.of`: orders each pair with Java `Math.min`/`Math.max`.
    pub fn of(x0: f64, x1: f64, z0: f64, z1: f64) -> Self {
        Self { min_x: java_min(x0, x1), max_x: java_max(x0, x1), min_z: java_min(z0, z1), max_z: java_max(z0, z1) }
    }

    pub fn intersects(&self, other: &Bounds2d) -> bool {
        self.min_x <= other.max_x && self.max_x >= other.min_x && self.min_z <= other.max_z && self.max_z >= other.min_z
    }

    pub fn contains(&self, other: &Bounds2d) -> bool {
        self.min_x <= other.min_x && self.max_x >= other.max_x && self.min_z <= other.min_z && self.max_z >= other.max_z
    }

    pub fn expand(&self, offset: f64) -> Self {
        Self::of(self.min_x - offset, self.max_x + offset, self.min_z - offset, self.max_z + offset)
    }

    pub fn union(&self, other: &Bounds2d) -> Self {
        Self::of(java_min(self.min_x, other.min_x), java_max(self.max_x, other.max_x), java_min(self.min_z, other.min_z), java_max(self.max_z, other.max_z))
    }
}

pub trait Bounded {
    fn bounds(&self) -> Bounds2d;
}

impl<T: Bounded + ?Sized> Bounded for std::sync::Arc<T> {
    fn bounds(&self) -> Bounds2d {
        (**self).bounds()
    }
}
