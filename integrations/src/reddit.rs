//! Reddit-specific integration.
//!
//! Fetches Reddit posts and comments using Reddit's JSON API by appending `.json`
//! to any Reddit URL. Handles posts, comments, and galleries, extracting media URLs
//! for downstream processing.

use std::fmt::Write as _;

use chrono::{TimeZone, Utc};
use serde::Deserialize;
use tracing::{debug, instrument};
use url::Url;

use crate::content::FetchedContent;
use crate::http::{HttpClient, HttpRequest, HttpResponse};
use crate::{FetchError, IntegrationMeta, IntegrationName, SingleFetcher};

/// Marker for deleted Reddit content.
const DELETED: &str = "[deleted]";
/// Marker for removed Reddit content.
const REMOVED: &str = "[removed]";

/// Reddit-specific integration.
///
/// Uses Reddit's JSON API by appending `.json` to URLs.
#[derive(Default)]
pub struct RedditIntegration;

impl RedditIntegration {
    /// Create a new Reddit integration.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// Reddit subdomains that should be normalized to canonical `reddit.com`.
const REDDIT_SUBDOMAINS: &[&str] = &[
    "www.reddit.com",
    "old.reddit.com",
    "new.reddit.com",
    "m.reddit.com",
    "i.reddit.com",
    "np.reddit.com",
    "amp.reddit.com",
];

/// Reddit-specific tracking parameters to remove during normalization.
const REDDIT_TRACKING_PARAMS: &[&str] = &["share_id", "ref_source"];

impl IntegrationMeta for RedditIntegration {
    fn name(&self) -> IntegrationName {
        IntegrationName::Reddit
    }

    fn domains(&self) -> &'static [&'static str] {
        &["reddit.com", "redd.it"]
    }

    fn normalize_url(&self, url: &Url) -> Url {
        let mut url = url.clone();

        // Normalize Reddit subdomains to canonical reddit.com
        if let Some(host) = url.host_str() {
            let host_lower = host.to_lowercase();
            if REDDIT_SUBDOMAINS.contains(&host_lower.as_str()) {
                // Ignore error - if set_host fails, we just keep the original
                let _ = url.set_host(Some("reddit.com"));
            }
        }

        // Remove Reddit-specific tracking parameters
        let keep_pairs: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(key, _)| {
                let key_lower = key.to_lowercase();
                !REDDIT_TRACKING_PARAMS.contains(&key_lower.as_str())
            })
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();

        {
            let mut query_mut = url.query_pairs_mut();
            query_mut.clear();
            for (k, v) in keep_pairs {
                query_mut.append_pair(&k, &v);
            }
        }

        // If query is now empty, remove it entirely
        if url.query() == Some("") {
            url.set_query(None);
        }

        url
    }
}

#[async_trait::async_trait]
impl SingleFetcher for RedditIntegration {
    #[instrument(skip(self, http), fields(url = %url))]
    async fn fetch(&self, http: &dyn HttpClient, url: &Url) -> Result<FetchedContent, FetchError> {
        // Resolve redirects for short links (redd.it) and share URLs (/r/{sub}/s/{id})
        let resolved_url = if url.host_str() == Some("redd.it") || is_share_url(url) {
            self.resolve_redirect(http, url).await?
        } else {
            url.clone()
        };

        // Fetch JSON version of the URL
        let json_url = build_json_url(&resolved_url);
        let response = fetch_json(http, &json_url).await?;

        // Parse and return the content
        Self::parse_json_response(&response.body)
    }
}

impl RedditIntegration {
    /// Resolve a redirect URL (redd.it short link or share URL) to the canonical Reddit URL.
    async fn resolve_redirect(&self, http: &dyn HttpClient, url: &Url) -> Result<Url, FetchError> {
        let request = HttpRequest::get(url.clone());
        let response = http
            .execute(request)
            .await
            .map_err(FetchError::from_http_error)?;

        // The final URL after redirects should be the full Reddit URL
        if response.final_url.host_str() != Some("reddit.com")
            && response.final_url.host_str() != Some("www.reddit.com")
        {
            return Err(FetchError::ParseError(format!(
                "redirect did not resolve to reddit.com: {}",
                response.final_url
            )));
        }

        Ok(response.final_url)
    }

