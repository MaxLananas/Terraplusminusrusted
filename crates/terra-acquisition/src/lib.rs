//! Tile acquisition for remote datasets: an HTTP/1.1 transport with hard limits, a retry and
//! rate-limit policy, and a crash-safe, size-bounded disk cache.

pub mod disk_cache;
pub mod external;
pub mod http;
pub mod policy;
pub mod routing;
pub mod subprocess;

pub use disk_cache::{CacheLookup, DEFAULT_CACHE_TTL, DiskCache, DiskCacheLimits, DiskCachedFetcher};
pub use external::{CurlTransport, DualTransport};
pub use http::{DEFAULT_USER_AGENT, HttpResponse, HttpTransport, PlainHttpTransport, TransportError, TransportLimits};
pub use policy::{DEFAULT_HOST_LIMITS, HttpTileFetcher, RetryPolicy, parse_host_limit};
pub use routing::RoutingFetcher;
pub use subprocess::{ProcessError, ProcessLimits, ProcessOutput, run_bounded};
