//! Content type detection and classification.
//!
//! Uses both HTTP Content-Type headers and magic byte sniffing to determine
//! the actual type of fetched content.

use std::sync::Arc;

use bytes::Bytes;
use chronoscope_db::media_store::{MediaStore, MediaStoreError};
use image::ImageEncoder;
use sha2::{Digest, Sha256};

/// Maximum dimension (width or height) for thumbnails.
const THUMBNAIL_SIZE: u32 = 512;

/// JPEG quality for thumbnails (0-100).
const THUMBNAIL_QUALITY: u8 = 80;

/// Compute SHA-256 hash of content bytes.
#[must_use]
pub fn content_hash(body: &Bytes) -> Vec<u8> {
    Sha256::digest(body).to_vec()
}

/// Generate a thumbnail from an image.
///
/// Resizes the image so the longest edge is at most [`THUMBNAIL_SIZE`] pixels,
/// then encodes as JPEG with quality [`THUMBNAIL_QUALITY`]. Always JPEG, so the
/// key produced by [`thumbnail_key`] matches the encoded bytes' format.
///
/// # Errors
/// Returns the encoder's message if JPEG encoding fails.
pub fn generate_thumbnail(img: &image::DynamicImage) -> Result<Bytes, String> {
    let thumb = img.thumbnail(THUMBNAIL_SIZE, THUMBNAIL_SIZE);
    let rgb = thumb.to_rgb8();

    let mut buffer = Vec::new();
    let encoder =
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buffer, THUMBNAIL_QUALITY);
    encoder
        .write_image(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|e| format!("failed to encode thumbnail: {e}"))?;

    Ok(Bytes::from(buffer))
}

/// The media-store keys and decoded image produced by [`store_image`].
pub struct StoredImage {
    /// Key of the stored original, `media/{hash}.{ext}`.
    pub storage_key: String,
    /// Key of the stored JPEG thumbnail, `media/{hash}_thumb.jpg`, or `None`
    /// when thumbnail generation or its `put` failed. The thumbnail is
    /// best-effort — a transient failure there must not lose the original.
    pub thumbnail_key: Option<String>,
    /// SHA-256 of the original bytes — the content address callers dedup on.
    pub exact_hash: Vec<u8>,
    /// The decoded original, for callers that read dimensions / metadata.
    pub image: image::DynamicImage,
}

/// Fatal failure while storing an image via [`store_image`] — decoding or the
/// original `put`. Thumbnail failures are best-effort and never surface here.
#[derive(Debug, thiserror::Error)]
pub enum StoreImageError {
    /// The bytes didn't decode as an image.
    #[error("failed to decode image: {0}")]
    Decode(#[from] image::ImageError),
    /// The media store rejected the original `put`.
    #[error(transparent)]
    MediaStore(#[from] MediaStoreError),
}

/// Content-address `body`, store the original and a generated JPEG thumbnail in
/// `media_store`, and return the keys plus the decoded image.
///
/// The one place the store sequence — key scheme, thumbnail parameters,
/// content types — lives, so the url-fetcher worker and the fact-store image
/// resolver can't drift. Callers layer their own concerns (EXIF, perceptual
/// hash, DB rows) on the returned `image`/`exact_hash`.
///
/// Decoding runs before the `put`, so undecodable bytes never leave an orphan
/// in the store. Storing the original is fatal; the thumbnail is best-effort —
/// a failure there is logged and yields `thumbnail_key: None` rather than
/// discarding the successfully-stored original.
///
/// # Errors
/// [`StoreImageError`] if the bytes don't decode or the original `put` fails.
pub async fn store_image(
    media_store: &Arc<dyn MediaStore>,
    body: &Bytes,
    format: ImageFormat,
) -> Result<StoredImage, StoreImageError> {
    let exact_hash = content_hash(body);
    let image = image::load_from_memory(body)?;

    let storage_key = storage_key(&exact_hash, format.extension());
    media_store
        .put(&storage_key, body.clone(), format.mime_type())
        .await?;

    let thumbnail_key = store_thumbnail(media_store, &exact_hash, &image).await;

    Ok(StoredImage {
        storage_key,
        thumbnail_key,
        exact_hash,
        image,
    })
}

/// Generate and store a thumbnail, returning its key on success. Best-effort:
/// a generate or `put` failure is logged and yields `None` so the caller keeps
/// the already-stored original.
async fn store_thumbnail(
    media_store: &Arc<dyn MediaStore>,
    exact_hash: &[u8],
    image: &image::DynamicImage,
) -> Option<String> {
    let thumbnail = match generate_thumbnail(image) {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::warn!(error = %e, "failed to generate thumbnail");
            return None;
        }
    };
    let key = thumbnail_key(exact_hash);
    match media_store.put(&key, thumbnail, "image/jpeg").await {
        Ok(()) => Some(key),
        Err(e) => {
            tracing::warn!(error = %e, key = %key, "failed to store thumbnail");
            None
        }
    }
}

/// Generate a storage key for media content.
///
/// Format: `media/{hex_hash}.{extension}`
#[must_use]
pub fn storage_key(hash: &[u8], extension: &str) -> String {
    format!("media/{}.{}", hex::encode(hash), extension)
}

/// Generate a thumbnail storage key from the hash.
///
/// Thumbnails are always JPEG for consistent compression.
#[must_use]
pub fn thumbnail_key(hash: &[u8]) -> String {
    format!("media/{}_thumb.jpg", hex::encode(hash))
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

    use std::pin::Pin;

    use async_trait::async_trait;
    use chronoscope_db::media_store::{InMemoryMediaStore, MediaMetadata};
    use tokio::io::AsyncRead;

    /// A media store that fails every thumbnail `put` (any `_thumb` key) and
    /// delegates the rest to an in-memory store — models a transient store
    /// hiccup that hits only the thumbnail.
    struct FailThumbnailStore(InMemoryMediaStore);

    #[async_trait]
    impl MediaStore for FailThumbnailStore {
        async fn put_stream(
            &self,
            key: &str,
            reader: Pin<Box<dyn AsyncRead + Send>>,
            content_type: &str,
        ) -> Result<(), MediaStoreError> {
            if key.contains("_thumb") {
                return Err(MediaStoreError::Io(std::io::Error::other(
                    "simulated thumbnail store failure",
                )));
            }
            self.0.put_stream(key, reader, content_type).await
        }

        async fn get_stream(
            &self,
            key: &str,
        ) -> Result<Option<Pin<Box<dyn AsyncRead + Send>>>, MediaStoreError> {
            self.0.get_stream(key).await
        }

        async fn head(&self, key: &str) -> Result<Option<MediaMetadata>, MediaStoreError> {
            self.0.head(key).await
        }
    }

    /// A minimal valid JPEG the `image` crate can decode.
    fn tiny_jpeg() -> Result<Bytes, Box<dyn std::error::Error + Send + Sync>> {
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            4,
            4,
            image::Rgb([10, 20, 30]),
        ));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Jpeg)?;
        Ok(Bytes::from(buf.into_inner()))
    }

    #[tokio::test]
    async fn store_image_keeps_original_when_thumbnail_put_fails()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let jpeg = tiny_jpeg()?;
        let store: Arc<dyn MediaStore> = Arc::new(FailThumbnailStore(InMemoryMediaStore::new()));

        let stored = store_image(&store, &jpeg, ImageFormat::Jpeg).await?;

        assert!(
            stored.thumbnail_key.is_none(),
            "a failed thumbnail put must not surface a key"
        );
        assert!(
            store.get(&stored.storage_key).await?.is_some(),
            "the original stays stored despite the thumbnail failure"
        );
        Ok(())
    }
}
