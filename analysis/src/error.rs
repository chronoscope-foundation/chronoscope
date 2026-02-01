//! Error types for analysis operations.

use reqwest::StatusCode;
use thiserror::Error;

/// Errors that can occur during analysis.
#[derive(Debug, Error)]
pub enum AnalysisError {
    /// Failed to connect to Triton server
    #[error("connection error: {0}")]
    Connection(String),

    /// Triton returned an error response
    #[error("triton error ({status}): {message}")]
    Triton {
        /// HTTP status code from Triton
        status: StatusCode,
        /// Error message from response body
        message: String,
    },

    /// Failed to parse response
    #[error("response parsing error: {0}")]
    ResponseParsing(String),

    /// Invalid image data
    #[error("invalid image: {0}")]
    InvalidImage(String),

    /// Transport-level HTTP error (connection refused, timeout, TLS failure).
    /// Distinct from `Triton` which represents an error response from Triton.
    #[error("HTTP error: {0}")]
    Http(#[from] chronoscope_integrations::HttpError),
}

impl AnalysisError {
    /// Whether this error should be retried.
    ///
    /// Retriable: connection errors, transport failures, server errors (5xx), rate limits.
    /// Permanent: client errors (4xx), parsing errors, invalid images.
    #[must_use]
    pub fn is_retriable(&self) -> bool {
        match self {
            Self::Connection(_) | Self::Http(_) => true,
            Self::Triton { status, .. } => {
                status.is_server_error() || *status == StatusCode::TOO_MANY_REQUESTS
            }
            Self::ResponseParsing(_) | Self::InvalidImage(_) => false,
        }
    }
}
