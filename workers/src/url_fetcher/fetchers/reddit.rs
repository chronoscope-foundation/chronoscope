//! Reddit-specific fetcher.
//!
//! Fetches Reddit posts and comments using Reddit's JSON API by appending `.json`
//! to any Reddit URL. Handles posts, comments, and galleries, extracting media URLs
//! for downstream processing.

use std::fmt::Write as _;

use chrono::{TimeZone, Utc};
use chronoscope_db::{MediaSlot, PageData, ResearchUrl, SourceType};
use serde::Deserialize;
use tracing::{debug, instrument};
use url::Url;

use crate::http::{HttpRequest, HttpResponse};
use crate::url_fetcher::fetcher::{FetchContext, FetchError, FetchOutcome, FetchResult, Fetcher};

/// Marker for deleted Reddit content.
const DELETED: &str = "[deleted]";
/// Marker for removed Reddit content.
const REMOVED: &str = "[removed]";

/// Reddit-specific fetcher.
///
/// Uses Reddit's JSON API by appending `.json` to URLs.
#[derive(Default)]
pub struct RedditFetcher;

impl RedditFetcher {
    /// Create a new Reddit fetcher.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Fetcher for RedditFetcher {
    fn domains(&self) -> &'static [&'static str] {
        &["reddit.com", "redd.it"]
    }

    #[instrument(skip(self, ctx, research_url), fields(url = %research_url.url))]
    async fn process(
        &self,
        ctx: &FetchContext,
        research_url: &ResearchUrl,
    ) -> Result<FetchResult, FetchError> {
        let url = Url::parse(&research_url.url)
            .map_err(|e| FetchError::ParseError(format!("invalid URL: {e}")))?;

        // Handle redd.it short links by resolving the redirect
        if url.host_str() == Some("redd.it") {
            return self.process_short_link(ctx, research_url, &url).await;
        }

        // Fetch JSON version of the URL
        let json_url = build_json_url(&url);
        let response = fetch_json(ctx, &json_url).await?;

        // Parse and store the post
        self.process_json_response(ctx, research_url, &response.body)
            .await
    }
}

impl RedditFetcher {
    /// Handle redd.it short links by following the redirect.
    async fn process_short_link(
        &self,
        ctx: &FetchContext,
        research_url: &ResearchUrl,
        url: &Url,
    ) -> Result<FetchResult, FetchError> {
        // Fetch the short link to get the redirect
        let request = HttpRequest::get(url.clone());
        let response = ctx
            .http
            .execute(request)
            .await
            .map_err(|e| FetchError::Http(e.to_string()))?;

        // The final URL after redirects should be the full Reddit URL
        if response.final_url.host_str() != Some("reddit.com")
            && response.final_url.host_str() != Some("www.reddit.com")
        {
            return Err(FetchError::ParseError(format!(
                "redd.it did not redirect to reddit.com: {}",
                response.final_url
            )));
        }

        // Fetch JSON for the resolved URL
        let json_url = build_json_url(&response.final_url);
        let json_response = fetch_json(ctx, &json_url).await?;

        // Parse and store the post
        self.process_json_response(ctx, research_url, &json_response.body)
            .await
    }

    /// Process JSON response body and store the page.
    async fn process_json_response(
        &self,
        ctx: &FetchContext,
        research_url: &ResearchUrl,
        body: &[u8],
    ) -> Result<FetchResult, FetchError> {
        let json: serde_json::Value = serde_json::from_slice(body)
            .map_err(|e| FetchError::ParseError(format!("invalid JSON: {e}")))?;

        let (post, comments) = extract_post_and_comments(&json)?;
        let (page_data, discovered_urls) = build_page_data(&post, &comments);

        let page_id = ctx.db.create_page(&page_data).await?;
        tracing::Span::current().record("page_id", page_id.to_string());

        ctx.db
            .mark_url_resolved_to_page(&research_url.id, &page_id)
            .await?;

        debug!(
            page_id = %page_id,
            discovered_count = discovered_urls.len(),
            "processed reddit post"
        );

        Ok(FetchResult {
            outcome: FetchOutcome::Page { page_id },
            discovered_urls,
        })
    }
}

