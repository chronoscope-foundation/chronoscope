//! URL normalization for deduplication and canonicalization.
//!
//! This module normalizes URLs so that equivalent URLs resolve to the same string,
//! improving deduplication when users submit the same content via different link variants.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use url::Url;

/// Tracking parameters to remove from URLs.
/// These don't affect content but clutter URLs and break deduplication.
///
/// This list is derived from the AdGuard URL Tracking Protection filter:
/// <https://github.com/AdguardTeam/FiltersRegistry/blob/master/filters/filter_17_TrackParam/filter.txt>
///
/// We include a subset of the most common cross-site tracking parameters.
/// The full AdGuard list includes many more domain-specific rules.
/// Note that the utm_* prefixed keys are removed further down in code, since there are so many of them.
static TRACKING_PARAMS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    HashSet::from([
        // Facebook/Meta
        "fbclid",
        // Google Ads
        "gclid",
        "gclsrc",
        "dclid",
        // Microsoft Ads
        "msclkid",
        // Mailchimp
        "mc_cid",
        "mc_eid",
        // Instagram
        "igsh",
        // YouTube
        "si",
        // Reddit
        "share_id",
        "ref_source",
    ])
});

/// Domain aliases that should be normalized to a canonical form.
///
/// These are subdomains/variants that serve identical content:
/// - Mobile variants (m., mobile.)
/// - Legacy variants (old.)
/// - Regional/preference variants (www., np.)
static DOMAIN_ALIASES: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    HashMap::from([
        // Reddit - all serve same content
        ("www.reddit.com", "reddit.com"),
        ("old.reddit.com", "reddit.com"),
        ("new.reddit.com", "reddit.com"),
        ("m.reddit.com", "reddit.com"),
        ("i.reddit.com", "reddit.com"),
        ("np.reddit.com", "reddit.com"),
        ("amp.reddit.com", "reddit.com"),
        // Twitter/X - x.com is the new domain
        ("www.twitter.com", "twitter.com"),
        ("mobile.twitter.com", "twitter.com"),
        ("m.twitter.com", "twitter.com"),
        ("x.com", "twitter.com"),
        ("www.x.com", "twitter.com"),
        // Instagram
        ("www.instagram.com", "instagram.com"),
        ("m.instagram.com", "instagram.com"),
        // YouTube
        ("www.youtube.com", "youtube.com"),
        ("m.youtube.com", "youtube.com"),
        // Imgur
        ("www.imgur.com", "imgur.com"),
        ("m.imgur.com", "imgur.com"),
        ("i.imgur.com", "imgur.com"),
        // Flickr
        ("www.flickr.com", "flickr.com"),
        ("m.flickr.com", "flickr.com"),
        // Wikipedia mobile
        ("en.m.wikipedia.org", "en.wikipedia.org"),
    ])
});

/// Error during URL normalization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizeError {
    /// URL scheme is not http or https
    UnsupportedScheme,
    /// Failed to set the normalized host
    InvalidHost,
}

impl std::fmt::Display for NormalizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedScheme => write!(f, "URL scheme must be http or https"),
            Self::InvalidHost => write!(f, "failed to normalize host"),
        }
    }
}

impl std::error::Error for NormalizeError {}

/// Normalize a URL for storage and deduplication.
///
/// This applies several transformations:
/// 1. Lowercase scheme and host
/// 2. Remove default ports (80 for HTTP, 443 for HTTPS)
/// 3. Remove fragments (the part after #)
/// 4. Remove known tracking parameters
/// 5. Normalize known domain aliases
/// 6. Remove trailing slashes from paths (except root)
///
/// # Errors
///
/// Returns `NormalizeError::UnsupportedScheme` if the URL is not http/https.
/// Returns `NormalizeError::InvalidHost` if host normalization fails.
pub fn normalize_url(url: &Url) -> Result<Url, NormalizeError> {
    // Only normalize http/https URLs
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(NormalizeError::UnsupportedScheme);
    }

    let mut url = url.clone();

    // Remove fragment
    url.set_fragment(None);

    // Normalize host (lowercase + domain aliasing)
    if let Some(host) = url.host_str() {
        let host_lower = host.to_lowercase();
        let canonical_host = canonicalize_host(&host_lower);

        url.set_host(Some(&canonical_host))
            .map_err(|_| NormalizeError::InvalidHost)?;
    }

    // Remove default ports
    if (url.scheme() == "http" && url.port() == Some(80))
        || (url.scheme() == "https" && url.port() == Some(443))
    {
        let _ = url.set_port(None);
    }

    // Remove tracking parameters using query_pairs_mut
    {
        // Collect non-tracking params first (can't mutate while iterating)
        let keep_pairs: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(key, _)| !is_tracking_param(key))
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();

        let mut query_mut = url.query_pairs_mut();
        query_mut.clear();
        for (k, v) in keep_pairs {
            query_mut.append_pair(&k, &v);
        }
    }

    // If query is now empty (just "?"), remove it entirely
    if url.query() == Some("") {
        url.set_query(None);
    }

    // Remove trailing slash from path (except for root path "/")
    let path = url.path().to_string();
    if path.len() > 1 && path.ends_with('/') {
        url.set_path(&path[..path.len() - 1]);
    }

    Ok(url)
}

