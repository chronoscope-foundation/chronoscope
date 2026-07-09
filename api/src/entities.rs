//! Entity API endpoints.
//!
//! Read side of the fact store, with resolved image URLs: markers carry a
//! representative thumbnail and entity detail carries an image grid, both
//! projected from `AppState.facts` (a `MemoryFactStore`) — the single read
//! source. No region clustering.

use std::num::NonZeroUsize;
use std::sync::Arc;

use base64::prelude::*;
use dropshot::{Body, HttpError, Query, RequestContext, endpoint};
use http::Response;
use schemars::JsonSchema;
use serde::Deserialize;

use chronoscope_api_client::{Cursor, DetailImage, EntityDetail, EntityListPage, MarkersResponse};
use chronoscope_core::conflicts::{cited_lineage, detect_conflicts};
use chronoscope_core::geo;
use chronoscope_core::grammar::ids::FactId;
use chronoscope_core::listing::{self, ListCursor, summaries_in_bbox};
use chronoscope_core::projection::{member_lineage, project_entity, project_image};
use chronoscope_core::store::memory::{MemoryEntityId, MemoryFactStore, MemoryIds, MemoryImageId};
use chronoscope_core::store::{FactStore, ImageView};
use chronoscope_core::typed;

use crate::cdn;
use crate::entity_types;
use crate::limits;
use crate::state::AppState;
use crate::validation::{
    bad_request_with_cors, cors_preflight, error_with_cors, fact_store_err,
    internal_error_with_cors, json_with_cors, json_with_cors_vary_language,
};

/// The `Accept-Language` header value, when present and valid UTF-8. Read off
/// the raw request (mirroring `auth::extract_bearer_token`) so the entity read
/// endpoints can negotiate a single display name per entity.
fn accept_language(ctx: &RequestContext<Arc<AppState>>) -> Option<&str> {
    ctx.request
        .headers()
        .get(http::header::ACCEPT_LANGUAGE)
        .and_then(|v| v.to_str().ok())
}

/// The server-internal resume cursor: the fact-store snapshot the listing was
/// pinned to plus the walk position. Encoded into the opaque wire [`Cursor`]
/// token via [`encode_cursor`], never exposed structurally.
type ListState = ListCursor<(MemoryEntityId, FactId)>;

/// Version byte prefixing an encoded cursor blob. A token minted under a
/// different version is rejected rather than misparsed.
const CURSOR_VERSION: u8 = 1;

/// Encode the internal [`ListState`] into the opaque wire token: a version byte
/// then its JSON form, base64url-encoded.
fn encode_cursor(state: &ListState) -> Result<Cursor, HttpError> {
    let mut bytes = vec![CURSOR_VERSION];
    serde_json::to_writer(&mut bytes, state)
        .map_err(|e| internal_error_with_cors(format!("cursor encode failed: {e}")))?;
    Ok(Cursor::new(BASE64_URL_SAFE_NO_PAD.encode(&bytes)))
}

/// Decode an opaque wire token back into the internal [`ListState`]. A malformed
/// blob or a token from a different [`CURSOR_VERSION`] surfaces as a CORS-tagged
/// 400 the browser can read.
fn decode_cursor(cursor: &Cursor) -> Result<ListState, HttpError> {
    let bytes = BASE64_URL_SAFE_NO_PAD
        .decode(cursor.as_str())
        .map_err(|e| bad_request_with_cors(format!("Invalid cursor: {e}")))?;
    let (&version, payload) = bytes
        .split_first()
        .ok_or_else(|| bad_request_with_cors("Invalid cursor: empty token".to_string()))?;
    if version != CURSOR_VERSION {
        return Err(bad_request_with_cors(format!(
            "Invalid cursor: unsupported version {version}"
        )));
    }
    serde_json::from_slice(payload)
        .map_err(|e| bad_request_with_cors(format!("Invalid cursor: {e}")))
}