    /// Process JSON response body and extract content.
    fn parse_json_response(body: &[u8]) -> Result<FetchedContent, FetchError> {
        let json: serde_json::Value = serde_json::from_slice(body)
            .map_err(|e| FetchError::ParseError(format!("invalid JSON: {e}")))?;

        let (post, comments) = extract_post_and_comments(&json)?;
        let content = build_fetched_content(&post, &comments);

        debug!(
            title = %post.title,
            media_count = content.media.len(),
            comment_count = comments.len(),
            "parsed reddit post"
        );

        Ok(content)
    }
}

/// Fetch JSON from a URL with proper error handling.
async fn fetch_json(http: &dyn HttpClient, url: &Url) -> Result<HttpResponse, FetchError> {
    let request = HttpRequest::get(url.clone());
    let response = http
        .execute(request)
        .await
        .map_err(FetchError::from_http_error)?;

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
    caption: Option<String>,
    outbound_url: Option<String>,
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
    // Reddit's JSON format varies by endpoint:
    // - Post pages (/r/sub/comments/id.json): 2-element array [post_listing, comments_listing]
    // - Subreddit listings (/r/sub.json): single listing object
    // We handle both by checking if it's an array first.
    // Reddit's API docs: https://www.reddit.com/dev/api/ (see "Listings" section)
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

    // Extract comments from second listing (navigate to children array, then recurse)
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

/// Extract media URLs from a Reddit post (gallery, video, image, or preview).
fn extract_media_urls(post: &RedditPost) -> Vec<Url> {
    let mut urls = Vec::new();

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
                    urls.push(url);
                }
            }
        }
        return urls;
    }

    // Handle video posts (v.redd.it)
    if post.is_video == Some(true) {
        if let Some(video_url) = extract_video_url(post.media.as_ref()) {
            urls.push(video_url);
        }
        return urls;
    }

    // Handle image posts (direct link to image)
    if let Some(ref url_str) = post.url
        && is_image_url(url_str)
        && let Ok(url) = Url::parse(url_str)
    {
        urls.push(url);
        return urls;
    }

    // Fallback: check preview images
    if let Some(ref preview) = post.preview {
        for image in &preview.images {
            // Reddit HTML-encodes the URL in preview.images
            let decoded_url = html_decode(&image.source.url);
            if let Ok(url) = Url::parse(&decoded_url) {
                urls.push(url);
                // preview.images can contain multiple images for link posts, but we only
                // need one fallback. Most posts have just one; taking first is sufficient.
                break;
            }
        }
    }

    urls
}

/// Build markdown content from post selftext, external links, gallery captions, and comments.
fn build_content(post: &RedditPost, comments: &[RedditComment]) -> Option<String> {
    // Collect parts then join with separators - idiomatic Rust for building
    // structured text. String::push_str exists but join() is cleaner here.
    let mut parts = Vec::new();

    // Add selftext (if not deleted/removed)
    if let Some(ref text) = post.selftext
        && !text.is_empty()
        && text != REMOVED
        && text != DELETED
    {
        parts.push(text.clone());
    }

    // Add external link if present (not Reddit, not image)
    if let Some(ref url) = post.url
        && !is_reddit_url(url)
        && !is_image_url(url)
    {
        parts.push(format!("[Link]({url})"));
    }

    // Add gallery captions if any items have them
    if let Some(ref gallery_data) = post.gallery_data {
        let has_captions = gallery_data
            .items
            .iter()
            .any(|item| item.caption.as_deref().is_some_and(|c| !c.is_empty()));

        if has_captions {
            let caption_lines: Vec<String> = gallery_data
                .items
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    let caption = item.caption.as_deref().filter(|c| !c.is_empty());
                    match (caption, &item.outbound_url) {
                        (Some(c), Some(url)) => format!("{}. {} ({})", i + 1, c, url),
                        (Some(c), None) => format!("{}. {}", i + 1, c),
                        _ => format!("{}.", i + 1),
                    }
                })
                .collect();

            parts.push(format!("## Captions\n\n{}", caption_lines.join("\n")));
        }
    }

    // Add comments section (double newlines = Markdown paragraph breaks)
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
        parts.push(comments_md);
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

