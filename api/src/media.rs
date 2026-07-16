//! Embedded media serving endpoint.
//!
//! This module provides a `/media/{key}` endpoint for serving media directly
//! from the API server. It's only available with the `embedded-media` feature
//! and is primarily intended for local development.
//!
//! In production, media should be served from a proper CDN.

use std::sync::Arc;

use dropshot::{Body, ClientErrorStatusCode, HttpError, Path, RequestContext, endpoint};
use http::header::{CACHE_CONTROL, CONTENT_TYPE};
use http::{Response, StatusCode};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::state::AppState;

/// Path parameters for media endpoint.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct MediaPath {
    /// The storage key for the media item (e.g., "media/abc123.jpg")
    pub key: String,
}

/// Get a media item by storage key.
///
/// Returns the raw media bytes with appropriate Content-Type header.
/// This endpoint is only available when the `embedded-media` feature is enabled.
#[endpoint {
    method = GET,
    path = "/media/{key}",
    tags = ["media"],
}]
pub async fn get_media(
    rqctx: RequestContext<Arc<AppState>>,
    path: Path<MediaPath>,
) -> Result<Response<Body>, HttpError> {
    let state = rqctx.context();
    let path_key = path.into_inner().key;

    // Reject path traversal attempts. The current InMemoryMediaStore is safe,
    // but this protects against future filesystem-backed implementations.
    if path_key.contains("..") {
        return Err(HttpError::for_client_error(
            None,
            ClientErrorStatusCode::BAD_REQUEST,
            "Invalid key".to_string(),
        ));
    }

    // Storage keys are prefixed with "media/" (e.g., "media/abc123.jpg"),
    // but we serve them at "/media/{key}" where key is "abc123.jpg".
    let storage_key = format!("media/{path_key}");

    let media = state
        .media_store
        .get(&storage_key)
        .await
        .map_err(|e| HttpError::for_internal_error(format!("Media store error: {e}")))?
        .ok_or_else(|| {
            HttpError::for_client_error(
                None,
                ClientErrorStatusCode::NOT_FOUND,
                "Media not found".to_string(),
            )
        })?;

    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, media.metadata.content_type)
        .header(CACHE_CONTROL, "public, max-age=31536000, immutable") // 1 year
        .body(media.data.to_vec().into())
        .map_err(|e| HttpError::for_internal_error(format!("Failed to build response: {e}")))
}
