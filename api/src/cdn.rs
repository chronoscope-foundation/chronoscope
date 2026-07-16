//! CDN URL generation for media assets.
//!
//! Converts internal storage keys to public CDN URLs with support
//! for different size variants (thumbnail vs full).

use url::Url;

/// Suffix appended to storage keys for thumbnail variants.
const THUMBNAIL_SUFFIX: &str = "_thumb";

/// Generate a full-resolution CDN URL for a media item.
///
/// Storage keys are multi-segment (`/`-separated), so each segment is appended
/// individually — a single push would percent-encode the slashes.
#[must_use]
pub fn full_url(base: &Url, storage_key: &str) -> Url {
    let mut url = base.clone();
    if let Ok(mut segments) = url.path_segments_mut() {
        segments.pop_if_empty().extend(storage_key.split('/'));
    }
    url
}

/// Generate a thumbnail CDN URL for a media item.
///
/// Thumbnails are always JPEG regardless of original format.
#[must_use]
pub fn thumbnail_url(base: &Url, storage_key: &str) -> Url {
    let thumb_key = match storage_key.rfind('.') {
        Some(dot) => format!("{}{THUMBNAIL_SUFFIX}.jpg", &storage_key[..dot]),
        None => format!("{storage_key}{THUMBNAIL_SUFFIX}.jpg"),
    };
    let mut url = base.clone();
    if let Ok(mut segments) = url.path_segments_mut() {
        segments.pop_if_empty().extend(thumb_key.split('/'));
    }
    url
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub const TEST_CDN_BASE_URL: &str = "https://cdn.test.chronoscope.io";

    type TestResult = Result<(), url::ParseError>;

    #[test]
    fn test_thumbnail_url_with_extension() -> TestResult {
        let base = Url::parse(TEST_CDN_BASE_URL)?;
        assert_eq!(
            thumbnail_url(&base, "abc123/image.jpg").as_str(),
            format!("{TEST_CDN_BASE_URL}/abc123/image_thumb.jpg")
        );
        Ok(())
    }

    #[test]
    fn test_thumbnail_url_png_becomes_jpg() -> TestResult {
        let base = Url::parse(TEST_CDN_BASE_URL)?;
        assert_eq!(
            thumbnail_url(&base, "abc123/image.png").as_str(),
            format!("{TEST_CDN_BASE_URL}/abc123/image_thumb.jpg")
        );
        Ok(())
    }

    #[test]
    fn test_thumbnail_url_without_extension() -> TestResult {
        let base = Url::parse(TEST_CDN_BASE_URL)?;
        assert_eq!(
            thumbnail_url(&base, "abc123/image").as_str(),
            format!("{TEST_CDN_BASE_URL}/abc123/image_thumb.jpg")
        );
        Ok(())
    }

    #[test]
    fn test_thumbnail_url_multiple_dots() -> TestResult {
        let base = Url::parse(TEST_CDN_BASE_URL)?;
        assert_eq!(
            thumbnail_url(&base, "2024/01/photo.2024.01.15.png").as_str(),
            format!("{TEST_CDN_BASE_URL}/2024/01/photo.2024.01.15_thumb.jpg")
        );
        Ok(())
    }

    #[test]
    fn test_full_url_appends_under_a_front_door_api_mount() -> TestResult {
        // Dev and test serve thumbnails same-origin through a `/api` front-door
        // proxy, so the base carries an `/api` path segment. The storage key must
        // land *under* it (`/api/media/...`), not replace the segment.
        let base = Url::parse("http://127.0.0.1:8080/api")?;
        assert_eq!(
            full_url(&base, "media/abc123.jpg").as_str(),
            "http://127.0.0.1:8080/api/media/abc123.jpg"
        );
        Ok(())
    }
}
