//! `MultiScalarDataset`: prioritised, optionally conditional scalar layers merged by NaN-filling.

use crate::bounds::{AxisBounds, CornerBounds};
use crate::condition::DoubleCondition;
use crate::dataset::{LoadedTiles, TiledScalarDataset};
use crate::error::ElevationError;
use crate::provider::TileProvider;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct ScalarLayer {
    pub dataset: Arc<TiledScalarDataset>,
    /// Geographic bounds (`minX`/`maxX` longitude, `minZ`/`maxZ` latitude).
    pub bounds: AxisBounds,
    pub priority: f64,
    pub condition: Option<DoubleCondition>,
    /// `"optional": true` (this port's extension; upstream has no such key): when loading the
    /// layer's tiles fails, its samples are `NaN` so lower layers fill in, instead of the whole
    /// request failing. Shared by every bounds entry of one dataset.
    pub optional: Option<Arc<OptionalLayer>>,
}

/// How long an optional layer is left out after a failure, so that an unreachable service costs
/// one round of retries per minute rather than one per chunk.
pub const OPTIONAL_LAYER_PAUSE: Duration = Duration::from_secs(60);

/// The failure state of an optional layer.
#[derive(Debug, Default)]
pub struct OptionalLayer {
    paused_until: Mutex<Option<Instant>>,
}

impl OptionalLayer {
    fn paused(&self, now: Instant) -> bool {
        self.paused_until.lock().unwrap_or_else(PoisonError::into_inner).is_some_and(|until| now < until)
    }

    fn failed(&self, now: Instant) {
        *self.paused_until.lock().unwrap_or_else(PoisonError::into_inner) = now.checked_add(OPTIONAL_LAYER_PAUSE);
    }
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
            [only] if only.condition.is_none() && only.optional.is_none() => return sample_layer(only, area, size_x, size_z, provider).map(Some),
            _ => {}
        }
        let total = size_x * size_z;
        let mut out: Option<Vec<f64>> = None;
        let mut remaining = total;
        for layer in layers {
            let data = match &layer.optional {
                None => sample_layer(layer, area, size_x, size_z, provider).map_err(|error| match error {
                    ElevationError::OutOfProjectionBounds => ElevationError::LayerOutOfProjectionBounds,
                    other => other,
                })?,
                Some(state) => {
                    if state.paused(Instant::now()) {
                        continue;
                    }
                    match sample_layer(layer, area, size_x, size_z, provider) {
                        Ok(data) => data,
                        Err(_) => {
                            state.failed(Instant::now());
                            continue;
                        }
                    }
                }
            };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blend::BlendMode;
    use crate::bounds::TilePosition;
    use crate::dataset::{Tile, TileFormat};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use terra_projection::{GeographicProjection, WebMercatorProjection};

    /// Answers every tile of dataset `ok` with `value` and fails every tile of `failing`.
    struct FakeProvider {
        failing: String,
        failures: AtomicUsize,
    }

    impl TileProvider for FakeProvider {
        fn load(&self, dataset: &TiledScalarDataset, _position: TilePosition) -> Result<Option<Tile>, ElevationError> {
            if dataset.id == self.failing {
                self.failures.fetch_add(1, Ordering::SeqCst);
                return Err(ElevationError::Fetch { url: "https://data.geopf.fr/wms-r".into(), reason: "HTTP 503".into() });
            }
            let value = if dataset.id == "high" { 2.0 } else { 1.0 };
            Ok(Some(vec![value; (dataset.resolution() * dataset.resolution()) as usize].into()))
        }
    }

    fn layer(id: &str, priority: f64, optional: Option<Arc<OptionalLayer>>) -> ScalarLayer {
        let projection: Arc<dyn GeographicProjection> = Arc::new(WebMercatorProjection::new(8).unwrap());
        let dataset = TiledScalarDataset::new(id.into(), vec!["https://example.invalid/${x}/${z}.tiff".into()], projection, 256, BlendMode::Near, TileFormat::Tiff).unwrap();
        ScalarLayer { dataset: Arc::new(dataset), bounds: AxisBounds::of(-180.0, 180.0, -85.0, 85.0), priority, condition: None, optional }
    }

    fn area() -> CornerBounds {
        let projection = WebMercatorProjection::new(8).unwrap();
        CornerBounds::from_axis(&AxisBounds::of(2.30, 2.31, 48.85, 48.86), &projection, crate::bounds::CoordinateSpace::Geographic).unwrap()
    }

    #[test]
    fn a_failing_optional_layer_falls_through_and_pauses() {
        let state = Arc::new(OptionalLayer::default());
        let dataset = LayeredScalarDataset::new(vec![layer("high", 150.0, Some(Arc::clone(&state))), layer("low", 100.0, None)]);
        let provider = FakeProvider { failing: "high".into(), failures: AtomicUsize::new(0) };
        let values = dataset.sample_grid(&area(), 4, 4, &provider).unwrap().unwrap();
        assert!(values.iter().all(|&value| value == 1.0), "{values:?}");
        let failures = provider.failures.load(Ordering::SeqCst);
        assert!(failures > 0);
        // Paused: the second request does not touch the failing service.
        assert_eq!(dataset.sample_grid(&area(), 4, 4, &provider).unwrap().unwrap(), values);
        assert_eq!(provider.failures.load(Ordering::SeqCst), failures);
        // After the pause, it is tried again.
        *state.paused_until.lock().unwrap() = Some(Instant::now());
        dataset.sample_grid(&area(), 4, 4, &provider).unwrap();
        assert!(provider.failures.load(Ordering::SeqCst) > failures);
    }

    #[test]
    fn a_working_optional_layer_wins_and_a_required_one_still_fails() {
        let dataset = LayeredScalarDataset::new(vec![layer("high", 150.0, Some(Arc::new(OptionalLayer::default()))), layer("low", 100.0, None)]);
        let healthy = FakeProvider { failing: "none".into(), failures: AtomicUsize::new(0) };
        assert!(dataset.sample_grid(&area(), 4, 4, &healthy).unwrap().unwrap().iter().all(|&value| value == 2.0));
        let required = LayeredScalarDataset::new(vec![layer("high", 150.0, None), layer("low", 100.0, None)]);
        let broken = FakeProvider { failing: "high".into(), failures: AtomicUsize::new(0) };
        assert!(required.sample_grid(&area(), 4, 4, &broken).is_err());
        // A lone optional layer that fails yields NaN rather than an error.
        let lone = LayeredScalarDataset::new(vec![layer("high", 150.0, Some(Arc::new(OptionalLayer::default())))]);
        assert!(lone.sample_grid(&area(), 4, 4, &broken).unwrap().unwrap_or_default().iter().all(|value| value.is_nan()));
    }
}
