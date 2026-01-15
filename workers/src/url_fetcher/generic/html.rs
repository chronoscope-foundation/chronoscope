//! HTML content processing.
//!
//! Extracts article content using readability-js, converts to markdown,
//! and discovers embedded images.

use bytes::Bytes;
use chrono::{NaiveDateTime, Utc};
use chronoscope_db::{MediaSlot, PageData, ResearchUrl, SourceType};
use scraper::{Html, Selector};
use tracing::{instrument, warn};
use url::Url;

use crate::url_fetcher::fetcher::{FetchContext, FetchError, FetchOutcome, FetchResult};

/// Extracted article data (Send-safe).
pub struct ExtractedArticle {
    pub title: String,
    pub byline: Option<String>,
    pub content: String,
    pub published_time: Option<String>,
}

/// Extract article content using readability-js.
///
/// Separated into sync function to avoid Send issues with `QuickJS` runtime.
pub fn extract_article(html: &str, base_url: &str) -> Result<ExtractedArticle, FetchError> {
    let readability = readability_js::Readability::new()
        .map_err(|e| FetchError::ParseError(format!("failed to create readability: {e}")))?;
    let article = readability
        .parse_with_url(html, base_url)
        .map_err(|e| FetchError::ParseError(format!("readability failed: {e}")))?;

    Ok(ExtractedArticle {
        title: article.title,
        byline: article.byline,
        content: article.content,
        published_time: article.published_time,
    })
}

/// Process HTML content: extract with readability, convert to markdown, discover images.
#[instrument(skip_all, fields(page_id))]
pub async fn process(
    ctx: &FetchContext,
    research_url: &ResearchUrl,
    final_url: &Url,
    body: &Bytes,
) -> Result<FetchResult, FetchError> {
    let html_str = String::from_utf8_lossy(body);

    // Extract article content (sync to avoid Send issues with QuickJS)
    let article = extract_article(&html_str, final_url.as_str())?;

    // Extract images from the clean HTML content
    let discovered_urls = extract_images(&article.content, final_url);

    // Convert HTML content to markdown
    let markdown = html2md::parse_html(&article.content);

    // Parse published time if available
    let published_at = article
        .published_time
        .as_ref()
        .and_then(|t| parse_published_time(t));

    // Create page data
    let page_data = PageData {
        source_type: SourceType::Generic,
        title: Some(article.title),
        author: article.byline,
        published_at,
        content: Some(markdown),
        fetched_at: Utc::now().naive_utc(),
        media: discovered_urls
            .iter()
            .map(|url| MediaSlot::pending(url.as_str()))
            .collect(),
    };

    // Store page in database
    let page_id = ctx.db.create_page(&page_data).await?;

    // Record page_id in the current span
    tracing::Span::current().record("page_id", page_id.to_string());

    // Mark URL as resolved
    ctx.db
        .mark_url_resolved_to_page(&research_url.id, &page_id)
        .await?;

    Ok(FetchResult {
        outcome: FetchOutcome::Page { page_id },
        discovered_urls,
    })
}

/// Extract image URLs from HTML content.
fn extract_images(html: &str, base_url: &Url) -> Vec<Url> {
    let document = Html::parse_fragment(html);

    let selector = Selector::parse("img[src]").ok();
    let Some(selector) = selector else {
        return vec![];
    };

    let mut urls = Vec::new();
    for element in document.select(&selector) {
        if let Some(src) = element.value().attr("src") {
            // TODO: Consider decoding data: URLs instead of skipping them
            if src.starts_with("data:") {
                continue;
            }

            // Resolve relative URLs
            match base_url.join(src) {
                Ok(url) => urls.push(url),
                Err(e) => {
                    warn!(src = src, error = %e, "failed to parse image URL");
                }
            }
        }
    }

    urls
}

/// Try to parse a published time string into a [`NaiveDateTime`].
fn parse_published_time(time_str: &str) -> Option<NaiveDateTime> {
    // Try ISO 8601 format first (most common)
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(time_str) {
        return Some(dt.naive_utc());
    }

    // Try RFC 2822 format
    if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(time_str) {
        return Some(dt.naive_utc());
    }

    // Try parsing as naive datetime without timezone
    if let Ok(dt) = NaiveDateTime::parse_from_str(time_str, "%Y-%m-%dT%H:%M:%S") {
        return Some(dt);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    #[test]
    fn test_extract_images_absolute_urls() -> TestResult {
        let html = r#"<div><img src="https://example.com/image.jpg"></div>"#;
        let base = Url::parse("https://example.com/page")?;

        let urls = extract_images(html, &base);

        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].as_str(), "https://example.com/image.jpg");
        Ok(())
    }

    #[test]
    fn test_extract_images_relative_urls() -> TestResult {
        let html = r#"<div><img src="/images/photo.png"></div>"#;
        let base = Url::parse("https://example.com/articles/page")?;

        let urls = extract_images(html, &base);

        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].as_str(), "https://example.com/images/photo.png");
        Ok(())
    }

    #[test]
    fn test_extract_images_multiple() -> TestResult {
        let html = r#"
            <div>
                <img src="a.jpg">
                <img src="b.png">
                <img src="https://other.com/c.gif">
            </div>
        "#;
        let base = Url::parse("https://example.com/")?;

        let urls = extract_images(html, &base);

        assert_eq!(urls.len(), 3);
        Ok(())
    }

    #[test]
    fn test_parse_published_time_rfc3339() {
        let result = parse_published_time("2024-03-15T10:30:00Z");
        assert!(result.is_some());
        let dt = result.as_ref().map(|d| d.and_utc());
        assert_eq!(dt.map(|d| d.year()), Some(2024));
        assert_eq!(dt.map(|d| d.month()), Some(3));
    }

    #[test]
    fn test_parse_published_time_invalid() {
        let result = parse_published_time("not a date");
        assert!(result.is_none());
    }
}
