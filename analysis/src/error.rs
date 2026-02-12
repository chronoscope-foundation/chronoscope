//! Error types for analysis operations.

use thiserror::Error;

/// Errors that can occur during analysis.
#[derive(Debug, Clone, Error)]
pub enum AnalysisError {
    /// Failed to connect to Triton server or gRPC transport error.
    #[error("transport error: {0}")]
    Transport(String),

    /// Triton returned an error response.
    #[error("triton error (retriable={retriable}): {message}")]
    Triton {
        /// Whether this error is worth retrying.
        retriable: bool,
        /// Error message from the server.
        message: String,
    },

    /// Failed to parse response.
    #[error("response parsing error: {0}")]
    ResponseParsing(String),

    /// Invalid image data.
    #[error("invalid image: {0}")]
    InvalidImage(String),
}

impl AnalysisError {
    /// Whether this error should be retried.
    ///
    /// Retriable: transport errors, server errors (Unavailable, Internal, etc.).
    /// Permanent: parsing errors, invalid images, client errors (`InvalidArgument`, etc.).
    #[must_use]
    pub fn is_retriable(&self) -> bool {
        match self {
            Self::Transport(_) => true,
            Self::Triton { retriable, .. } => *retriable,
            Self::ResponseParsing(_) | Self::InvalidImage(_) => false,
        }
    }
}