/// Build [`FetchedContent`] from the extracted post and comments.
fn build_fetched_content(post: &RedditPost, comments: &[RedditComment]) -> FetchedContent {
    // Parse created_utc to NaiveDateTime
    // Unix timestamps always fit in i64, so truncation is safe
    #[allow(clippy::cast_possible_truncation)]
    let published_at = post.created_utc.and_then(|ts| {
        Utc.timestamp_opt(ts as i64, 0)
            .single()
            .map(|dt| dt.naive_utc())
    });

    // Format author with u/ prefix (standard Reddit convention)
    let author = post.author.as_ref().and_then(|a| {
        if a == DELETED {
            None
        } else {
            Some(format!("u/{a}"))
        }
    });

    FetchedContent {
        title: Some(post.title.clone()),
        author,
        published_at,
        content: build_content(post, comments),
        media: extract_media_urls(post),
        discovered_urls: Vec::new(),
    }
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

/// Check if a URL is a Reddit share URL (/r/{subreddit}/s/{id}).
///
/// These URLs are generated by Reddit's share button and must be resolved
/// via redirect before fetching JSON.
fn is_share_url(url: &Url) -> bool {
    // Share URLs have path like /r/subreddit/s/abc123
    let path = url.path();
    path.contains("/s/") && path.starts_with("/r/")
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

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

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
    fn test_is_share_url() -> TestResult {
        let share_url = Url::parse("https://reddit.com/r/pics/s/abc123")?;
        assert!(is_share_url(&share_url));

        let normal_url = Url::parse("https://reddit.com/r/pics/comments/abc123")?;
        assert!(!is_share_url(&normal_url));

        Ok(())
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

    // ==================== URL Normalization Tests ====================

    #[test]
    fn test_normalize_www_reddit() -> TestResult {
        let integration = RedditIntegration::new();
        let url = Url::parse("https://www.reddit.com/r/test")?;
        assert_eq!(
            integration.normalize_url(&url).as_str(),
            "https://reddit.com/r/test"
        );
        Ok(())
    }

    #[test]
    fn test_normalize_old_reddit() -> TestResult {
        let integration = RedditIntegration::new();
        let url = Url::parse("https://old.reddit.com/r/test")?;
        assert_eq!(
            integration.normalize_url(&url).as_str(),
            "https://reddit.com/r/test"
        );
        Ok(())
    }

    #[test]
    fn test_normalize_mobile_reddit() -> TestResult {
        let integration = RedditIntegration::new();
        let url = Url::parse("https://m.reddit.com/r/test")?;
        assert_eq!(
            integration.normalize_url(&url).as_str(),
            "https://reddit.com/r/test"
        );
        Ok(())
    }

    #[test]
    fn test_normalize_np_reddit() -> TestResult {
        let integration = RedditIntegration::new();
        let url = Url::parse("https://np.reddit.com/r/test")?;
        assert_eq!(
            integration.normalize_url(&url).as_str(),
            "https://reddit.com/r/test"
        );
        Ok(())
    }

    #[test]
    fn test_normalize_removes_share_params() -> TestResult {
        let integration = RedditIntegration::new();
        let url =
            Url::parse("https://reddit.com/r/pics/comments/abc?share_id=xyz&ref_source=link")?;
        assert_eq!(
            integration.normalize_url(&url).as_str(),
            "https://reddit.com/r/pics/comments/abc"
        );
        Ok(())
    }

    #[test]
    fn test_normalize_preserves_non_tracking_params() -> TestResult {
        let integration = RedditIntegration::new();
        let url = Url::parse("https://reddit.com/r/test?context=3&share_id=xyz")?;
        assert_eq!(
            integration.normalize_url(&url).as_str(),
            "https://reddit.com/r/test?context=3"
        );
        Ok(())
    }

    #[test]
    fn test_normalize_combined() -> TestResult {
        let integration = RedditIntegration::new();
        let url = Url::parse("https://old.reddit.com/r/test?share_id=abc&context=3")?;
        assert_eq!(
            integration.normalize_url(&url).as_str(),
            "https://reddit.com/r/test?context=3"
        );
        Ok(())
    }

    // ==================== Gallery Caption Tests ====================

    /// Helper to build a gallery post JSON with optional captions and selftext.
    fn gallery_post_json_with_selftext(
        items: &[serde_json::Value],
        selftext: &str,
    ) -> serde_json::Value {
        let media_metadata: serde_json::Map<String, serde_json::Value> = items
            .iter()
            .filter_map(|item| {
                let media_id = item.get("media_id")?.as_str()?;
                Some((
                    media_id.to_string(),
                    serde_json::json!({
                        "status": "valid",
                        "e": "Image",
                        "m": "image/jpg",
                        "s": {
                            "u": format!("https://i.redd.it/{media_id}.jpg"),
                            "x": 1920,
                            "y": 1080
                        },
                        "id": media_id
                    }),
                ))
            })
            .collect();

        serde_json::json!([
            {
                "kind": "Listing",
                "data": {
                    "children": [{
                        "kind": "t3",
                        "data": {
                            "title": "Gallery Post",
                            "author": "testuser",
                            "selftext": selftext,
                            "created_utc": 1700000000.0,
                            "is_gallery": true,
                            "gallery_data": { "items": items },
                            "media_metadata": media_metadata
                        }
                    }]
                }
            },
            { "kind": "Listing", "data": { "children": [] } }
        ])
    }

    fn gallery_post_json(items: &[serde_json::Value]) -> serde_json::Value {
        gallery_post_json_with_selftext(items, "")
    }

    #[test]
    fn test_gallery_without_captions_produces_no_content() -> TestResult {
        let json = gallery_post_json(&[
            serde_json::json!({ "media_id": "img1", "id": 1 }),
            serde_json::json!({ "media_id": "img2", "id": 2 }),
        ]);

        let (post, comments) = extract_post_and_comments(&json)?;
        assert!(
            build_content(&post, &comments).is_none(),
            "gallery with no captions and no selftext should produce no content"
        );
        Ok(())
    }

    #[test]
    fn test_gallery_captions_rendering() -> TestResult {
        // Exercises: selftext coexistence, gap numbering (positions 2/4 skipped),
        // empty-caption skipping, outbound URL rendering, caption-only rendering
        let json = gallery_post_json_with_selftext(
            &[
                serde_json::json!({
                    "media_id": "img1", "id": 1,
                    "caption": "Town square",
                    "outbound_url": "https://example.com/square"
                }),
                serde_json::json!({ "media_id": "img2", "id": 2 }),
                serde_json::json!({ "media_id": "img3", "id": 3, "caption": "Church ruins" }),
                serde_json::json!({ "media_id": "img4", "id": 4, "caption": "" }),
                serde_json::json!({ "media_id": "img5", "id": 5, "caption": "Overgrown path" }),
            ],
            "Some context about the photos.",
        );

        let (post, comments) = extract_post_and_comments(&json)?;
        let content = build_content(&post, &comments).ok_or("should have content")?;

        assert!(content.contains("Some context about the photos."));

        let captions_section = content
            .split("## Captions\n\n")
            .nth(1)
            .ok_or("should have captions section")?;
        assert_eq!(
            captions_section.trim(),
            "1. Town square (https://example.com/square)\n\
             2.\n\
             3. Church ruins\n\
             4.\n\
             5. Overgrown path"
        );
        Ok(())
    }
}
