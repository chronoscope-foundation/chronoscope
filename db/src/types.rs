//! Strongly-typed domain types to prevent mixing up IDs and other values.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

// Re-export ID types and enums from api-client for convenience.
pub use chronoscope_api_client::{
    Email, MediaId, MediaType, ResearchUrlId, ResearchUrlStatus, UserId,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, sqlx::Type)]
#[serde(transparent)]
#[sqlx(transparent)]
pub struct PageId(String);

impl PageId {
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
    #[must_use]
    pub fn generate() -> Self {
        Self(uuid::Uuid::now_v7().to_string())
    }
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl AsRef<str> for PageId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Status of media analysis in the processing pipeline.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
    sqlx::Type,
    strum::Display,
    strum::AsRefStr,
)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum AnalysisStatus {
    /// Waiting to be analyzed
    Pending,
    /// Analysis in progress
    Processing,
    /// Analysis completed successfully
    Complete,
    /// Analysis failed (see `analysis_error`)
    Failed,
}

/// Analysis state with associated data - enforces valid state combinations.
///
/// Unlike the flat DB representation (status + optional fields), this enum
/// guarantees that Complete always has results and Failed always has an error.
#[derive(Debug, Clone)]
pub enum MediaAnalysisState {
    /// Waiting to be analyzed
    Pending,
    /// Analysis in progress
    Processing,
    /// Analysis completed successfully with results
    Complete {
        /// Full analysis result (JSON-serialized `AnalysisResult`).
        analysis_result: String,
    },
    /// Analysis failed with an error message
    Failed {
        /// Error description
        error: String,
    },
}

impl MediaAnalysisState {
    /// Get the status enum value (for display/logging).
    #[must_use]
    pub fn status(&self) -> AnalysisStatus {
        match self {
            Self::Pending => AnalysisStatus::Pending,
            Self::Processing => AnalysisStatus::Processing,
            Self::Complete { .. } => AnalysisStatus::Complete,
            Self::Failed { .. } => AnalysisStatus::Failed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_id_generate_is_valid_uuid() {
        let id = UserId::generate();
        // UUIDv7 format: 8-4-4-4-12 hex chars with dashes = 36 chars
        assert_eq!(id.as_str().len(), 36);
        assert!(uuid::Uuid::parse_str(id.as_str()).is_ok());
    }

    #[test]
    fn research_url_status_as_str_matches_serde() -> Result<(), serde_json::Error> {
        for status in [
            ResearchUrlStatus::Pending,
            ResearchUrlStatus::Processing,
            ResearchUrlStatus::Complete,
            ResearchUrlStatus::Failed,
        ] {
            let json = serde_json::to_string(&status)?;
            assert_eq!(json, format!("\"{}\"", status.as_ref()));
        }
        Ok(())
    }
}
