//! Dossier types for research URL summaries and detailed views.
//!
//! A "dossier" is the assembled information about a research URL, including
//! the resolved content (page or media), extracted metadata, and analysis results.

use chrono::NaiveDateTime;
use chronoscope_core::{UncertainDate, UncertainLocation};
use chronoscope_db::{
    FollowedUrl, MediaId, MediaType, ResearchUrl, ResearchUrlId, ResearchUrlStatus, SourceType,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// Re-export analysis types for API consumers
pub use chronoscope_analysis::AnalysisResult;

// ==================== Analysis Outcome (Generic) ====================

/// Outcome of an analysis stage.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AnalysisOutcome<T> {
    /// Analysis not yet attempted
    #[default]
    Pending,
    /// Analysis currently running
    InProgress,
    /// Analysis completed successfully
    Success(T),
    /// Analysis failed
    Failed { error: String },
}

// ==================== Analysis Result Types ====================

/// Reverse image search results.
///
/// Will contain: matching images found across the web, higher-resolution
/// versions, and source attribution for provenance tracking.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReverseImageSearchResults {}

/// Deep research synthesis results.
///
/// Will contain: synthesized date estimates, location hypotheses,
/// historical narrative, and confidence scores combining all analysis sources.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DeepResearchResults {}

// ==================== Analysis Progress ====================

/// Per-media analysis stages.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct MediaAnalysis {
    /// Image analysis (segmentation + VLM + embeddings).
    pub analysis: AnalysisOutcome<AnalysisResult>,
    pub reverse_image_search: AnalysisOutcome<ReverseImageSearchResults>,
}

/// Rollup counts across all media items for a URL.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct MediaAnalysisCounts {
    /// Total media items referenced
    pub total: u32,
    /// Media items successfully fetched
    pub fetched: u32,
    /// Media items with completed analysis
    pub analyzed: u32,
    pub reverse_image_search: u32,
}

/// URL-level analysis progress.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct UrlAnalysis {
    /// Rollup of per-media analysis stages
    pub media: MediaAnalysisCounts,
    /// Deep research results
    pub deep_research: AnalysisOutcome<DeepResearchResults>,
}

// ==================== Summary Types (for list endpoints) ====================

/// Summary of a research URL for list views.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ResearchUrlSummary {
    pub id: ResearchUrlId,
    pub url: String,
    pub status: ResearchUrlStatus,
    /// Best available summary for list display.
    /// Initially derived from URL parsing, upgraded as analysis extracts
    /// meaningful context (dates, locations, titles).
    pub summary: String,
    /// Thumbnail URL (CDN path), if available
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail_url: Option<String>,
    /// Analysis progress
    pub analysis: UrlAnalysis,
    /// When this URL was submitted
    pub created_at: NaiveDateTime,
}

/// Summary of a followed research URL (includes follow timestamp).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FollowedUrlSummary {
    pub research_url: ResearchUrlSummary,
    pub followed_at: NaiveDateTime,
}

// ==================== DB Conversion Traits ====================

impl From<ResearchUrl> for ResearchUrlSummary {
    fn from(u: ResearchUrl) -> Self {
        // TODO: Implement smart summary extraction from URL and resolved content
        let summary = u.url.clone();

        ResearchUrlSummary {
            id: u.id,
            summary,
            url: u.url,
            status: u.status,
            thumbnail_url: None,
            analysis: UrlAnalysis::default(),
            created_at: u.created_at,
        }
    }
}

impl From<FollowedUrl> for FollowedUrlSummary {
    fn from(u: FollowedUrl) -> Self {
        FollowedUrlSummary {
            research_url: u.research_url.into(),
            followed_at: u.followed_at,
        }
    }
}

// ==================== Dossier Types (for detail view) ====================

/// Full dossier for a research URL.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ResearchUrlDossier {
    pub id: ResearchUrlId,
    pub url: String,
    pub status: ResearchUrlStatus,
    pub created_at: NaiveDateTime,
    pub analysis: UrlAnalysis,
    /// Resolved content (None while pending)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved: Option<ResolvedContent>,
}

/// Resolved content - either a page with embedded media, or direct media.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResolvedContent {
    Page(PageDossier),
    Media(Box<MediaDossier>),
}

/// Page content (Instagram post, Reddit thread, etc.).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PageDossier {
    pub source_type: SourceType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published: Option<UncertainDate>,
    /// Content as markdown (includes comments under heading)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// All media referenced by this page (in source order)
    pub media: Vec<MediaReference>,
    pub fetched_at: NaiveDateTime,
}

/// A media item referenced by a page - may or may not be fetched yet.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum MediaReference {
    /// Media is still being fetched
    Pending { source_url: String },
    /// Media has been fetched and processed
    Fetched(Box<MediaDossier>),
}

/// A fetched media item with full details.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MediaDossier {
    pub id: MediaId,
    pub media_type: MediaType,
    pub width: u32,
    pub height: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f32>,
    pub thumbnail_url: String,
    pub full_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub captured: Option<UncertainDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<UncertainLocation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_metadata: Option<serde_json::Value>,
    pub fetched_at: NaiveDateTime,
    pub analysis: MediaAnalysis,
}