/// Parse the four viewport query fields into the fact store's `Bbox`.
///
/// Shared by `/entities` and `/markers`, whose query params carry the same
/// bbox corners. `geo::Bbox::from_coords` range-validates each corner (admitting
/// an antimeridian-crossing `min_lon > max_lon` box) and rejects an inverted
/// latitude span; either rejection surfaces as a CORS-tagged 400 the browser can
/// read.
fn request_bbox(
    min_lat: f64,
    max_lat: f64,
    min_lon: f64,
    max_lon: f64,
) -> Result<geo::Bbox, HttpError> {
    geo::Bbox::from_coords(min_lat, max_lat, min_lon, max_lon)
        .map_err(|e| bad_request_with_cors(format!("Invalid bbox: {e}")))
}

/// Project an image's `SameArtifact` class to its typed read DTO, or `None` when
/// no fact ever named the id. Shared by the detail grid and the marker thumbnail
/// path; a backend error maps to a 500.
async fn typed_image<V>(
    view: &mut V,
    image_id: MemoryImageId,
) -> Result<Option<typed::Image<MemoryEntityId, MemoryImageId>>, HttpError>
where
    V: ImageView<MemoryFactStore> + Sync,
{
    let Some((class, projected)) =
        project_image::<MemoryFactStore, _, _>(&mut *view, image_id, member_lineage)
            .await
            .map_err(fact_store_err)?
    else {
        return Ok(None);
    };
    Ok(Some(typed::Image::parse(&projected, &class)))
}

// ==================== Path params ====================

// EntityIdPath is required by Dropshot — Path<T> needs a struct with named
// fields matching the URL template parameter. `MemoryEntityId` now deserializes
// from an opaque string, so the `/entities/5` segment arrives as the string
// "5" and parses back to the `u64` — and the generated path-param schema is a
// bare `string`, carrying no backend id type name.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct EntityIdPath {
    pub id: MemoryEntityId,
}

// ==================== Endpoints ====================

/// Query parameters for the entity viewport listing endpoint.
///
/// Bbox fields are declared inline because Dropshot's query parameter
/// deserializer doesn't support `serde(flatten)`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct EntitiesQueryParams {
    pub min_lat: f64,
    pub max_lat: f64,
    pub min_lon: f64,
    pub max_lon: f64,
    /// Page size; server clamps to `limits::ENTITY_LIST_MAX_PAGE_SIZE`.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Opaque resume cursor from a previous page's `next`. Absent for the first
    /// page.
    #[serde(default)]
    pub cursor: Option<Cursor>,
}

/// List entities within a geographic bounding box (public, no authentication required).
///
/// Returns the placeable entities (those whose current marker resolves to a
/// point) in `bbox`, ordered by the underlying fact-store walk. The primary
/// live consumer of viewport data is `/markers`; this endpoint keeps a
/// straightforward first-page-plus-cursor shape rather than fully general
/// pagination.
#[endpoint {
    method = GET,
    path = "/entities",
}]
pub async fn list_entities(
    ctx: RequestContext<Arc<AppState>>,
    query: Query<EntitiesQueryParams>,
) -> Result<Response<Body>, HttpError> {
    let state = ctx.context();
    let params = query.into_inner();

    let core_bbox = request_bbox(
        params.min_lat,
        params.max_lat,
        params.min_lon,
        params.max_lon,
    )?;

    let requested_limit = params.limit.unwrap_or(limits::ENTITY_LIST_MAX_PAGE_SIZE);
    if requested_limit > limits::ENTITY_LIST_MAX_PAGE_SIZE {
        return error_with_cors(
            http::StatusCode::BAD_REQUEST,
            &format!(
                "Requested page size {requested_limit} exceeds maximum {}",
                limits::ENTITY_LIST_MAX_PAGE_SIZE
            ),
        );
    }
    let limit = NonZeroUsize::new(requested_limit as usize)
        .ok_or_else(|| bad_request_with_cors("limit must be at least 1".to_string()))?;

    let cursor: Option<ListState> = match params.cursor {
        None => None,
        Some(c) => Some(decode_cursor(&c)?),
    };

    let mut view = state.facts.now().await.map_err(fact_store_err)?;
    let page =
        match summaries_in_bbox::<MemoryFactStore, _>(&mut view, &core_bbox, cursor, limit).await {
            Ok(p) => p,
            Err(listing::ListError::Backend(e)) => return Err(fact_store_err(e)),
            Err(listing::ListError::SnapshotMismatch) => {
                return error_with_cors(
                    http::StatusCode::BAD_REQUEST,
                    "cursor is from a stale snapshot; restart the listing without a cursor",
                );
            }
        };

    let next = page.next.map(|c| encode_cursor(&c)).transpose()?;
    let response = EntityListPage {
        summaries: page.summaries,
        next,
    };
    json_with_cors(&response)
}

