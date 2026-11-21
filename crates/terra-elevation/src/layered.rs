//! `MultiScalarDataset`: prioritised, optionally conditional scalar layers merged by NaN-filling.

use crate::bounds::{AxisBounds, CornerBounds};
use crate::condition::DoubleCondition;
use crate::dataset::{LoadedTiles, TiledScalarDataset};
use crate::error::ElevationError;
use crate::provider::TileProvider;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct ScalarLayer {
    pub dataset: Arc<TiledScalarDataset>,
    /// Geographic bounds (`minX`/`maxX` longitude, `minZ`/`maxZ` latitude).
    pub bounds: AxisBounds,
    pub priority: f64,
    pub condition: Option<DoubleCondition>,
}

impl ScalarLayer {
    fn accepts(&self, value: f64) -> bool {
        self.condition.as_ref().is_none_or(|condition| condition.test(value))
    }
}

#[derive(Debug, Clone, Default)]
pub struct LayeredScalarDataset {
    layers: Vec<ScalarLayer>,
}

impl LayeredScalarDataset {
    /// Layers with several bounds must already be flattened into one layer per bounds entry.
    pub fn new(layers: Vec<ScalarLayer>) -> Self {
        Self { layers }
    }

    pub fn layers(&self) -> &[ScalarLayer] {
        &self.layers
    }

    /// Layers intersecting `area`, highest priority first. Equal priorities keep configuration
    /// order (upstream keeps BVH traversal order, see docs/feature-matrix.md).
    pub fn intersecting(&self, area: &AxisBounds) -> Vec<&ScalarLayer> {
        let mut matching: Vec<&ScalarLayer> = self.layers.iter().filter(|layer| layer.bounds.intersects(area)).collect();
        matching.sort_by(|a, b| b.priority.total_cmp(&a.priority));
        matching
    }

    /// `getAsync(CornerBoundingBox2d, sizeX, sizeZ)`. `Ok(None)` means no layer covers the area.
    ///
    /// With exactly one unconditional layer, projection failures surface as
    /// `OutOfProjectionBounds` (thrown synchronously upstream); with several layers they surface
    /// as `LayerOutOfProjectionBounds` (an exceptionally completed future upstream).
    pub fn sample_grid(
        &self,
        area: &CornerBounds,
        size_x: usize,
        size_z: usize,
        provider: &dyn TileProvider,
    ) -> Result<Option<Vec<f64>>, ElevationError> {
        if size_x == 0 || size_z == 0 {
            return Ok(Some(Vec::new()));
        }
        let layers = self.intersecting(&area.hull());
        match layers.as_slice() {
            [] => return Ok(None),
            [only] if only.condition.is_none() => return sample_layer(only, area, size_x, size_z, provider).map(Some),
            _ => {}
        }
        let total = size_x * size_z;
        let mut out: Option<Vec<f64>> = None;
        let mut remaining = total;
        for layer in layers {
            let data = sample_layer(layer, area, size_x, size_z, provider).map_err(|error| match error {
                ElevationError::OutOfProjectionBounds => ElevationError::LayerOutOfProjectionBounds,
                other => other,
            })?;
            let merged = out.get_or_insert_with(|| vec![f64::NAN; total]);
            for (slot, value) in merged.iter_mut().zip(data) {
                if slot.is_nan() && !value.is_nan() && layer.accepts(value) {
                    *slot = value;
                    remaining -= 1;
                    if remaining == 0 {
                        return Ok(out);
                    }
                }
            }
        }
        Ok(out)
    }
}

fn sample_layer(
    layer: &ScalarLayer,
    area: &CornerBounds,
    size_x: usize,
    size_z: usize,
    provider: &dyn TileProvider,
) -> Result<Vec<f64>, ElevationError> {
    let dataset = layer.dataset.as_ref();
    let plan = dataset.plan(area)?;
    let mut tiles = LoadedTiles::with_capacity(plan.tiles.len());
    for (position, tile) in plan.tiles.iter().zip(provider.load_many(dataset, &plan.tiles)?) {
        tiles.insert(*position, tile);
    }
    Ok(dataset.sample(&plan, size_x, size_z, &tiles))
}
