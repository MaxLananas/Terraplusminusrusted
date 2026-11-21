//! Tile acquisition for remote datasets: an HTTP/1.1 transport with hard limits, a retry and
//! rate-limit policy, and a crash-safe, size-bounded disk cache.

pub mod disk_cache;
pub mod http;
pub mod policy;

pub use disk_cache::{DiskCache, DiskCacheLimits, DiskCachedFetcher};
pub use http::{HttpResponse, HttpTransport, PlainHttpTransport, TransportError, TransportLimits};
pub use policy::{HttpTileFetcher, RetryPolicy};
