//! Decoders for the compressed and raster formats used by TerraMinusMinus resources and raster tiles.
//!
//! Every entry point takes an explicit output limit so that hostile or corrupt inputs cannot
//! force unbounded allocation (decompression bombs, oversized image headers).

mod checksum;
mod deflate;
mod error;
mod gzip;
mod java_number;
mod png;
mod relaxed_json;
mod tiff;
mod zlib;

pub use checksum::{adler32, crc32};
pub use deflate::{inflate, inflate_streaming};
pub use error::DecodeError;
pub use gzip::{gunzip, gunzip_streaming};
pub use java_number::{java_double_compare, parse_java_double};
pub use png::{PngImage, PngLimits, decode_png};
pub use relaxed_json::{JsonError, JsonValue, parse_relaxed_json, parse_relaxed_json_with_duplicate_keys};
pub use tiff::{TiffLimits, TiffRaster, decode_tiff_scalar, decode_tiff_scalar_limited};
pub use zlib::zlib_decompress;
