//! Entity API endpoints.
//!
//! Read side of the fact store, with resolved image URLs: markers carry a
//! representative thumbnail and entity detail carries an image grid, both
//! projected from `AppState.facts` (a `MemoryFactStore`) — the single read
//! source. No region clustering.

use std::num::NonZeroUsize;
use std::sync::Arc;

use dropshot::{Body, HttpError, Query, RequestContext, endpoint};
use http::Response;
use schemars::JsonSchema;
use serde::Deserialize;

use chronoscope_api_client::{Bbox, DetailImage, EntityDetail, EntityListCursor, MarkersResponse};
use chronoscope_core::geo;
use chronoscope_core::listing::{self, ListCursor, summaries_in_bbox};
use chronoscope_core::projection::{member_lineage, project_entity, project_image};
use chronoscope_core::store::memory::{MemoryEntityId, MemoryFactStore, MemoryImageId};
use chronoscope_core::store::{FactStore, ImageView};
use chronoscope_core::typed;

use crate::cdn;
use crate::entity_types;
use crate::limits;
use crate::state::AppState;
use crate::validation::{
    bad_request_with_cors, cors_preflight, error_with_cors, fact_store_err, json_with_cors,
    json_with_cors_vary_language,
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

/// A `GET /entities` resume cursor: the fact-store snapshot it was minted
/// against plus the walk position, JSON-encoded into the `cursor` query
/// parameter (opaque to the client, round-tripped verbatim).
type Cursor = ListCursor<EntityListCursor>;

/// Parse the four viewport query fields into the fact store's `Bbox`.
///
/// Shared by `/entities` and `/markers`, whose query params carry the same
/// bbox corners. `chronoscope_api_client::Bbox` range-validates the
/// coordinates (admitting an antimeridian-crossing `min_lon > max_lon` box);
/// the core conversion then only rejects inverted latitude. Either rejection
/// surfaces as a CORS-tagged 400 the browser can read.
fn request_bbox(
    min_lat: f64,
    max_lat: f64,
    min_lon: f64,
    max_lon: f64,
) -> Result<geo::Bbox, HttpError> {
    let bbox = Bbox::new(min_lat, max_lat, min_lon, max_lon)
        .map_err(|e| bad_request_with_cors(format!("Invalid bbox: {e}")))?;
    entity_types::to_core_bbox(&bbox)
        .map_err(|e| bad_request_with_cors(format!("Invalid bbox: {e}")))
}

/// Project an image's `SameArtifact` class to its typed read DTO, or `None` when
/// no fact ever named the id. Shared by the detail grid and the marker thumbnail
/// path; a backend error maps to a 500.
async fn typed_image<V>(
    view: &V,
    image_id: MemoryImageId,
) -> Result<Option<typed::Image<MemoryEntityId, MemoryImageId>>, HttpError>
where
    V: ImageView<MemoryFactStore> + Sync,
{
    let Some((class, projected)) =
        project_image::<MemoryFactStore, _, _>(view, image_id, member_lineage)
            .await
            .map_err(fact_store_err)?
    else {
        return Ok(None);
    };
    Ok(Some(typed::Image::parse(&projected, &class)))
}

// ==================== Path params ====================

// EntityIdPath is required by Dropshot — Path<T> needs a struct with named
// fields matching the URL template parameter. Dropshot's path/query
// deserializer parses each segment via `FromStr` per target type, so a plain
// numeric path segment like `/entities/5` deserializes straight into
// `MemoryEntityId`'s `#[serde(transparent)]` `u64` — no string-path adapter
// needed. (Its `Display` renders the unrelated debug form `entity-5`; that
// never enters the path-parsing path.)
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
    /// Resume cursor from a previous page's `next`, JSON-encoded. Absent for
    /// the first page.
    #[serde(default)]
    pub cursor: Option<String>,
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

    let cursor: Option<Cursor> = match params.cursor {
        None => None,
        Some(s) => match serde_json::from_str(&s) {
            Ok(c) => Some(c),
            Err(e) => {
                return error_with_cors(
                    http::StatusCode::BAD_REQUEST,
                    &format!("Invalid cursor: {e}"),
                );
            }
        },
    };

    let view = state.facts.now().await.map_err(fact_store_err)?;
    let page = match summaries_in_bbox::<MemoryFactStore, _>(&view, &core_bbox, cursor, limit).await
    {
        Ok(p) => p,
        Err(listing::ListError::Backend(e)) => return Err(fact_store_err(e)),
        Err(listing::ListError::SnapshotMismatch) => {
            return error_with_cors(
                http::StatusCode::BAD_REQUEST,
                "cursor is from a stale snapshot; restart the listing without a cursor",
            );
        }
    };

    json_with_cors(&page)
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

    let view = state.facts.now().await.map_err(fact_store_err)?;
    // An id no committed fact ever named projects as `None` — the fact store's
    // "not found", since a real entity carries at least the fact that minted it.
    let Some((class, projected)) =
        project_entity::<MemoryFactStore, _, _>(&view, id, member_lineage)
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
        let Some(image) = typed_image(&view, dep.other).await? else {
            continue;
        };
        let Some(source_url) = image.urls.first().map(|a| a.value.to_string()) else {
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

    let display_name = entity_types::negotiate_name(&entity.names, accept_language(&ctx));
    let detail = EntityDetail {
        entity,
        display_name,
        images,
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

    let view = state.facts.now().await.map_err(fact_store_err)?;
    let page = match summaries_in_bbox::<MemoryFactStore, _>(&view, &core_bbox, None, limit).await {
        Ok(p) => p,
        Err(listing::ListError::Backend(e)) => return Err(fact_store_err(e)),
        // No cursor is ever passed here, and `summaries_in_bbox` only checks
        // snapshot staleness against a supplied cursor — unreachable in
        // practice, handled rather than assumed away.
        Err(listing::ListError::SnapshotMismatch) => {
            return Err(HttpError::for_internal_error(
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
