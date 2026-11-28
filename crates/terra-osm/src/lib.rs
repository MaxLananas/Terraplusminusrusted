//! OpenStreetMap features for the generator: TerraMinusMinus' GeoJSON tiles, `osm.json5` mapping
//! and rasterisation of water, coastlines, roads and buildings onto chunk data.

pub mod bounds;
pub mod bvh;
pub mod cache;
pub mod dataset;
pub mod draw;
pub mod geojson;
pub mod geometry;
pub mod interval_tree;
pub mod mapper;

pub use bounds::Bounds2d;
pub use dataset::{ChunkElements, DEFAULT_OSM_SERVERS, OsmDataset, OsmError, OsmSettings, OsmStatistics, apply_elements};
pub use draw::{BlockState, DrawFunction, DrawTarget, PropertyValue};
pub use geometry::{Point, VectorGeometry};
pub use mapper::{DEFAULT_OSM_CONFIG, MapperError, OsmMapper};
