//! Image content processing.
//!
//! Decodes images, extracts EXIF metadata (date, GPS), computes perceptual hash,
//! and stores to media store.

use std::io::Cursor;

use bytes::Bytes;
use chrono::Utc;
use chronoscope_core::{GeoPoint, Location, UncertainDate, UnresolvedLocation};
use chronoscope_db::{MediaData, MediaType, ResearchUrl};
use image::GenericImageView;
use image_hasher::{HashAlg, HasherConfig};
use tracing::instrument;
use url::Url;

use crate::url_fetcher::content::{ImageFormat, store_image};
use crate::url_fetcher::fetcher::{FetchContext, FetchError, FetchOutcome, FetchResult};

/// Process image content: decode, extract EXIF, compute perceptual hash, store.
#[instrument(skip_all, fields(media_id, width, height))]
pub async fn process(
    ctx: &FetchContext,
    research_url: &ResearchUrl,
    final_url: &Url,
    body: &Bytes,
    format: ImageFormat,
) -> Result<FetchResult, FetchError> {
    // Content-address, decode, and store the original + thumbnail (shared with
    // the fact-store image resolver so the key scheme and thumbnail can't drift).
    let stored = store_image(&ctx.media_store, body, format).await?;
    let (width, height) = stored.image.dimensions();

    // Record dimensions in current span
    let span = tracing::Span::current();
    span.record("width", width);
    span.record("height", height);

    // Compute perceptual hash (Option because videos don't have one, not because this can fail)
    let perceptual_hash = Some(compute_perceptual_hash(&stored.image));

    // Extract EXIF data
    let (captured, location) = extract_exif(body);

    // Create media data
    let media_data = MediaData {
        exact_hash: stored.exact_hash,
        perceptual_hash,
        storage_key: stored.storage_key,
        media_type: MediaType::Image,
        width: i32::try_from(width).unwrap_or(i32::MAX),
        height: i32::try_from(height).unwrap_or(i32::MAX),
        duration_seconds: None,
        captured,
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

// ==================== EXIF Extraction ====================

/// Extract EXIF data from image bytes.
///
/// Returns captured timestamp (as `UncertainDate` with second precision)
/// and GPS location (as `UnresolvedLocation::Resolved(Location::Circle)`) if available.
pub fn extract_exif(body: &Bytes) -> (Option<UncertainDate>, Option<UnresolvedLocation>) {
    let cursor = Cursor::new(body.as_ref());
    let exif_reader = exif::Reader::new();

    let Ok(exif) = exif_reader.read_from_container(&mut std::io::BufReader::new(cursor)) else {
        return (None, None);
    };

    let captured = extract_capture_time(&exif);
    let location = extract_gps_location(&exif);

    (captured, location)
}

/// Extract capture time from EXIF data as `UncertainDate` (day precision).
fn extract_capture_time(exif: &exif::Exif) -> Option<UncertainDate> {
    exif.get_field(exif::Tag::DateTimeOriginal, exif::In::PRIMARY)
        .or_else(|| exif.get_field(exif::Tag::DateTime, exif::In::PRIMARY))
        .and_then(|field| {
            if let exif::Value::Ascii(vec) = &field.value {
                vec.first()
                    .and_then(|bytes| std::str::from_utf8(bytes).ok())
                    .and_then(|s| {
                        chrono::NaiveDateTime::parse_from_str(s, "%Y:%m:%d %H:%M:%S").ok()
                    })
                    .and_then(|dt| {
                        UncertainDate::with_precision(
                            dt.date(),
                            chronoscope_core::DatePrecision::Day,
                        )
                        .ok()
                    })
            } else {
                None
            }
        })
}

/// Extract GPS location from EXIF data as `UnresolvedLocation::Resolved(Location::Circle)`.
fn extract_gps_location(exif: &exif::Exif) -> Option<UnresolvedLocation> {
    let lat = exif.get_field(exif::Tag::GPSLatitude, exif::In::PRIMARY)?;
    let lat_ref = exif.get_field(exif::Tag::GPSLatitudeRef, exif::In::PRIMARY)?;
    let lon = exif.get_field(exif::Tag::GPSLongitude, exif::In::PRIMARY)?;
    let lon_ref = exif.get_field(exif::Tag::GPSLongitudeRef, exif::In::PRIMARY)?;

    let latitude = parse_gps_coordinate(&lat.value)?;
    let longitude = parse_gps_coordinate(&lon.value)?;

    // Apply reference (S and W are negative)
    let latitude = apply_gps_sign(latitude, &lat_ref.value, b"S");
    let longitude = apply_gps_sign(longitude, &lon_ref.value, b"W");

    // Elevation is not embedded in location types; drop it.
    // TODO: Store elevation separately if needed.

    // A GPS coordinate outside the valid lat/lon range is dropped (the
    // whole extractor returns None) rather than surfaced as an error —
    // EXIF GPS data is best-effort.
    let center = GeoPoint::new(latitude, longitude).ok()?;
    Some(UnresolvedLocation::Resolved(Location::point(center)))
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

        let (captured, location) = extract_exif(&png_bytes);

        assert!(captured.is_none());
        assert!(location.is_none());
    }
}
