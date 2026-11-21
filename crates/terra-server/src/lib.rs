//! Engine-neutral TerraPlusMinus server behaviour: configuration, `/tpll` parsing and
//! authorization, teleport planning across linked worlds, and assembly of the generation stack.

pub mod coordinates;
pub mod java_text;

pub use coordinates::{LatLng, UpstreamParseOutcome, parse_coordinates, parse_coordinates_upstream};
