//! Engine error types with machine-readable codes (`E_*`) for retry policy
//! and UI surfacing.

use std::path::PathBuf;

use swiftfetch_store::StoreError;

/// Errors surfaced by the download engine.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// The server could not be probed (HEAD + range probe both failed).
    #[error("probe failed for {url}: {message}")]
    Probe {
        /// URL that failed to probe.
        url: String,
        /// Human-readable reason.
        message: String,
    },
    /// The server answered with an unexpected status.
    #[error("unexpected HTTP status {status} for {url}")]
    Http {
        /// URL that returned the status.
        url: String,
        /// Status code.
        status: u16,
    },
    /// A 206 response did not match the requested range.
    #[error("server range mismatch on {url}")]
    RangeMismatch {
        /// URL whose `Content-Range` mismatched.
        url: String,
    },
    /// The server does not support ranges (resume impossible).
    #[error("resume not supported by server")]
    NoResume,
    /// The completed file hash does not match the expected digest.
    #[error("checksum mismatch for {}", path.display())]
    ChecksumMismatch {
        /// File whose hash mismatched.
        path: PathBuf,
    },
    /// The completed file size does not match the expected size.
    #[error("size mismatch for {}: expected {expected}, got {actual}", path.display())]
    SizeMismatch {
        /// File whose size mismatched.
        path: PathBuf,
        /// Expected size in bytes.
        expected: u64,
        /// Actual size in bytes.
        actual: u64,
    },
    /// The destination file already exists and overwrite was not requested.
    #[error("destination already exists: {}", .0.display())]
    DestExists(PathBuf),
    /// The URL refresher could not produce a working URL in the allowed
    /// number of attempts.
    #[error("URL refresh attempts exhausted for {url}")]
    UrlRefreshExhausted {
        /// URL that kept failing.
        url: String,
    },
    /// Invalid engine/job configuration.
    #[error("invalid configuration: {0}")]
    Config(String),
    /// Filesystem error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// SQLite journal error.
    #[error(transparent)]
    Journal(#[from] StoreError),
    /// HTTP transport error (connection reset, timeout, TLS failure…).
    #[error(transparent)]
    Transport(#[from] reqwest::Error),
    /// The job was cancelled by the user.
    #[error("cancelled")]
    Cancelled,
}

impl EngineError {
    /// Machine-readable error code persisted in `downloads.error_code`.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Probe { .. } => "E_PROBE_FAILED",
            Self::Http { .. } => "E_HTTP_STATUS",
            Self::RangeMismatch { .. } => "E_RANGE_MISMATCH",
            Self::NoResume => "E_NO_RESUME",
            Self::ChecksumMismatch { .. } => "E_CHECKSUM_MISMATCH",
            Self::SizeMismatch { .. } => "E_SIZE_MISMATCH",
            Self::DestExists(_) => "E_DEST_EXISTS",
            Self::UrlRefreshExhausted { .. } => "E_URL_REFRESH_EXHAUSTED",
            Self::Config(_) => "E_CONFIG",
            Self::Io(_) => "E_IO",
            Self::Journal(_) => "E_JOURNAL",
            Self::Transport(_) => "E_TRANSPORT",
            Self::Cancelled => "E_CANCELLED",
        }
    }

    /// Whether refreshing the URL could plausibly fix this error (expired or
    /// forbidden links, dropped sessions).
    #[must_use]
    pub fn is_refresh_worthy(&self) -> bool {
        match self {
            Self::Http { status, .. } => matches!(status, 403 | 404 | 410),
            Self::Probe { .. } | Self::Transport(_) => true,
            _ => false,
        }
    }
}