/// Fetch JSON from a URL with proper error handling.
async fn fetch_json(ctx: &FetchContext, url: &Url) -> Result<HttpResponse, FetchError> {
    let request = HttpRequest::get(url.clone());
    let response = ctx
        .http
        .execute(request)
        .await
        .map_err(|e| FetchError::Http(e.to_string()))?;

    FetchError::from_status(response.status)?;
    Ok(response)
}

// ==================== JSON Parsing ====================

/// Reddit post data extracted from JSON.
#[derive(Debug, Deserialize)]
struct RedditPost {
    title: String,
    author: Option<String>,
    selftext: Option<String>,
    url: Option<String>,
    created_utc: Option<f64>,
    is_gallery: Option<bool>,
    gallery_data: Option<GalleryData>,
    media_metadata: Option<serde_json::Value>,
    preview: Option<Preview>,
    is_video: Option<bool>,
    media: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct GalleryData {
    items: Vec<GalleryItem>,
}

#[derive(Debug, Deserialize)]
struct GalleryItem {
    media_id: String,
}

#[derive(Debug, Deserialize)]
struct Preview {
    images: Vec<PreviewImage>,
}

#[derive(Debug, Deserialize)]
struct PreviewImage {
    source: ImageSource,
}

#[derive(Debug, Deserialize)]
struct ImageSource {
    url: String,
}

/// Build the JSON API URL by appending `.json`.
fn build_json_url(url: &Url) -> Url {
    let mut json_url = url.clone();

    // Get the path and ensure it ends with .json
    let path = json_url.path().to_string();
    let new_path = if path.ends_with('/') {
        format!("{}.json", path.trim_end_matches('/'))
    } else if path.ends_with(".json") {
        path
    } else {
        format!("{path}.json")
    };

    json_url.set_path(&new_path);

    // Clear query params (Reddit doesn't need them for JSON endpoint)
    json_url.set_query(None);

    json_url
}

/// A comment extracted from Reddit's JSON.
#[derive(Debug)]
struct RedditComment {
    author: String,
    body: String,
}

/// Extract the post data and comments from Reddit's listing JSON format.
fn extract_post_and_comments(
    json: &serde_json::Value,
) -> Result<(RedditPost, Vec<RedditComment>), FetchError> {
    // Reddit returns an array: [listing of post, listing of comments]
    let post_listing = if json.is_array() {
        json.get(0)
            .ok_or_else(|| FetchError::ParseError("empty response array".into()))?
    } else {
        json
    };

    // Get the first child (the post itself)
    let children = post_listing
        .get("data")
        .and_then(|d| d.get("children"))
        .and_then(|c| c.as_array())
        .ok_or_else(|| FetchError::ParseError("missing listing children".into()))?;

    let first_child = children
        .first()
        .ok_or_else(|| FetchError::ParseError("no posts in listing".into()))?;

    let post_data = first_child
        .get("data")
        .ok_or_else(|| FetchError::ParseError("missing post data".into()))?;

    let post: RedditPost = serde_json::from_value(post_data.clone())
        .map_err(|e| FetchError::ParseError(format!("failed to parse post: {e}")))?;

    // Extract comments from second listing
    let comments = json
        .get(1)
        .and_then(|c| c.get("data"))
        .and_then(|d| d.get("children"))
        .and_then(|c| c.as_array())
        .map(|children| extract_comments_recursive(children))
        .unwrap_or_default();

    Ok((post, comments))
}

/// Recursively extract comments from a Reddit comment tree.
fn extract_comments_recursive(children: &[serde_json::Value]) -> Vec<RedditComment> {
    let mut comments = Vec::new();

    for child in children {
        // Skip "more" links
        if child.get("kind").and_then(|k| k.as_str()) != Some("t1") {
            continue;
        }

        if let Some(data) = child.get("data") {
            let author = data
                .get("author")
                .and_then(|a| a.as_str())
                .unwrap_or(DELETED);
            let body = data.get("body").and_then(|b| b.as_str()).unwrap_or("");

            // Skip deleted/removed comments
            if author != DELETED && body != DELETED && body != REMOVED {
                comments.push(RedditComment {
                    author: author.to_string(),
                    body: body.to_string(),
                });
            }

            // Recurse into replies
            if let Some(replies) = data
                .get("replies")
                .and_then(|r| r.get("data"))
                .and_then(|d| d.get("children"))
                .and_then(|c| c.as_array())
            {
                comments.extend(extract_comments_recursive(replies));
            }
        }
    }

    comments
}

/// Build page data from the extracted post and comments.
fn build_page_data(post: &RedditPost, comments: &[RedditComment]) -> (PageData, Vec<Url>) {
    let mut discovered_urls = Vec::new();

    // Extract selftext content
    let selftext = post
        .selftext
        .as_ref()
        .filter(|s| !s.is_empty() && *s != REMOVED && *s != DELETED)
        .cloned();

    // Handle gallery posts
    if post.is_gallery == Some(true) {
        if let (Some(gallery_data), Some(media_metadata)) =
            (&post.gallery_data, &post.media_metadata)
        {
            for item in &gallery_data.items {
                if let Some(url) = media_metadata
                    .get(&item.media_id)
                    .and_then(extract_gallery_image_url)
                {
                    discovered_urls.push(url);
                }
            }
        }
    }
    // Handle video posts (v.redd.it)
    else if post.is_video == Some(true) {
        if let Some(video_url) = extract_video_url(post.media.as_ref()) {
            discovered_urls.push(video_url);
        }
    }
    // Handle image posts (direct link to image)
    else if let Some(ref url_str) = post.url
        && is_image_url(url_str)
        && let Ok(url) = Url::parse(url_str)
    {
        discovered_urls.push(url);
    }

    // Also check preview images as fallback
    if discovered_urls.is_empty()
        && let Some(ref preview) = post.preview
    {
        for image in &preview.images {
            // Reddit HTML-encodes the URL in preview.images
            let decoded_url = html_decode(&image.source.url);
            if let Ok(url) = Url::parse(&decoded_url) {
                discovered_urls.push(url);
                break; // Just take the first preview image
            }
        }
    }

    // Parse created_utc to NaiveDateTime
    // Unix timestamps always fit in i64, so truncation is safe
    #[allow(clippy::cast_possible_truncation)]
    let published_at = post.created_utc.and_then(|ts| {
        Utc.timestamp_opt(ts as i64, 0)
            .single()
            .map(|dt| dt.naive_utc())
    });

    // Format author
    let author = post.author.as_ref().and_then(|a| {
        if a == DELETED {
            None
        } else {
            Some(format!("u/{a}"))
        }
    });

    // Build content: selftext + link (if external) + comments
    let mut content_parts = Vec::new();

    // Add selftext
    if let Some(ref text) = selftext {
        content_parts.push(text.clone());
    }

    // Add external link if present
    if let Some(ref url) = post.url
        && !is_reddit_url(url)
        && !is_image_url(url)
    {
        content_parts.push(format!("[Link]({url})"));
    }

    // Add comments section
    if !comments.is_empty() {
        let mut comments_md = String::from("\n\n---\n\n## Comments\n\n");
        for comment in comments {
            // write! to String is infallible, ignore result
            let _ = write!(
                comments_md,
                "**u/{}**:\n\n{}\n\n",
                comment.author, comment.body
            );
        }
        content_parts.push(comments_md);
    }

    let full_content = if content_parts.is_empty() {
        None
    } else {
        Some(content_parts.join("\n\n"))
    };

    let page_data = PageData {
        source_type: SourceType::Reddit,
        title: Some(post.title.clone()),
        author,
        published_at,
        content: full_content,
        fetched_at: Utc::now().naive_utc(),
        media: discovered_urls
            .iter()
            .map(|url| MediaSlot::pending(url.as_str()))
            .collect(),
    };

    debug!(
        title = %post.title,
        media_count = discovered_urls.len(),
        comment_count = comments.len(),
        "built page data"
    );

    (page_data, discovered_urls)
}

/// Extract the best quality image URL from gallery media metadata.
fn extract_gallery_image_url(media: &serde_json::Value) -> Option<Url> {
    // Try to get the source image (highest quality)
    // Gallery items have structure: { "s": { "u": "url" }, "p": [...previews] }
    let url_str = media
        .get("s")
        .and_then(|s| s.get("u"))
        .and_then(|u| u.as_str())
        .map(html_decode)?;

    Url::parse(&url_str).ok()
}

/// Extract video URL from Reddit video media.
fn extract_video_url(media: Option<&serde_json::Value>) -> Option<Url> {
    // Reddit video structure: { "reddit_video": { "fallback_url": "...", "dash_url": "..." } }
    // fallback_url is a direct MP4 URL, dash_url requires DASH parsing
    let reddit_video = media?.get("reddit_video")?;

    // Prefer fallback_url as it's a direct MP4
    let url_str = reddit_video
        .get("fallback_url")
        .and_then(|u| u.as_str())
        .map(html_decode)?;

    Url::parse(&url_str).ok()
}

/// Check if a URL points to an image.
// URL is lowercased before comparison, so case sensitivity is not an issue.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn is_image_url(url: &str) -> bool {
    let lower = url.to_lowercase();
    lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".png")
        || lower.ends_with(".gif")
        || lower.ends_with(".webp")
        || lower.contains("i.redd.it")
        || lower.contains("i.imgur.com")
}

