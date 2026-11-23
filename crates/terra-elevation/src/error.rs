use std::fmt;
use terra_codec::DecodeError;
use terra_projection::OutOfProjectionBounds;

#[derive(Debug, Clone, PartialEq)]
pub enum ElevationError {
    OutOfProjectionBounds,
    /// A projection failure inside a multi-layer merge, which upstream reports as a failed load.
    LayerOutOfProjectionBounds,
    InvalidConfiguration(String),
    RequestTooLarge { tiles: u64 },
    Decode { url: String, error: DecodeError },
    TileDecode(DecodeError),
    Fetch { url: String, reason: String },
    /// The URL cannot be fetched by this build's configuration at all (HTTPS without curl, no
    /// mirror, an unsupported scheme): retrying cannot help.
    Unreachable { url: String, reason: String },
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
            Self::RequestTooLarge { tiles } => write!(f, "request touches {tiles} tiles, above the safety limit"),
            Self::Decode { url, error } => write!(f, "failed to decode {url}: {error}"),
            Self::TileDecode(error) => write!(f, "failed to decode tile: {error}"),
            Self::Fetch { url, reason } => write!(f, "failed to fetch {url}: {reason}"),
            Self::Unreachable { url, reason } => write!(f, "cannot fetch {url}: {reason}"),
            Self::AllSourcesFailed { dataset, failures } => {
                write!(f, "all sources failed for dataset {dataset}: {}", failures.join("; "))
            }
            Self::Cancelled => f.write_str("request cancelled"),
            Self::Overloaded => f.write_str("tile loader queue is full"),
        }
    }
}

impl std::error::Error for ElevationError {}

impl ElevationError {
    /// Whether retrying the same request can succeed. Configuration errors, malformed tile content and unreachable URLs are permanent; network failures, overload and
    /// cancellation are not. Missing tiles are not errors (they are ocean).
    pub fn is_permanent(&self) -> bool {
        match self {
            Self::InvalidConfiguration(_) | Self::RequestTooLarge { .. } | Self::Decode { .. } | Self::TileDecode(_) | Self::Unreachable { .. } => true,
            Self::OutOfProjectionBounds | Self::LayerOutOfProjectionBounds => true,
            Self::AllSourcesFailed { .. } | Self::Fetch { .. } | Self::Cancelled | Self::Overloaded => false,
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    /// The retry policy depends on this split: a permanent error stops the chunk at once, a
    /// transient one is retried with backoff. Every variant is listed, so adding one forces a
    /// decision here.
    #[test]
    fn permanence_of_every_variant() {
        let url = || "https://example.test/1.png".to_owned();
        let cases = [
            (ElevationError::OutOfProjectionBounds, true),
            (ElevationError::LayerOutOfProjectionBounds, true),
            (ElevationError::InvalidConfiguration("x".into()), true),
            (ElevationError::RequestTooLarge { tiles: 9 }, true),
            (ElevationError::Decode { url: url(), error: DecodeError::Truncated }, true),
            (ElevationError::TileDecode(DecodeError::Truncated), true),
            (ElevationError::Unreachable { url: url(), reason: "no curl for HTTPS".into() }, true),
            (ElevationError::Fetch { url: url(), reason: "reset".into() }, false),
            (ElevationError::AllSourcesFailed { dataset: "d".into(), failures: vec!["reset".into()] }, false),
            (ElevationError::Cancelled, false),
            (ElevationError::Overloaded, false),
        ];
        for (error, permanent) in cases {
            assert_eq!(error.is_permanent(), permanent, "{error}");
            match error {
                ElevationError::OutOfProjectionBounds
                | ElevationError::LayerOutOfProjectionBounds
                | ElevationError::InvalidConfiguration(_)
                | ElevationError::RequestTooLarge { .. }
                | ElevationError::Decode { .. }
                | ElevationError::TileDecode(_)
                | ElevationError::Fetch { .. }
                | ElevationError::Unreachable { .. }
                | ElevationError::AllSourcesFailed { .. }
                | ElevationError::Cancelled
                | ElevationError::Overloaded => {}
            }
        }
    }
}
