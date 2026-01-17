//! Video content processing.
//!
//! Parses video metadata (dimensions, duration) and stores to media store.

use std::io::Cursor;

use bytes::Bytes;
use chrono::Utc;
use chronoscope_db::{MediaData, MediaType, ResearchUrl};
use tracing::instrument;
use url::Url;

use crate::url_fetcher::content::{VideoFormat, content_hash, storage_key};
use crate::url_fetcher::fetcher::{FetchContext, FetchError, FetchOutcome, FetchResult};

/// Video metadata extracted from container.
struct VideoMetadata {
    width: i32,
    height: i32,
    duration_seconds: f32,
}

/// Process video content: parse metadata, store.
#[instrument(skip_all, fields(media_id, width, height, duration))]
pub async fn process(
    ctx: &FetchContext,
    research_url: &ResearchUrl,
    final_url: &Url,
    body: &Bytes,
    format: VideoFormat,
) -> Result<FetchResult, FetchError> {
    // Compute exact hash
    let exact_hash = content_hash(body);

    // Parse video metadata based on format
    let metadata = match format {
        VideoFormat::Mp4 | VideoFormat::Mov => parse_mp4_metadata(body)?,
        VideoFormat::Webm => {
            // TODO: WebM parsing requires file path, not in-memory buffer.
            // For now, store with unknown dimensions.
            VideoMetadata {
                width: 0,
                height: 0,
                duration_seconds: 0.0,
            }
        }
    };

    // Record metadata in current span
    let span = tracing::Span::current();
    span.record("width", metadata.width);
    span.record("height", metadata.height);
    span.record("duration", metadata.duration_seconds);

    // Generate storage key based on exact hash
    let storage_key = storage_key(&exact_hash, format.extension());

    // Store in media store
    ctx.media_store
        .put(&storage_key, body.clone(), format.mime_type())
        .await?;

    // Create media data
    let media_data = MediaData {
        exact_hash,
        perceptual_hash: None, // No perceptual hash for video
        storage_key,
        media_type: MediaType::Video,
        width: metadata.width,
        height: metadata.height,
        duration_seconds: Some(metadata.duration_seconds),
        // TODO: Video metadata extraction (captured_at, location) is possible via the `creation_time`
        // field in MP4/MOV mvhd atoms, but mp4parse doesn't expose it directly. Would need to parse
        // raw atoms or use a different library. No standard like EXIF exists for video.
        captured_at: None,
        location: None,
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

/// Parse MP4/MOV video metadata.
///
/// Extracts dimensions and duration from the video container.
fn parse_mp4_metadata(body: &Bytes) -> Result<VideoMetadata, FetchError> {
    let cursor = Cursor::new(body.as_ref());

    let context = mp4parse::read_mp4(&mut std::io::BufReader::new(cursor))
        .map_err(|e| FetchError::ContentProcessing(format!("failed to parse MP4: {e}")))?;

    // Find video track
    let video_track = context
        .tracks
        .iter()
        .find(|t| matches!(t.track_type, mp4parse::TrackType::Video));

    let Some(track) = video_track else {
        return Err(FetchError::ContentProcessing(
            "no video track found".to_string(),
        ));
    };

    // Get dimensions from track header.
    // Per ISO 14496-12, tkhd width/height are stored in fixed-point 16.16 format:
    // upper 16 bits = integer part, lower 16 bits = fractional part.
    // We extract the integer part by right-shifting.
    let (width, height) = track.tkhd.as_ref().map_or((0, 0), |tkhd| {
        let w = i32::try_from(tkhd.width >> 16).unwrap_or(i32::MAX);
        let h = i32::try_from(tkhd.height >> 16).unwrap_or(i32::MAX);
        (w, h)
    });

    // Calculate duration from track timescale.
    // Use f64 for intermediate calculation to avoid precision loss with large values.
    let duration_seconds = track
        .duration
        .and_then(|d| {
            track.timescale.map(|ts| {
                let timescale = ts.0 as f64;
                if timescale > 0.0 {
                    (d.0 as f64 / timescale) as f32
                } else {
                    0.0
                }
            })
        })
        .unwrap_or(0.0);

    Ok(VideoMetadata {
        width,
        height,
        duration_seconds,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_mp4_invalid_data() {
        let invalid_bytes = Bytes::from_static(b"not an mp4 file");
        let result = parse_mp4_metadata(&invalid_bytes);
        assert!(result.is_err());
    }
}
