use std::fmt;
use terra_codec::DecodeError;
use terra_projection::OutOfProjectionBounds;

#[derive(Debug, Clone, PartialEq)]
pub enum ElevationError {
    OutOfProjectionBounds,
    /// A projection failure inside a multi-layer merge, which upstream reports as a failed load.
    LayerOutOfProjectionBounds,
    InvalidConfiguration(String),
    UnsupportedFormat(&'static str),
    RequestTooLarge { tiles: u64 },
    Decode { url: String, error: DecodeError },
    TileDecode(DecodeError),
    Fetch { url: String, reason: String },
    /// Every URL failed for a tile (`Http.getFirst` "All URLs completed exceptionally!").
    AllSourcesFailed { dataset: String, failures: Vec<String> },
    Cancelled,
    Overloaded,
}

impl fmt::Display for ElevationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfProjectionBounds => f.write_str("coordinates are outside the dataset projection"),
            Self::LayerOutOfProjectionBounds => f.write_str("a dataset layer rejected the area's coordinates"),
            Self::InvalidConfiguration(reason) => write!(f, "invalid dataset configuration: {reason}"),
            Self::UnsupportedFormat(name) => write!(f, "tile format {name} is not supported"),
            Self::RequestTooLarge { tiles } => write!(f, "request touches {tiles} tiles, above the safety limit"),
            Self::Decode { url, error } => write!(f, "failed to decode {url}: {error}"),
            Self::TileDecode(error) => write!(f, "failed to decode tile: {error}"),
            Self::Fetch { url, reason } => write!(f, "failed to fetch {url}: {reason}"),
            Self::AllSourcesFailed { dataset, failures } => {
                write!(f, "all sources failed for dataset {dataset}: {}", failures.join("; "))
            }
            Self::Cancelled => f.write_str("request cancelled"),
            Self::Overloaded => f.write_str("tile loader queue is full"),
        }
    }
}

impl std::error::Error for ElevationError {}

impl From<OutOfProjectionBounds> for ElevationError {
    fn from(_: OutOfProjectionBounds) -> Self {
        Self::OutOfProjectionBounds
    }
}

impl From<DecodeError> for ElevationError {
    fn from(error: DecodeError) -> Self {
        Self::TileDecode(error)
    }
}
