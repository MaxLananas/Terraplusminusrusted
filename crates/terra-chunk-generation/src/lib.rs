//! TerraPlusMinus terrain generation on top of TerraMinusMinus chunk data.

pub mod biome;
pub mod chunk_data;
pub mod fingerprint;
pub mod generator;
pub mod java_random;
pub mod loader;
pub mod trees;

pub use biome::{Biome, KoppenMap};
pub use chunk_data::{ChunkDataBuilder, ChunkTerrain, FALLBACK_TREE_DENSITY, SurfaceBlock, is_null_island, tree_chance};
pub use generator::{Block, ColumnSink, FIXED_SPAWN, GenerationError, GeneratorSettings, HeightmapKind, TerrainGenerator, WorldHeight};
pub use loader::{ChunkKey, ChunkLoader, ChunkTicket, LoaderDiagnostics, LoaderError, LoaderLimits};
pub use trees::{PlacedTree, TreeError, TreeLibrary, TreePopulator};
pub use fingerprint::{FINGERPRINT_FILE_NAME, FingerprintError, FingerprintStatus, GenerationFingerprint, verify_or_record};