/// Get a single entity with full detail (public, no authentication required).
///
/// The response is the fact store's typed entity projection wrapped alongside
/// its resolved detail image grid: each depicted image is projected for its
/// source URL and rendered into a [`DetailImage`]. External links live in
/// `entity.external_refs`.
#[endpoint {
    method = GET,
    path = "/entities/{id}",
}]
pub async fn get_entity(
    ctx: RequestContext<Arc<AppState>>,
    path: dropshot::Path<EntityIdPath>,
) -> Result<Response<Body>, HttpError> {
    let state = ctx.context();
    let id = path.into_inner().id;

    let mut view = state.facts.now().await.map_err(fact_store_err)?;
    // An id no committed fact ever named projects as `None` — the fact store's
    // "not found", since a real entity carries at least the fact that minted it.
    let Some((class, projected)) =
        project_entity::<MemoryFactStore, _, _>(&mut view, id, member_lineage)
            .await
            .map_err(fact_store_err)?
    else {
        return error_with_cors(http::StatusCode::NOT_FOUND, "Entity not found");
    };
    let entity = typed::Entity::parse(&projected, &class);

    // Each depicted image serves its full-resolution original from our own
    // `/media/{key}` as `display_url`, keeping the upstream Commons URL as
    // `source_url` for the "open original" link. A depiction whose image lacks
    // a `Source` fact, or whose image the resolver never stored, contributes no
    // grid tile — a tile that can't load is worse than an absent one.
    let mut images = Vec::new();
    for dep in &entity.depictions {
        let Some(image) = typed_image(&mut view, dep.other).await? else {
            continue;
        };
        let Some(source_url) = image.urls.first().map(|a| a.value.clone()) else {
            continue;
        };
        let Some(media) = state.image_media.get(&image.id) else {
            continue;
        };
        let display_url = cdn::full_url(&state.config.cdn_base_url, &media.storage_key);
        images.push(DetailImage {
            id: dep.other,
            display_url,
            source_url,
            perspective: dep.perspective.settled().copied(),
            medium: image.medium.settled().copied(),
        });
    }

    // This entity's own over-determined date slots. The typed projection above
    // carries citations for the read DTO; the detector reads the whole fighting
    // facts, so the same class is projected again under `cited_lineage`. The id
    // already projected `Some` above, so the cited projection matches; a `None`
    // means the class emptied between the two reads, which carries no conflicts.
    let conflicts = match project_entity::<MemoryFactStore, _, _>(&mut view, id, cited_lineage)
        .await
        .map_err(fact_store_err)?
    {
        Some((_, cited_entity)) => detect_conflicts::<MemoryIds>(&id, &cited_entity),
        None => Vec::new(),
    };

    let display_name = entity_types::negotiate_name(&entity.names, accept_language(&ctx));
    let detail = EntityDetail {
        entity,
        display_name,
        images,
        conflicts,
    };
    json_with_cors_vary_language(&detail)
}

// ==================== Unified Markers ====================

/// Query parameters for the unified markers endpoint.
///
/// Bbox fields are declared inline because Dropshot's query parameter
/// deserializer doesn't support `serde(flatten)`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct MarkersQueryParams {
    pub min_lat: f64,
    pub max_lat: f64,
    pub min_lon: f64,
    pub max_lon: f64,
}

