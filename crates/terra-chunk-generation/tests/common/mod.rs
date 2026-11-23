//! Offline Grand Canyon generator shared by the integration tests.

#![allow(dead_code, clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;
use terra_chunk_generation::{GeneratorSettings, KoppenMap, TerrainGenerator, WorldHeight};
use terra_elevation::{AxisBounds, BlendMode, CachingTileProvider, LayeredScalarDataset, MirrorFetcher, ScalarLayer, TileFormat, TileProvider, TiledScalarDataset};
use terra_projection::{GeoCoordinate, SharedProjection, WebMercatorProjection, terraplusminus_projection};

const MIRROR_PREFIX: &str = "mirror://terrarium/";
pub const WORLD: WorldHeight = WorldHeight { min_y: -64, max_y: 2032 };

pub fn generator(koppen: bool) -> TerrainGenerator {
    generator_with(koppen, |provider| provider)
}

pub fn offline_provider() -> Arc<dyn TileProvider> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/elevation/terrarium");
    Arc::new(CachingTileProvider::new(MirrorFetcher::new().with_mapping(MIRROR_PREFIX, root), 16))
}

pub fn generator_with(koppen: bool, wrap: impl FnOnce(Arc<dyn TileProvider>) -> Arc<dyn TileProvider>) -> TerrainGenerator {
    let projection: SharedProjection = Arc::new(WebMercatorProjection::new(12).unwrap());
    let dataset = TiledScalarDataset::new(
        "offline-terrarium-z12".into(),
        vec![format!("{MIRROR_PREFIX}12/${{x}}/${{z}}.png")],
        projection,
        256,
        BlendMode::Cubic,
        TileFormat::TerrariumPng,
    )
    .unwrap();
    let heights = LayeredScalarDataset::new(vec![ScalarLayer {
        dataset: Arc::new(dataset),
        bounds: AxisBounds::of(-180.0, 180.0, -85.0, 85.0),
        priority: 0.0,
        condition: None,
        optional: None,
    }]);
    let provider = wrap(offline_provider());
    let settings = GeneratorSettings { use_biome_dataset: koppen, ..GeneratorSettings::default() };
    let koppen = koppen.then(|| KoppenMap::embedded().unwrap());
    TerrainGenerator::new(terraplusminus_projection(0, 0).unwrap(), Arc::new(heights), provider, koppen, settings)
}

pub fn centre_chunk(generator: &TerrainGenerator) -> (i32, i32) {
    let block = generator.projection().from_geo(GeoCoordinate::new(-113.247, 35.995)).unwrap();
    ((block.x.floor() as i32) >> 4, (block.y.floor() as i32) >> 4)
}

