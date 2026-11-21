use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    Truncated,
    InvalidBlockType,
    InvalidStoredLength,
    InvalidHuffmanTable(&'static str),
    InvalidSymbol,
    DistanceTooFar,
    OutputLimitExceeded { limit: usize },
    InvalidGzipHeader(&'static str),
    InvalidZlibHeader(&'static str),
    ChecksumMismatch { kind: &'static str, expected: u32, actual: u32 },
    LengthMismatch { expected: u32, actual: u32 },
    InvalidPng(&'static str),
    UnsupportedPng(String),
    ImageTooLarge { width: u32, height: u32 },
    Sink(String),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => write!(f, "compressed stream is truncated"),
            Self::InvalidBlockType => write!(f, "invalid DEFLATE block type"),
            Self::InvalidStoredLength => write!(f, "stored DEFLATE block length check failed"),
            Self::InvalidHuffmanTable(reason) => write!(f, "invalid Huffman table: {reason}"),
            Self::InvalidSymbol => write!(f, "invalid Huffman symbol"),
            Self::DistanceTooFar => write!(f, "back-reference distance exceeds produced output"),
            Self::OutputLimitExceeded { limit } => {
                write!(f, "decompressed output exceeds the limit of {limit} bytes")
            }
            Self::InvalidGzipHeader(reason) => write!(f, "invalid gzip header: {reason}"),
            Self::InvalidZlibHeader(reason) => write!(f, "invalid zlib header: {reason}"),
            Self::ChecksumMismatch { kind, expected, actual } => {
                write!(f, "{kind} mismatch: expected {expected:#010x}, computed {actual:#010x}")
            }
            Self::LengthMismatch { expected, actual } => {
                write!(f, "length mismatch: trailer says {expected}, decoded {actual}")
            }
            Self::InvalidPng(reason) => write!(f, "invalid PNG: {reason}"),
            Self::UnsupportedPng(reason) => write!(f, "unsupported PNG: {reason}"),
            Self::ImageTooLarge { width, height } => {
                write!(f, "image dimensions {width}x{height} exceed the configured limit")
            }
            Self::Sink(reason) => write!(f, "output consumer failed: {reason}"),
        }
    }
}

impl std::error::Error for DecodeError {}