/// Unified map markers endpoint (public, no authentication required).
///
/// No clustering: every placeable entity in `bbox` becomes a marker, up to
/// `limits::ENTITY_LIST_MAX_PAGE_SIZE`, each carrying its representative's
/// thumbnail URL when it depicts an image. Co-located entities (identical
/// point) collapse into one disambiguation marker.
#[endpoint {
    method = GET,
    path = "/markers",
}]
pub async fn list_markers(
    ctx: RequestContext<Arc<AppState>>,
    query: Query<MarkersQueryParams>,
) -> Result<Response<Body>, HttpError> {
    let state = ctx.context();
    let params = query.into_inner();

    let core_bbox = request_bbox(
        params.min_lat,
        params.max_lat,
        params.min_lon,
        params.max_lon,
    )?;

    let limit = entity_types::max_page_limit()?;

    let mut view = state.facts.now().await.map_err(fact_store_err)?;
    let page =
        match summaries_in_bbox::<MemoryFactStore, _>(&mut view, &core_bbox, None, limit).await {
            Ok(p) => p,
            Err(listing::ListError::Backend(e)) => return Err(fact_store_err(e)),
            // No cursor is ever passed here, and `summaries_in_bbox` only checks
            // snapshot staleness against a supplied cursor — unreachable in
            // practice, handled rather than assumed away.
            Err(listing::ListError::SnapshotMismatch) => {
                return Err(internal_error_with_cors(
                    "unexpected snapshot mismatch with no cursor".to_string(),
                ));
            }
        };

    // Serve each marker's representative thumbnail from our own `/media/{key}`.
    // The summary's thumbnail id is a class member; resolving it to the
    // `SameArtifact` representative matches the key the resolver stored under.
    // An unresolved representative leaves the marker with no thumbnail. Marker
    // assembly is pure; the fact-store read lives here.
    let assembled = entity_types::markers_from_summaries(page.summaries, accept_language(&ctx));
    let mut markers = Vec::with_capacity(assembled.len());
    for (mut marker, thumbnail) in assembled {
        if let Some(image_id) = thumbnail {
            let representative = view
                .image_representative(&image_id)
                .await
                .map_err(fact_store_err)?;
            if let Some(media) = state.image_media.get(&representative) {
                marker.thumbnail_url = Some(cdn::full_url(
                    &state.config.cdn_base_url,
                    &media.thumbnail_key,
                ));
            }
        }
        markers.push(marker);
    }
    let truncated = page.next.is_some();

    let response = MarkersResponse { markers, truncated };
    json_with_cors_vary_language(&response)
}

// ==================== CORS Preflight ====================

/// CORS preflight for entity endpoints.
#[endpoint {
    method = OPTIONS,
    path = "/entities",
}]
pub async fn entities_options(
    _ctx: RequestContext<Arc<AppState>>,
) -> Result<Response<Body>, HttpError> {
    cors_preflight()
}

/// CORS preflight for entity detail endpoint.
#[endpoint {
    method = OPTIONS,
    path = "/entities/{id}",
}]
pub async fn entity_options(
    _ctx: RequestContext<Arc<AppState>>,
    _path: dropshot::Path<EntityIdPath>,
) -> Result<Response<Body>, HttpError> {
    cors_preflight()
}

/// CORS preflight for unified markers endpoint.
#[endpoint {
    method = OPTIONS,
    path = "/markers",
}]
pub async fn markers_options(
    _ctx: RequestContext<Arc<AppState>>,
) -> Result<Response<Body>, HttpError> {
    cors_preflight()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rejected cursor is client-supplied input, so it must surface as a
    /// browser-readable 400 rather than a panic or a 500.
    fn assert_rejected_400(result: Result<ListState, HttpError>) -> Result<(), String> {
        match result {
            Ok(_) => Err("expected a rejected cursor, got a decoded ListState".to_string()),
            Err(err) if err.status_code.as_status() == http::StatusCode::BAD_REQUEST => Ok(()),
            Err(err) => Err(format!(
                "expected a 400, got {}",
                err.status_code.as_status()
            )),
        }
    }

    #[test]
    fn decode_cursor_rejects_an_empty_token() -> Result<(), String> {
        // Empty base64 decodes to zero bytes, so there is no version byte to
        // split off — the rejection must not panic on the empty slice.
        assert_rejected_400(decode_cursor(&Cursor::new("")))
    }

    #[test]
    fn decode_cursor_rejects_non_base64() -> Result<(), String> {
        assert_rejected_400(decode_cursor(&Cursor::new("!!! not base64 !!!")))
    }

    #[test]
    fn decode_cursor_rejects_an_unsupported_version_byte() -> Result<(), String> {
        // A well-formed base64 blob whose leading version byte isn't
        // `CURSOR_VERSION` is refused before any payload parse.
        let token = BASE64_URL_SAFE_NO_PAD.encode([CURSOR_VERSION.wrapping_add(1)]);
        assert_rejected_400(decode_cursor(&Cursor::new(token)))
    }
}