/// Check if a query parameter is a known tracking parameter.
fn is_tracking_param(key: &str) -> bool {
    let key_lower = key.to_lowercase();

    // Check utm_ prefix (Google Analytics and variants)
    // https://support.google.com/analytics/answer/1033863
    if key_lower.starts_with("utm_") {
        return true;
    }

    // Check known tracking params
    TRACKING_PARAMS.contains(key_lower.as_str())
}

/// Canonicalize a hostname using the domain alias table.
fn canonicalize_host(host: &str) -> String {
    DOMAIN_ALIASES
        .get(host)
        .copied()
        .unwrap_or(host)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    /// Helper to normalize a URL string, returning the result as a string.
    fn normalize(input: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let url = Url::parse(input)?;
        Ok(normalize_url(&url)?.to_string())
    }

    // ==================== Basic Normalization ====================

    #[test]
    fn test_lowercase_host() -> TestResult {
        assert_eq!(
            normalize("https://EXAMPLE.COM/path")?,
            "https://example.com/path"
        );
        Ok(())
    }

    #[test]
    fn test_remove_fragment() -> TestResult {
        assert_eq!(
            normalize("https://example.com/page#section")?,
            "https://example.com/page"
        );
        Ok(())
    }

    #[test]
    fn test_remove_default_http_port() -> TestResult {
        assert_eq!(
            normalize("http://example.com:80/path")?,
            "http://example.com/path"
        );
        Ok(())
    }

    #[test]
    fn test_remove_default_https_port() -> TestResult {
        assert_eq!(
            normalize("https://example.com:443/path")?,
            "https://example.com/path"
        );
        Ok(())
    }

    #[test]
    fn test_preserve_non_default_port() -> TestResult {
        assert_eq!(
            normalize("https://example.com:8080/path")?,
            "https://example.com:8080/path"
        );
        Ok(())
    }

    #[test]
    fn test_remove_trailing_slash() -> TestResult {
        assert_eq!(
            normalize("https://example.com/path/")?,
            "https://example.com/path"
        );
        Ok(())
    }

    #[test]
    fn test_preserve_root_path() -> TestResult {
        assert_eq!(normalize("https://example.com/")?, "https://example.com/");
        Ok(())
    }

    #[test]
    fn test_preserve_root_path_no_slash() -> TestResult {
        // URL parser adds trailing slash to root
        assert_eq!(normalize("https://example.com")?, "https://example.com/");
        Ok(())
    }

    // ==================== Tracking Parameter Removal ====================

    #[test]
    fn test_remove_utm_params() -> TestResult {
        assert_eq!(
            normalize("https://example.com/page?utm_source=twitter&utm_medium=social&id=123")?,
            "https://example.com/page?id=123"
        );
        Ok(())
    }

    #[test]
    fn test_remove_fbclid() -> TestResult {
        assert_eq!(
            normalize("https://example.com/page?fbclid=abc123&id=456")?,
            "https://example.com/page?id=456"
        );
        Ok(())
    }

    #[test]
    fn test_remove_all_tracking_leaves_no_query() -> TestResult {
        assert_eq!(
            normalize("https://example.com/page?utm_source=x&fbclid=y")?,
            "https://example.com/page"
        );
        Ok(())
    }

    #[test]
    fn test_preserve_non_tracking_params() -> TestResult {
        assert_eq!(
            normalize("https://example.com/search?q=test&page=2")?,
            "https://example.com/search?q=test&page=2"
        );
        Ok(())
    }

    #[test]
    fn test_remove_utm_prefix_variants() -> TestResult {
        // Should catch any utm_* even if not in our explicit list
        assert_eq!(
            normalize("https://example.com/?utm_custom=foo&id=1")?,
            "https://example.com/?id=1"
        );
        Ok(())
    }

    #[test]
    fn test_tracking_params_case_insensitive() -> TestResult {
        assert_eq!(
            normalize("https://example.com/page?UTM_SOURCE=x&FBCLID=y&id=1")?,
            "https://example.com/page?id=1"
        );
        Ok(())
    }

    // ==================== Domain Aliasing ====================

    #[test]
    fn test_reddit_www_to_canonical() -> TestResult {
        assert_eq!(
            normalize("https://www.reddit.com/r/test")?,
            "https://reddit.com/r/test"
        );
        Ok(())
    }

    #[test]
    fn test_reddit_old_to_canonical() -> TestResult {
        assert_eq!(
            normalize("https://old.reddit.com/r/test")?,
            "https://reddit.com/r/test"
        );
        Ok(())
    }

    #[test]
    fn test_reddit_mobile_to_canonical() -> TestResult {
        assert_eq!(
            normalize("https://m.reddit.com/r/test")?,
            "https://reddit.com/r/test"
        );
        Ok(())
    }

    #[test]
    fn test_twitter_www_to_canonical() -> TestResult {
        assert_eq!(
            normalize("https://www.twitter.com/user/status/123")?,
            "https://twitter.com/user/status/123"
        );
        Ok(())
    }

    #[test]
    fn test_x_to_twitter() -> TestResult {
        assert_eq!(
            normalize("https://x.com/user/status/123")?,
            "https://twitter.com/user/status/123"
        );
        Ok(())
    }

    #[test]
    fn test_www_x_to_twitter() -> TestResult {
        assert_eq!(
            normalize("https://www.x.com/user/status/123")?,
            "https://twitter.com/user/status/123"
        );
        Ok(())
    }

    #[test]
    fn test_youtube_mobile_to_canonical() -> TestResult {
        assert_eq!(
            normalize("https://m.youtube.com/watch?v=abc123")?,
            "https://youtube.com/watch?v=abc123"
        );
        Ok(())
    }

    #[test]
    fn test_wikipedia_mobile_to_canonical() -> TestResult {
        assert_eq!(
            normalize("https://en.m.wikipedia.org/wiki/Test")?,
            "https://en.wikipedia.org/wiki/Test"
        );
        Ok(())
    }

    #[test]
    fn test_unknown_domain_unchanged() -> TestResult {
        // Domains not in our alias list are left as-is (including www)
        assert_eq!(
            normalize("https://www.somesite.com/page")?,
            "https://www.somesite.com/page"
        );
        Ok(())
    }

    // ==================== Error Cases ====================

    #[test]
    fn test_non_http_scheme_returns_error() -> TestResult {
        let url = Url::parse("ftp://files.example.com/file.txt")?;
        assert_eq!(normalize_url(&url), Err(NormalizeError::UnsupportedScheme));
        Ok(())
    }

    #[test]
    fn test_mailto_returns_error() -> TestResult {
        let url = Url::parse("mailto:user@example.com")?;
        assert_eq!(normalize_url(&url), Err(NormalizeError::UnsupportedScheme));
        Ok(())
    }

    // ==================== Edge Cases ====================

    #[test]
    fn test_combined_normalizations() -> TestResult {
        // Test multiple normalizations at once
        assert_eq!(
            normalize(
                "https://OLD.REDDIT.COM:443/r/HistoryPorn/comments/abc123/?utm_source=share&utm_medium=web#comments"
            )?,
            "https://reddit.com/r/HistoryPorn/comments/abc123"
        );
        Ok(())
    }

    #[test]
    fn test_empty_query_value() -> TestResult {
        assert_eq!(
            normalize("https://example.com/page?flag=&id=123")?,
            "https://example.com/page?flag=&id=123"
        );
        Ok(())
    }

    #[test]
    fn test_encoded_characters_preserved() -> TestResult {
        assert_eq!(
            normalize("https://example.com/path%20with%20spaces")?,
            "https://example.com/path%20with%20spaces"
        );
        Ok(())
    }

    #[test]
    fn test_reddit_share_params_removed() -> TestResult {
        assert_eq!(
            normalize("https://reddit.com/r/pics/comments/abc?share_id=xyz&ref_source=link")?,
            "https://reddit.com/r/pics/comments/abc"
        );
        Ok(())
    }

    #[test]
    fn test_instagram_tracking_removed() -> TestResult {
        assert_eq!(
            normalize("https://instagram.com/p/abc123?igsh=xyz")?,
            "https://instagram.com/p/abc123"
        );
        Ok(())
    }

    #[test]
    fn test_normalization_is_idempotent() -> TestResult {
        let urls = [
            "https://EXAMPLE.COM/path?utm_source=x#frag",
            "https://old.reddit.com/r/test/?share_id=abc",
            "https://www.x.com/user/status/123",
            "https://example.com/search?q=test&page=2",
        ];
        for url_str in urls {
            let url = Url::parse(url_str)?;
            let once = normalize_url(&url)?;
            let twice = normalize_url(&once)?;
            assert_eq!(once, twice, "Normalization not idempotent for: {url_str}");
        }
        Ok(())
    }

    #[test]
    fn test_trailing_slash_with_query_string() -> TestResult {
        assert_eq!(
            normalize("https://example.com/path/?q=test")?,
            "https://example.com/path?q=test"
        );
        Ok(())
    }
}
