//! Elevation datasets ported from TerraMinusMinus: tile decoding, blending, layered merging and
//! per-chunk sampling, with pluggable tile acquisition.

pub mod blend;
pub mod bounds;
pub mod chunk;
pub mod condition;
pub mod config;
pub mod dataset;
pub mod error;
pub mod layered;
pub mod numeric;
pub mod provider;
pub mod terrarium;

pub use blend::BlendMode;
pub use bounds::{AxisBounds, CoordinateSpace, CornerBounds, TilePosition};
pub use chunk::{CHUNK_COLUMNS, CHUNK_SIZE, chunk_block_bounds, chunk_geo_bounds, sample_chunk_heights};
pub use condition::DoubleCondition;
pub use config::{DEFAULT_WORLD_HEIGHTS, LoadedLayers, UnsupportedLayerPolicy, load_heights_config};
pub use dataset::{GridPlan, LoadedTiles, MAX_TILES_PER_REQUEST, Tile, TileFormat, TiledScalarDataset};
pub use error::ElevationError;
pub use layered::{LayeredScalarDataset, ScalarLayer};
pub use provider::{CacheStatistics, CachingTileProvider, FetchOutcome, MirrorFetcher, TileFetcher, TileProvider, fetch_first};
pub use terrarium::{decode_terrarium_png, terrarium_value};
