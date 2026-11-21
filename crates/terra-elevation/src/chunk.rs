//! Per-chunk height sampling: `IEarthAsyncPipelineStep.getFuture` bounds plus `HeightsBaker.requestData`.

use crate::bounds::{AxisBounds, CoordinateSpace, CornerBounds};
use crate::error::ElevationError;
use crate::layered::LayeredScalarDataset;
use crate::provider::TileProvider;
use terra_projection::{GeographicProjection, ProjectionResult};

pub const CHUNK_SIZE: usize = 16;
pub const CHUNK_COLUMNS: usize = CHUNK_SIZE * CHUNK_SIZE;

/// The block-space bounds of a chunk; Java `int` arithmetic wraps for extreme chunk indices.
pub fn chunk_block_bounds(chunk_x: i32, chunk_z: i32) -> AxisBounds {
    let base_x = chunk_x.wrapping_shl(4);
    let base_z = chunk_z.wrapping_shl(4);
    AxisBounds::of(f64::from(base_x), f64::from(base_x.wrapping_add(16)), f64::from(base_z), f64::from(base_z.wrapping_add(16)))
}

/// `chunkBounds.toCornerBB(projection, false).toGeo()`.
pub fn chunk_geo_bounds(projection: &dyn GeographicProjection, chunk_x: i32, chunk_z: i32) -> ProjectionResult<CornerBounds> {
    CornerBounds::from_axis(&chunk_block_bounds(chunk_x, chunk_z), projection, CoordinateSpace::Projected)?.to_geo(projection)
}

/// Raw heights for the chunk (`x * 16 + z`), or `None` when upstream would bake no height data:
/// the chunk lies outside the world projection, no layer covers it, or a single-layer dataset
/// rejects its coordinates.
pub fn sample_chunk_heights(
    projection: &dyn GeographicProjection,
    heights: &LayeredScalarDataset,
    provider: &dyn TileProvider,
    chunk_x: i32,
    chunk_z: i32,
) -> Result<Option<Vec<f64>>, ElevationError> {
    let Ok(area) = chunk_geo_bounds(projection, chunk_x, chunk_z) else {
        return Ok(None);
    };
    match heights.sample_grid(&area, CHUNK_SIZE, CHUNK_SIZE, provider) {
        Err(ElevationError::OutOfProjectionBounds) => Ok(None),
        other => other,
    }
}
