//! Image content processing.
//!
//! Decodes images, extracts EXIF metadata (date, GPS), computes perceptual hash,
//! and stores to media store.

use std::io::Cursor;

use bytes::Bytes;
use chrono::{NaiveDateTime, Utc};
use chronoscope_db::{GpsLocation, MediaData, MediaType, ResearchUrl};
use image::{GenericImageView, ImageEncoder};
use image_hasher::{HashAlg, HasherConfig};
use tracing::instrument;
use url::Url;

use crate::url_fetcher::content::{ImageFormat, content_hash, storage_key, thumbnail_key};
use crate::url_fetcher::fetcher::{FetchContext, FetchError, FetchOutcome, FetchResult};

/// Maximum dimension (width or height) for thumbnails.
const THUMBNAIL_SIZE: u32 = 512;

/// JPEG quality for thumbnails (0-100).
const THUMBNAIL_QUALITY: u8 = 80;

/// Process image content: decode, extract EXIF, compute perceptual hash, store.
#[instrument(skip_all, fields(media_id, width, height))]
pub async fn process(
    ctx: &FetchContext,
    research_url: &ResearchUrl,
    final_url: &Url,
    body: &Bytes,
    format: ImageFormat,
) -> Result<FetchResult, FetchError> {
    // Compute exact hash
    let exact_hash = content_hash(body);

    // Decode image to get dimensions
    let img = image::load_from_memory(body)
        .map_err(|e| FetchError::ContentProcessing(format!("failed to decode image: {e}")))?;
    let (width, height) = img.dimensions();

    // Record dimensions in current span
    let span = tracing::Span::current();
    span.record("width", width);
    span.record("height", height);

    // Compute perceptual hash (Option because videos don't have one, not because this can fail)
    let perceptual_hash = Some(compute_perceptual_hash(&img));

    // Extract EXIF data
    let (captured_at, location) = extract_exif(body);

    // Generate storage key based on exact hash
    let storage_key = storage_key(&exact_hash, format.extension());

    // Store in media store
    ctx.media_store
        .put(&storage_key, body.clone(), format.mime_type())
        .await?;

    // Generate and store thumbnail (failures don't fail the fetch)
    let thumb_key = thumbnail_key(&exact_hash);
    match generate_thumbnail(&img) {
        Ok(thumb_bytes) => {
            if let Err(e) = ctx
                .media_store
                .put(&thumb_key, thumb_bytes, "image/jpeg")
                .await
            {
                tracing::warn!(error = %e, "failed to store thumbnail");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to generate thumbnail");
        }
    }

    // Create media data
    let media_data = MediaData {
        exact_hash,
        perceptual_hash,
        storage_key,
        media_type: MediaType::Image,
        width: i32::try_from(width).unwrap_or(i32::MAX),
        height: i32::try_from(height).unwrap_or(i32::MAX),
        duration_seconds: None,
        captured_at,
        location,
        source_metadata: Some(
            serde_json::json!({
                "final_url": final_url.as_str(),
                "format": format.extension(),
            })
            .to_string(),
        ),
        fetched_at: Utc::now().naive_utc(),
    };

    // Store media in database (deduplicates by exact hash)
    let media_id = ctx.db.get_or_create_media(&media_data).await?;

    // Record media_id in current span
    span.record("media_id", media_id.to_string());

    // Mark URL as resolved
    ctx.db
        .mark_url_resolved_to_media(&research_url.id, &media_id)
        .await?;

    Ok(FetchResult {
        outcome: FetchOutcome::Media { media_id },
        discovered_urls: vec![],
    })
}

/// Compute perceptual hash of an image using double gradient algorithm.
fn compute_perceptual_hash(img: &image::DynamicImage) -> Vec<u8> {
    // DoubleGradient computes horizontal and vertical gradients, making it robust against
    // brightness/contrast changes and minor edits while being more discriminative than
    // simpler algorithms like Mean or Gradient. The 16x16 size provides a good balance
    // between collision resistance (256-bit hash) and tolerance for resizing/compression.
    let hasher = HasherConfig::new()
        .hash_alg(HashAlg::DoubleGradient)
        .hash_size(16, 16)
        .to_hasher();

    let hash = hasher.hash_image(img);
    hash.as_bytes().to_vec()
}

/// Generate a thumbnail from an image.
///
/// Resizes the image so the longest edge is at most [`THUMBNAIL_SIZE`] pixels,
/// then encodes as JPEG with quality [`THUMBNAIL_QUALITY`].
fn generate_thumbnail(img: &image::DynamicImage) -> Result<Bytes, String> {
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

// ==================== EXIF Extraction ====================

/// Extract EXIF data from image bytes.
///
/// Returns captured timestamp and GPS location if available.
pub fn extract_exif(body: &Bytes) -> (Option<NaiveDateTime>, Option<GpsLocation>) {
    let cursor = Cursor::new(body.as_ref());
    let exif_reader = exif::Reader::new();

    let Ok(exif) = exif_reader.read_from_container(&mut std::io::BufReader::new(cursor)) else {
        return (None, None);
    };

    let captured_at = extract_capture_time(&exif);
    let location = extract_gps_location(&exif);

    (captured_at, location)
}

/// Extract capture time from EXIF data.
fn extract_capture_time(exif: &exif::Exif) -> Option<NaiveDateTime> {
    exif.get_field(exif::Tag::DateTimeOriginal, exif::In::PRIMARY)
        .or_else(|| exif.get_field(exif::Tag::DateTime, exif::In::PRIMARY))
        .and_then(|field| {
            if let exif::Value::Ascii(vec) = &field.value {
                vec.first()
                    .and_then(|bytes| std::str::from_utf8(bytes).ok())
                    .and_then(|s| NaiveDateTime::parse_from_str(s, "%Y:%m:%d %H:%M:%S").ok())
            } else {
                None
            }
        })
}

/// Extract GPS location from EXIF data.
fn extract_gps_location(exif: &exif::Exif) -> Option<GpsLocation> {
    let lat = exif.get_field(exif::Tag::GPSLatitude, exif::In::PRIMARY)?;
    let lat_ref = exif.get_field(exif::Tag::GPSLatitudeRef, exif::In::PRIMARY)?;
    let lon = exif.get_field(exif::Tag::GPSLongitude, exif::In::PRIMARY)?;
    let lon_ref = exif.get_field(exif::Tag::GPSLongitudeRef, exif::In::PRIMARY)?;

    let latitude = parse_gps_coordinate(&lat.value)?;
    let longitude = parse_gps_coordinate(&lon.value)?;

    // Apply reference (S and W are negative)
    let latitude = apply_gps_sign(latitude, &lat_ref.value, b"S");
    let longitude = apply_gps_sign(longitude, &lon_ref.value, b"W");

    // Extract altitude if available
    let altitude = exif
        .get_field(exif::Tag::GPSAltitude, exif::In::PRIMARY)
        .and_then(|field| parse_gps_altitude(&field.value));

    Some(GpsLocation {
        latitude,
        longitude,
        altitude,
    })
}

/// Apply sign to GPS coordinate based on reference (S/W are negative).
fn apply_gps_sign(value: f64, ref_value: &exif::Value, negative_ref: &[u8]) -> f64 {
    if matches_ref(ref_value, negative_ref) {
        -value
    } else {
        value
    }
}

/// Parse GPS coordinate from EXIF rational values (degrees, minutes, seconds).
fn parse_gps_coordinate(value: &exif::Value) -> Option<f64> {
    if let exif::Value::Rational(rationals) = value
        && rationals.len() >= 3
    {
        let degrees = rationals[0].to_f64();
        let minutes = rationals[1].to_f64();
        let seconds = rationals[2].to_f64();
        return Some(degrees + minutes / 60.0 + seconds / 3600.0);
    }
    None
}

/// Parse GPS altitude from EXIF rational value.
fn parse_gps_altitude(value: &exif::Value) -> Option<f64> {
    if let exif::Value::Rational(rationals) = value {
        rationals.first().map(exif::Rational::to_f64)
    } else {
        None
    }
}

/// Check if an EXIF value matches a reference byte (e.g., "N", "S", "E", "W").
fn matches_ref(value: &exif::Value, expected: &[u8]) -> bool {
    if let exif::Value::Ascii(vec) = value {
        vec.first().is_some_and(|v| v == expected)
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_exif_no_exif() {
        // PNG with no EXIF data
        let png_bytes = Bytes::from_static(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);

        let (captured_at, location) = extract_exif(&png_bytes);

        assert!(captured_at.is_none());
        assert!(location.is_none());
    }
}
