//! Content type detection and classification.
//!
//! Uses both HTTP Content-Type headers and magic byte sniffing to determine
//! the actual type of fetched content.

use bytes::Bytes;
use sha2::{Digest, Sha256};

/// Compute SHA-256 hash of content bytes.
#[must_use]
pub fn content_hash(body: &Bytes) -> Vec<u8> {
    Sha256::digest(body).to_vec()
}

/// Generate a storage key for media content.
///
/// Format: `media/{hex_hash}.{extension}`
#[must_use]
pub fn storage_key(hash: &[u8], extension: &str) -> String {
    format!("media/{}.{}", hex::encode(hash), extension)
}

/// Detected content type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentType {
    /// HTML document.
    Html,
    /// Image (JPEG, PNG, GIF, WebP, etc.)
    Image(ImageFormat),
    /// Video (MP4, `WebM`, etc.)
    Video(VideoFormat),
    /// Unknown or unsupported type.
    Unknown(String),
}

/// Supported image formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Jpeg,
    Png,
    Gif,
    Webp,
    Avif,
    Heic,
    Tiff,
}

impl ImageFormat {
    /// Get the MIME type for this format.
    #[must_use]
    pub fn mime_type(&self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
            Self::Avif => "image/avif",
            Self::Heic => "image/heic",
            Self::Tiff => "image/tiff",
        }
    }

    /// Get the file extension for this format.
    #[must_use]
    pub fn extension(&self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::Gif => "gif",
            Self::Webp => "webp",
            Self::Avif => "avif",
            Self::Heic => "heic",
            Self::Tiff => "tiff",
        }
    }
}

/// Supported video formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoFormat {
    Mp4,
    Webm,
    Mov,
}

impl VideoFormat {
    /// Get the MIME type for this format.
    #[must_use]
    pub fn mime_type(&self) -> &'static str {
        match self {
            Self::Mp4 => "video/mp4",
            Self::Webm => "video/webm",
            Self::Mov => "video/quicktime",
        }
    }

    /// Get the file extension for this format.
    #[must_use]
    pub fn extension(&self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Webm => "webm",
            Self::Mov => "mov",
        }
    }
}

/// Convert a MIME type string to a `ContentType`.
///
/// Returns `None` if the MIME type is not recognized as a supported type.
fn mime_to_content_type(mime: &str) -> Option<ContentType> {
    match mime {
        "text/html" | "application/xhtml+xml" => Some(ContentType::Html),
        "image/jpeg" | "image/jpg" => Some(ContentType::Image(ImageFormat::Jpeg)),
        "image/png" => Some(ContentType::Image(ImageFormat::Png)),
        "image/gif" => Some(ContentType::Image(ImageFormat::Gif)),
        "image/webp" => Some(ContentType::Image(ImageFormat::Webp)),
        "image/avif" => Some(ContentType::Image(ImageFormat::Avif)),
        "image/heic" => Some(ContentType::Image(ImageFormat::Heic)),
        "image/tiff" => Some(ContentType::Image(ImageFormat::Tiff)),
        "video/mp4" => Some(ContentType::Video(VideoFormat::Mp4)),
        "video/webm" => Some(ContentType::Video(VideoFormat::Webm)),
        "video/quicktime" => Some(ContentType::Video(VideoFormat::Mov)),
        _ => None,
    }
}

/// Detect content type from HTTP headers and/or body bytes.
///
/// Prioritizes magic byte detection over Content-Type header since
/// headers can be misconfigured.
#[must_use]
pub fn detect_content_type(content_type_header: Option<&str>, body: &Bytes) -> ContentType {
    // Try magic byte detection first (more reliable)
    if let Some(inferred) = infer::get(body) {
        let mime = inferred.mime_type();
        if let Some(content_type) = mime_to_content_type(mime) {
            return content_type;
        }
        // Check if it's an image/video type we don't handle
        if mime.starts_with("image/") || mime.starts_with("video/") {
            return ContentType::Unknown(mime.to_string());
        }
        // Fall through to header check for other types
    }

    // Fall back to Content-Type header
    if let Some(header) = content_type_header {
        // Parse MIME type (ignore parameters like charset)
        let mime = header.split(';').next().unwrap_or(header).trim();
        if let Some(content_type) = mime_to_content_type(&mime.to_lowercase()) {
            return content_type;
        }
        return ContentType::Unknown(mime.to_string());
    }

    // Last resort: try to detect HTML by looking for common patterns
    if body.len() >= 5 {
        let start = &body[..body.len().min(1024)];
        if let Ok(text) = std::str::from_utf8(start) {
            let lower = text.to_lowercase();
            if lower.contains("<!doctype html")
                || lower.contains("<html")
                || lower.contains("<head")
                || lower.contains("<body")
            {
                return ContentType::Html;
            }
        }
    }

    ContentType::Unknown("application/octet-stream".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_html_by_header() {
        let html_bytes = Bytes::from_static(b"<html><body>Hello</body></html>");
        let result = detect_content_type(Some("text/html; charset=utf-8"), &html_bytes);
        assert_eq!(result, ContentType::Html);
    }

    #[test]
    fn test_detect_html_by_content_fallback() {
        let html_bytes = Bytes::from_static(b"<!DOCTYPE html><html><body>Hello</body></html>");
        let result = detect_content_type(None, &html_bytes);
        assert_eq!(result, ContentType::Html);
    }

    #[test]
    fn test_detect_unknown() {
        let random_bytes = Bytes::from_static(b"some random bytes that aren't anything");
        let result = detect_content_type(None, &random_bytes);
        assert!(matches!(result, ContentType::Unknown(_)));
    }
}