/// Check if a URL is a Reddit URL (to avoid storing reddit links as content).
fn is_reddit_url(url: &str) -> bool {
    url.contains("reddit.com") || url.contains("redd.it")
}

/// Decode HTML entities in a string.
///
/// This handles only the entities that Reddit's JSON API actually produces
/// in URL fields (`&amp;`, `&lt;`, `&gt;`, `&quot;`). Numeric entities like
/// `&#39;` are not used by Reddit in these contexts and are left as-is.
fn html_decode(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::url_fetcher::test_harness::{TestHarness, TestResult};
    use chronoscope_db::SourceType;

    // ==================== Unit Tests ====================

    #[test]
    fn test_build_json_url_simple_path() -> TestResult {
        let url = Url::parse("https://reddit.com/r/rust/comments/abc123/some_title")?;
        let json_url = build_json_url(&url);
        assert_eq!(
            json_url.as_str(),
            "https://reddit.com/r/rust/comments/abc123/some_title.json"
        );
        Ok(())
    }

    #[test]
    fn test_build_json_url_trailing_slash() -> TestResult {
        let url = Url::parse("https://reddit.com/r/rust/comments/abc123/")?;
        let json_url = build_json_url(&url);
        assert_eq!(
            json_url.as_str(),
            "https://reddit.com/r/rust/comments/abc123.json"
        );
        Ok(())
    }

    #[test]
    fn test_build_json_url_already_json() -> TestResult {
        let url = Url::parse("https://reddit.com/r/rust/comments/abc123.json")?;
        let json_url = build_json_url(&url);
        assert_eq!(
            json_url.as_str(),
            "https://reddit.com/r/rust/comments/abc123.json"
        );
        Ok(())
    }

    #[test]
    fn test_build_json_url_strips_query_params() -> TestResult {
        let url = Url::parse("https://reddit.com/r/rust/comments/abc123?utm_source=share")?;
        let json_url = build_json_url(&url);
        assert_eq!(
            json_url.as_str(),
            "https://reddit.com/r/rust/comments/abc123.json"
        );
        Ok(())
    }

    #[test]
    fn test_is_image_url() {
        assert!(is_image_url("https://i.redd.it/abc123.jpg"));
        assert!(is_image_url("https://i.imgur.com/abc.png"));
        assert!(is_image_url("https://example.com/photo.jpeg"));
        assert!(is_image_url("https://example.com/image.gif"));
        assert!(!is_image_url("https://reddit.com/r/rust"));
        assert!(!is_image_url("https://example.com/page.html"));
    }

    #[test]
    fn test_html_decode() {
        assert_eq!(html_decode("a&amp;b"), "a&b");
        assert_eq!(html_decode("&lt;tag&gt;"), "<tag>");
        assert_eq!(
            html_decode("https://example.com?a=1&amp;b=2"),
            "https://example.com?a=1&b=2"
        );
    }

    #[test]
    fn test_extract_post_and_comments() -> TestResult {
        let json: serde_json::Value = serde_json::json!([
            {
                "kind": "Listing",
                "data": {
                    "children": [
                        {
                            "kind": "t3",
                            "data": {
                                "title": "Test Post",
                                "author": "testuser",
                                "selftext": "Hello world",
                                "created_utc": 1700000000.0
                            }
                        }
                    ]
                }
            },
            {
                "kind": "Listing",
                "data": {
                    "children": [
                        {
                            "kind": "t1",
                            "data": {
                                "author": "commenter1",
                                "body": "Great post!"
                            }
                        },
                        {
                            "kind": "t1",
                            "data": {
                                "author": "commenter2",
                                "body": "I agree"
                            }
                        }
                    ]
                }
            }
        ]);

        let (post, comments) = extract_post_and_comments(&json)?;
        assert_eq!(post.title, "Test Post");
        assert_eq!(post.author, Some("testuser".to_string()));
        assert_eq!(post.selftext, Some("Hello world".to_string()));
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].author, "commenter1");
        assert_eq!(comments[0].body, "Great post!");
        assert_eq!(comments[1].author, "commenter2");
        Ok(())
    }

    // ==================== Integration Tests ====================
    //
    // Run with: cargo test -p chronoscope-workers --features record-fixtures
    // to record new fixtures.

    /// Single image post with deleted author - tests basic Reddit JSON parsing
    #[tokio::test]
    async fn test_reddit_image_post() -> TestResult {
        let url = "https://www.reddit.com/r/AbandonedPorn/comments/1jaqh6g/aerial_view_the_abandoned_methodist_church/";

        let fetched = TestHarness::new().await?.fetch_page(url).await?;

        assert_eq!(fetched.page.data.source_type, SourceType::Reddit);

        // Exact title
        assert_eq!(
            fetched.page.data.title.as_deref(),
            Some("Aerial view the abandoned Methodist Church located in Gary Indiana")
        );

        // Author was deleted
        assert_eq!(fetched.page.data.author, None);

        let content = fetched.page.data.content.as_deref().unwrap_or("");

        // Body text includes post selftext
        assert!(content.contains("absolutely massive"), "missing selftext");
        assert!(
            content.contains("downtown Gary Indiana"),
            "missing selftext"
        );
        assert!(
            content.contains("largest Methodist Church"),
            "missing selftext"
        );

        // Comments section header
        assert!(content.contains("## Comments"), "missing comments section");

        // Specific comments with author attribution
        assert!(content.contains("**u/Rusty3414**"), "missing commenter");
        assert!(content.contains("Sad"), "missing comment text");
        assert!(content.contains("**u/imameanone**"), "missing commenter");
        assert!(content.contains("Oh, Gary!"), "missing comment text");
        assert!(content.contains("**u/TCSpeedy**"), "missing commenter");
        assert!(
            content.contains("industrious graffiti"),
            "missing comment text"
        );

        // Exact image URL discovered
        assert_eq!(fetched.discovered_urls.len(), 1);
        assert_eq!(
            fetched.discovered_urls[0].as_str(),
            "https://i.redd.it/c0v4utg6ojoe1.jpeg"
        );

        Ok(())
    }

    /// Gallery post with 20 images - tests gallery extraction and author parsing
    #[tokio::test]
    async fn test_reddit_gallery_post() -> TestResult {
        let url = "https://www.reddit.com/r/abandoned/comments/1qcyfyj/bolshoye_selo/";

        let fetched = TestHarness::new().await?.fetch_page(url).await?;

        assert_eq!(fetched.page.data.source_type, SourceType::Reddit);

        // Exact title
        assert_eq!(fetched.page.data.title.as_deref(), Some("Bolshoye Selo"));

        // Author with u/ prefix
        assert_eq!(fetched.page.data.author.as_deref(), Some("u/ferzunkin"));

        let content = fetched.page.data.content.as_deref().unwrap_or("");

        // Body text includes location info
        assert!(content.contains("Pronsk district"), "missing location");
        assert!(content.contains("Ryazan region"), "missing location");
        assert!(content.contains("Russia"), "missing location");

        // Comments section with Spanish comment
        assert!(content.contains("## Comments"), "missing comments section");
        assert!(
            content.contains("**u/Free-Outcome2922**"),
            "missing commenter"
        );
        assert!(
            content.contains("La foto 13 es una joya"),
            "missing comment text"
        );

        // All 20 gallery images discovered
        assert_eq!(fetched.discovered_urls.len(), 20);

        // First image URL - exact URL after HTML decoding
        assert_eq!(
            fetched.discovered_urls[0].as_str(),
            "https://preview.redd.it/yqu901j9jddg1.jpg?width=3504&format=pjpg&auto=webp&s=3811410cf9613a933d5d958ab56f2c049d94c2cc"
        );

        // All URLs are from preview.redd.it (gallery images)
        for url in &fetched.discovered_urls {
            assert!(
                url.as_str().starts_with("https://preview.redd.it/"),
                "unexpected host: {url}"
            );
        }

        Ok(())
    }

    /// Shortlink (redd.it) resolution - follows redirect then fetches JSON
    #[tokio::test]
    async fn test_reddit_shortlink_resolves() -> TestResult {
        let url = "https://redd.it/16abzpl";
        let fetched = TestHarness::new().await?.fetch_page(url).await?;

        assert_eq!(fetched.page.data.source_type, SourceType::Reddit);

        // Exact title - shortlink should resolve to this specific post
        assert_eq!(
            fetched.page.data.title.as_deref(),
            Some("Mining ruins in CO")
        );

        Ok(())
    }

    /// Reddit CDN subdomains should NOT match the registered `redd.it` shortlink domain.
    ///
    /// This verifies that `preview.redd.it`, `i.redd.it`, and `v.redd.it` (CDN subdomains)
    /// fall through to the generic fetcher rather than being routed to RedditFetcher.
    #[test]
    fn test_reddit_cdn_does_not_match_shortlink_domain() -> TestResult {
        use crate::url_fetcher::{FetcherRegistry, GenericFetcher, RedditFetcher};
        use std::sync::Arc;

        let generic: Arc<dyn Fetcher> = Arc::new(GenericFetcher::new());
        let mut registry = FetcherRegistry::new(generic);
        registry.register(Arc::new(RedditFetcher::new()));

        // Shortlink domain should route to Reddit fetcher
        let shortlink = Url::parse("https://redd.it/abc123")?;
        assert!(
            registry.get(&shortlink).domains().contains(&"reddit.com"),
            "redd.it shortlinks should use RedditFetcher"
        );

        // CDN subdomains should NOT match - they fall through to generic
        for cdn_url in [
            "https://i.redd.it/abc123.jpg",
            "https://preview.redd.it/abc123.jpg?width=1024",
            "https://v.redd.it/abc123/DASH_720.mp4",
        ] {
            let url = Url::parse(cdn_url)?;
            assert!(
                registry.get(&url).domains().is_empty(),
                "{cdn_url} should fall through to generic fetcher"
            );
        }

        Ok(())
    }
}
