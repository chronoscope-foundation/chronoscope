//! Content types returned by integrations.
//!
//! [`FetchedContent`] is the common result type for all integrations, containing
//! structured content extracted from a source.

use chrono::NaiveDateTime;
use url::Url;

/// Content fetched from a source - pure data, no DB types.
///
/// Note: This struct intentionally does not include `source_type`. The source type
/// is determined by which integration produced the content, not self-reported by
/// the content itself. This prevents integrations from misrepresenting their origin.
#[derive(Debug, Clone, Default)]
pub struct FetchedContent {
    /// Title of the content (e.g., post title, page title).
    pub title: Option<String>,

    /// Author of the content (e.g., username, byline).
    pub author: Option<String>,

    /// When the content was originally published.
    pub published_at: Option<NaiveDateTime>,

    /// Main content body as Markdown.
    pub content: Option<String>,

    /// Media URLs discovered in the content (images, videos).
    /// Order is preserved from the source.
    pub media: Vec<Url>,

    /// Additional URLs discovered that should be queued for fetching.
    /// These are URLs linked from the content that aren't direct media.
    pub discovered_urls: Vec<Url>,
}
