//! Error types for analysis worker.

use thiserror::Error;

/// Errors that can occur during analysis processing.
#[derive(Debug, Error)]
pub enum AnalysisError {
    /// Failed to fetch media from storage.
    #[error("storage error: {0}")]
    Storage(String),

    /// Image analysis failed.
    #[error("analysis error: {0}")]
    Analysis(#[from] chronoscope_analysis::AnalysisError),

    /// Database operation failed.
    #[error("database error: {0}")]
    Database(#[from] chronoscope_db::DbError),

    /// Failed to serialize results.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

impl AnalysisError {
    /// Whether this error should be retried.
    #[must_use]
    pub fn is_retriable(&self) -> bool {
        match self {
            Self::Storage(_) => true, // Storage might be temporarily unavailable
            Self::Analysis(e) => e.is_retriable(),
            Self::Database(_) => true, // DB errors might be transient
            Self::Serialization(_) => false, // Serialization errors are permanent
        }
    }
}
