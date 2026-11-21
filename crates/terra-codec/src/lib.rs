//! Decoders for the compressed formats used by TerraMinusMinus resources and raster tiles.
//!
//! Every entry point takes an explicit output limit so that hostile or corrupt inputs cannot
//! force unbounded allocation (decompression bombs, oversized image headers).

mod checksum;
mod deflate;
mod error;
mod gzip;
mod png;
mod relaxed_json;
mod zlib;

pub use checksum::{adler32, crc32};
pub use deflate::{inflate, inflate_streaming};
pub use error::DecodeError;
pub use gzip::{gunzip, gunzip_streaming};
pub use png::{PngImage, PngLimits, decode_png};
pub use relaxed_json::{JsonError, JsonValue, parse_relaxed_json};
pub use zlib::zlib_decompress;
