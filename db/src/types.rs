//! Strongly-typed domain types to prevent mixing up IDs and other values.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

// Re-export ID types and enums from api-client for convenience.
pub use chronoscope_api_client::{
    AnnotationId, Email, EntityId, EntityLinkId, MediaId, MediaType, ResearchUrlId,
    ResearchUrlStatus, SourceId, UserId,
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

/// External ID source type for entity deduplication.
///
/// Must stay in sync with the CHECK constraint on `entity_external_ids.id_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, sqlx::Type, strum::Display, strum::AsRefStr)]
#[sqlx(type_name = "TEXT")]
#[strum(serialize_all = "snake_case")]
pub enum ExternalIdType {
    #[sqlx(rename = "wikidata")]
    Wikidata,
    #[sqlx(rename = "osm_node")]
    OsmNode,
    #[sqlx(rename = "osm_way")]
    OsmWay,
    #[sqlx(rename = "osm_relation")]
    OsmRelation,
    #[sqlx(rename = "geonames")]
    #[strum(serialize = "geonames")]
    GeoNames,
    #[sqlx(rename = "pleiades")]
    Pleiades,
    #[sqlx(rename = "getty_tgn")]
    GettyTgn,
    #[sqlx(rename = "nrhp")]
    Nrhp,
}

impl ExternalIdType {
    /// All variants, for exhaustive testing against DB CHECK constraints.
    #[must_use]
    pub fn all() -> &'static [Self] {
        &[
            Self::Wikidata,
            Self::OsmNode,
            Self::OsmWay,
            Self::OsmRelation,
            Self::GeoNames,
            Self::Pleiades,
            Self::GettyTgn,
            Self::Nrhp,
        ]
    }
}

/// Annotation kind discriminant as stored in the database.
///
/// This is the tag-only version of `chronoscope_core::annotation::AnnotationKind`
/// (which is a data-carrying enum). The full JSON lives in `kind_json`; this enum
/// maps the generated `kind` column used for indexing.
///
/// Mirrors the discriminant tag of `chronoscope_core::annotation::AnnotationKind`
/// with `sqlx::Type` support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, sqlx::Type, strum::Display)]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum AnnotationKindTag {
    SpatialTrace,
    ExteriorView,
    InteriorView,
    TextualNote,
}

impl AnnotationKindTag {
    /// All variants, for exhaustive testing against DB CHECK constraints.
    #[must_use]
    pub fn all() -> &'static [Self] {
        &[
            Self::SpatialTrace,
            Self::ExteriorView,
            Self::InteriorView,
            Self::TextualNote,
        ]
    }
}

/// Exhaustive match ensures adding a variant to
/// `chronoscope_core::annotation::AnnotationKind` forces a db-side update.
impl From<&chronoscope_core::annotation::AnnotationKind> for AnnotationKindTag {
    fn from(kind: &chronoscope_core::annotation::AnnotationKind) -> Self {
        match kind {
            chronoscope_core::annotation::AnnotationKind::SpatialTrace { .. } => Self::SpatialTrace,
            chronoscope_core::annotation::AnnotationKind::ExteriorView { .. } => Self::ExteriorView,
            chronoscope_core::annotation::AnnotationKind::InteriorView { .. } => Self::InteriorView,
            chronoscope_core::annotation::AnnotationKind::TextualNote { .. } => Self::TextualNote,
        }
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
