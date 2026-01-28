//! Error types for analysis operations.

use thiserror::Error;

/// Errors that can occur during analysis.
#[derive(Debug, Error)]
pub enum AnalysisError {
    /// Failed to connect to Triton server
    #[error("connection error: {0}")]
    Connection(String),

    /// Triton returned an error response
    #[error("triton error: {0}")]
    Triton(String),

    /// Failed to parse response
    #[error("response parsing error: {0}")]
    ResponseParsing(String),

    /// Invalid image data
    #[error("invalid image: {0}")]
    InvalidImage(String),

    /// HTTP error
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
}
