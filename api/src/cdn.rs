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
///
/// Thumbnails are always JPEG regardless of original format.
#[must_use]
pub fn thumbnail_url(base_url: &str, storage_key: &str) -> String {
    // Strip original extension and always use .jpg
    if let Some(dot_pos) = storage_key.rfind('.') {
        let name = &storage_key[..dot_pos];
        format!("{base_url}/{name}{THUMBNAIL_SUFFIX}.jpg")
    } else {
        format!("{base_url}/{storage_key}{THUMBNAIL_SUFFIX}.jpg")
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub const TEST_CDN_BASE_URL: &str = "https://cdn.test.chronoscope.io";

    #[test]
    fn test_thumbnail_url_with_extension() {
        assert_eq!(
            thumbnail_url(TEST_CDN_BASE_URL, "abc123/image.jpg"),
            format!("{TEST_CDN_BASE_URL}/abc123/image_thumb.jpg")
        );
    }

    #[test]
    fn test_thumbnail_url_png_becomes_jpg() {
        assert_eq!(
            thumbnail_url(TEST_CDN_BASE_URL, "abc123/image.png"),
            format!("{TEST_CDN_BASE_URL}/abc123/image_thumb.jpg")
        );
    }

    #[test]
    fn test_thumbnail_url_without_extension() {
        assert_eq!(
            thumbnail_url(TEST_CDN_BASE_URL, "abc123/image"),
            format!("{TEST_CDN_BASE_URL}/abc123/image_thumb.jpg")
        );
    }

    #[test]
    fn test_thumbnail_url_multiple_dots() {
        assert_eq!(
            thumbnail_url(TEST_CDN_BASE_URL, "2024/01/photo.2024.01.15.png"),
            format!("{TEST_CDN_BASE_URL}/2024/01/photo.2024.01.15_thumb.jpg")
        );
    }
}
