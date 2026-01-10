//! CDN URL generation for media assets.
//!
//! Converts internal storage keys to public CDN URLs with support
//! for different size variants (thumbnail vs full).

/// Suffix appended to storage keys for thumbnail variants.
const THUMBNAIL_SUFFIX: &str = "_thumb";

/// Generate a full-resolution CDN URL for a media item.
#[must_use]
pub fn full_url(base_url: &str, storage_key: &str) -> String {
    format!("{base_url}/{storage_key}")
}

/// Generate a thumbnail CDN URL for a media item.
#[must_use]
pub fn thumbnail_url(base_url: &str, storage_key: &str) -> String {
    // Insert suffix before file extension, or append if no extension
    if let Some(dot_pos) = storage_key.rfind('.') {
        let (name, ext) = storage_key.split_at(dot_pos);
        format!("{base_url}/{name}{THUMBNAIL_SUFFIX}{ext}")
    } else {
        format!("{base_url}/{storage_key}{THUMBNAIL_SUFFIX}")
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub const TEST_CDN_BASE_URL: &str = "https://cdn.test.chronoscope.io";

    #[test]
    fn test_full_url() {
        assert_eq!(
            full_url(TEST_CDN_BASE_URL, "abc123/image.jpg"),
            format!("{TEST_CDN_BASE_URL}/abc123/image.jpg")
        );
    }

    #[test]
    fn test_thumbnail_url_with_extension() {
        assert_eq!(
            thumbnail_url(TEST_CDN_BASE_URL, "abc123/image.jpg"),
            format!("{TEST_CDN_BASE_URL}/abc123/image_thumb.jpg")
        );
    }

    #[test]
    fn test_thumbnail_url_without_extension() {
        assert_eq!(
            thumbnail_url(TEST_CDN_BASE_URL, "abc123/image"),
            format!("{TEST_CDN_BASE_URL}/abc123/image_thumb")
        );
    }

    #[test]
    fn test_thumbnail_url_multiple_dots() {
        assert_eq!(
            thumbnail_url(TEST_CDN_BASE_URL, "2024/01/photo.2024.01.15.png"),
            format!("{TEST_CDN_BASE_URL}/2024/01/photo.2024.01.15_thumb.png")
        );
    }
}
