//! Entity API endpoints.
//!
//! Read side of the fact store, with resolved image URLs: markers carry a
//! representative thumbnail and entity detail carries an image grid, both
//! projected from `AppState.facts` (a `ServerFactStore`) — the single read
//! source. No region clustering.

use std::num::NonZeroUsize;
use std::sync::Arc;

use base64::prelude::*;
use dropshot::{HttpError, HttpResponseHeaders, HttpResponseOk, Query, RequestContext, endpoint};
use schemars::JsonSchema;
use serde::Deserialize;

use chronoscope_api_client::{
    Cursor, DetailImage, EntityDetail, EntityImagesPage, EntityListPage, MarkersResponse, Snapshot,
};
use chronoscope_core::conflicts::fact_lineage;
use chronoscope_core::geo;
use chronoscope_core::grammar::ids::FactId;
use chronoscope_core::listing::{self, ListCursor, summaries_in_viewport};
use chronoscope_core::projection::{
    member_lineage, project_entity, project_entity_images, project_image,
};
use chronoscope_core::solvers;
use chronoscope_core::store::{EntityIdOf, EntityView, FactStore, FactView, ImageIdOf, ImageView};
use chronoscope_core::typed;

use crate::cdn;
use crate::entity_types;
use crate::limits;
use crate::state::{
    AppState, ServerEntityId, ServerEventId, ServerFactStore, ServerIds, ServerImageId,
};
use crate::validation::fact_store_err;

/// The `Accept-Language` header value, when present and valid UTF-8. Read off
/// the raw request (mirroring `auth::extract_bearer_token`) so the entity read
/// endpoints can negotiate a single display name per entity.
fn accept_language(ctx: &RequestContext<Arc<AppState>>) -> Option<&str> {
    ctx.request
        .headers()
        .get(http::header::ACCEPT_LANGUAGE)
        .and_then(|v| v.to_str().ok())
}

/// Tag a `200 OK` body with `Vary: Accept-Language`. Both content-negotiated
/// read endpoints (`get_entity`, `list_markers`) pick their display name from
/// the request's `Accept-Language`, so a shared cache must key each negotiated
/// form by that header. The body schema is carried through `T`; the header
/// stays out of the schema.
fn vary_language<T>(body: HttpResponseOk<T>) -> HttpResponseHeaders<HttpResponseOk<T>>
where
    T: JsonSchema + serde::Serialize + Send + Sync + 'static,
{
    let mut response = HttpResponseHeaders::new_unnamed(body);
    response.headers_mut().insert(
        http::header::VARY,
        http::HeaderValue::from_static("Accept-Language"),
    );
    response
}

/// The server-internal resume cursor: the fact-store snapshot the listing was
/// pinned to plus the walk position. Encoded into the opaque wire [`Cursor`]
/// token via [`encode_cursor`], never exposed structurally.
type ListState = ListCursor<(ServerEntityId, FactId)>;

/// The `/entities/{id}/images` resume cursor: the pinned snapshot plus the
/// depiction walk's image-class position. Its own version namespace, so a token
/// minted for the entity listing can't be replayed here.
type ImagesListState = ListCursor<(ServerImageId, FactId)>;

/// Version byte prefixing an encoded entity-listing cursor. A token minted under
/// a different version is rejected rather than misparsed.
const CURSOR_VERSION: u8 = 1;

/// Version byte prefixing an encoded entity-images cursor. Distinct from
/// [`CURSOR_VERSION`] so the two cursor families can't be confused — their walk
/// payloads are shape-identical on the wire (entity and image ids both
/// serialize as decimal strings), so the version byte is the only thing that
/// tells them apart.
const IMAGES_CURSOR_VERSION: u8 = 2;

/// Version byte prefixing an encoded read [`Snapshot`]. Distinct from the two
/// cursor versions so a snapshot token can't be replayed as a cursor (whose
/// walk payload it lacks) nor a cursor mistaken for a snapshot — the version
/// byte is what keeps the three token families apart.
const SNAPSHOT_VERSION: u8 = 3;

/// The opaque-token wire envelope: a `version` byte then the value's JSON form,
/// base64url-encoded. Each token family (cursors, snapshots) owns a version
/// byte, so this is the one place the envelope lives; `noun` names the family
/// for the diagnostic.
fn encode_blob<T: serde::Serialize>(
    version: u8,
    noun: &str,
    value: &T,
) -> Result<String, HttpError> {
    let mut bytes = vec![version];
    serde_json::to_writer(&mut bytes, value)
        .map_err(|e| HttpError::for_internal_error(format!("{noun} encode failed: {e}")))?;
    Ok(BASE64_URL_SAFE_NO_PAD.encode(&bytes))
}

/// Decode an opaque wire token back into its value. A malformed blob or a token
/// from a different `version` is rejected with a 400. The inverse of
/// [`encode_blob`]; `noun` names the family for the diagnostic.
fn decode_blob<T: serde::de::DeserializeOwned>(
    version: u8,
    noun: &str,
    token: &str,
) -> Result<T, HttpError> {
    let bytes = BASE64_URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|e| HttpError::for_bad_request(None, format!("Invalid {noun}: {e}")))?;
    let (&found, payload) = bytes
        .split_first()
        .ok_or_else(|| HttpError::for_bad_request(None, format!("Invalid {noun}: empty token")))?;
    if found != version {
        return Err(HttpError::for_bad_request(
            None,
            format!("Invalid {noun}: unsupported version {found}"),
        ));
    }
    serde_json::from_slice(payload)
        .map_err(|e| HttpError::for_bad_request(None, format!("Invalid {noun}: {e}")))
}

/// Encode a cursor state into the opaque wire token, tagged with its family's
/// `version` byte.
fn encode_cursor_blob<T: serde::Serialize>(version: u8, state: &T) -> Result<Cursor, HttpError> {
    Ok(Cursor::new(encode_blob(version, "cursor", state)?))
}

/// Decode an opaque wire token back into a cursor state, requiring its family's
/// `version` byte.
fn decode_cursor_blob<T: serde::de::DeserializeOwned>(
    version: u8,
    cursor: &Cursor,
) -> Result<T, HttpError> {
    decode_blob(version, "cursor", cursor.as_str())
}

/// Encode the entity-listing resume cursor.
fn encode_cursor(state: &ListState) -> Result<Cursor, HttpError> {
    encode_cursor_blob(CURSOR_VERSION, state)
}

/// Decode the entity-listing resume cursor.
fn decode_cursor(cursor: &Cursor) -> Result<ListState, HttpError> {
    decode_cursor_blob(CURSOR_VERSION, cursor)
}

/// Encode the entity-images resume cursor.
fn encode_images_cursor(state: &ImagesListState) -> Result<Cursor, HttpError> {
    encode_cursor_blob(IMAGES_CURSOR_VERSION, state)
}

/// Decode the entity-images resume cursor.
fn decode_images_cursor(cursor: &Cursor) -> Result<ImagesListState, HttpError> {
    decode_cursor_blob(IMAGES_CURSOR_VERSION, cursor)
}

/// Encode a read watermark into the opaque client-facing [`Snapshot`] token.
fn encode_snapshot(id: FactId) -> Result<Snapshot, HttpError> {
    Ok(Snapshot::new(encode_blob(
        SNAPSHOT_VERSION,
        "snapshot",
        &id,
    )?))
}

/// Decode a client-supplied [`Snapshot`] token back into the read watermark it
/// pins. A cursor token (a different version byte) is refused here, never
/// misread as a snapshot.
fn decode_snapshot(snapshot: &Snapshot) -> Result<FactId, HttpError> {
    decode_blob(SNAPSHOT_VERSION, "snapshot", snapshot.as_str())
}

/// Open the read view the four entity endpoints share, reconciling an optional
/// client-supplied `?snapshot=` against a cursor's already-pinned snapshot.
///
/// | `snapshot_param` | `cursor_snapshot` | action |
/// |---|---|---|
/// | `Some(r)` | `Some(c)`, `r != c` | **400** mismatch |
/// | `Some(r)` | `Some(c)`, `r == c` | `no_later_than(c)` |
/// | `None`    | `Some(c)`           | `no_later_than(c)` |
/// | `Some(r)` | `None`              | `r > next_fact_id()` ⇒ **400** future; else `no_later_than(r)` |
/// | `None`    | `None`              | `now()` |
///
/// The future-check reads `next_fact_id()` only on the standalone-snapshot
/// branch: a cursor-derived snapshot is one this server minted, always real.
async fn open_read_view<S: FactStore>(
    facts: &S,
    snapshot_param: Option<Snapshot>,
    cursor_snapshot: Option<FactId>,
) -> Result<S::View<'_>, HttpError> {
    let requested = snapshot_param.as_ref().map(decode_snapshot).transpose()?;
    match (requested, cursor_snapshot) {
        (Some(r), Some(c)) if r != c => Err(HttpError::for_bad_request(
            None,
            format!("snapshot {r} does not match the cursor's pinned snapshot {c}"),
        )),
        // Equal-to-cursor or no explicit snapshot: read at the cursor's snapshot.
        // A cursor is one we minted, so its snapshot is always real — no check.
        (_, Some(c)) => facts.no_later_than(c).await.map_err(fact_store_err),
        (Some(r), None) => {
            // `next_fact_id()` is exact on a single-writer monotonic log. A
            // multi-node backend (Postgres read-replicas) or a rebuilt store makes
            // it node-local, so a snapshot a lagging node hasn't reached reads as
            // future here; pin-aware read routing is the Postgres backend's call.
            let watermark = facts.next_fact_id().await.map_err(fact_store_err)?;
            if r > watermark {
                return Err(HttpError::for_bad_request(
                    None,
                    format!("snapshot {r} is in the future (store watermark {watermark})"),
                ));
            }
            facts.no_later_than(r).await.map_err(fact_store_err)
        }
        (None, None) => facts.now().await.map_err(fact_store_err),
    }
}

/// Parse the four viewport query fields into the fact store's `Viewport`.
///
/// Shared by `/entities` and `/markers`, whose query params carry the same
/// viewport corners. `geo::Viewport::from_coords` range-validates each corner (admitting
/// an antimeridian-crossing `min_lon > max_lon` box) and rejects an inverted
/// latitude span; either rejection is a 400.
fn request_viewport(
    min_lat: f64,
    max_lat: f64,
    min_lon: f64,
    max_lon: f64,
) -> Result<geo::Viewport, HttpError> {
    geo::Viewport::from_coords(min_lat, max_lat, min_lon, max_lon)
        .map_err(|e| HttpError::for_bad_request(None, format!("Invalid viewport: {e}")))
}

/// Project an image's `SameArtifact` class to its typed read DTO, or `None` when
/// no fact ever named the id. Shared by the detail grid and the marker thumbnail
/// path; a backend error maps to a 500.
async fn typed_image<S, V>(
    view: &mut V,
    image_id: ImageIdOf<S>,
) -> Result<Option<typed::Image<EntityIdOf<S>, ImageIdOf<S>>>, HttpError>
where
    S: FactStore,
    V: ImageView<S> + Sync,
{
    let Some((class, projected)) = project_image::<S, _, _>(&mut *view, image_id, member_lineage)
        .await
        .map_err(fact_store_err)?
    else {
        return Ok(None);
    };
    Ok(Some(typed::Image::parse(&projected, &class)))
}

// ==================== Path params ====================

// EntityIdPath is required by Dropshot — Path<T> needs a struct with named
// fields matching the URL template parameter. `ServerEntityId` deserializes
// from an opaque string, so the `/entities/5` segment arrives as the string
// "5" and parses back to the backend's integer — and the generated path-param
// schema is a bare `string`, carrying no backend id type name.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct EntityIdPath {
    pub id: ServerEntityId,
}

// ==================== Endpoints ====================

/// Query parameters for the entity viewport listing endpoint.
///
/// Viewport fields are declared inline because Dropshot's query parameter
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
    /// Pin the read to a previously echoed [`Snapshot`]. Absent reads the live
    /// point; present alongside a `cursor` it must equal the cursor's pinned
    /// snapshot.
    #[serde(default)]
    pub snapshot: Option<Snapshot>,
}

/// List entities within a geographic bounding box (public, no authentication required).
///
/// Returns the placeable entities (those whose current marker resolves to a
/// point) in `viewport`, ordered by the underlying fact-store walk. The primary
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
) -> Result<HttpResponseOk<EntityListPage<ServerEntityId, ServerImageId>>, HttpError> {
    let state = ctx.context();
    let params = query.into_inner();

    let core_viewport = request_viewport(
        params.min_lat,
        params.max_lat,
        params.min_lon,
        params.max_lon,
    )?;

    let requested_limit = params.limit.unwrap_or(limits::ENTITY_LIST_MAX_PAGE_SIZE);
    if requested_limit > limits::ENTITY_LIST_MAX_PAGE_SIZE {
        return Err(HttpError::for_bad_request(
            None,
            format!(
                "Requested page size {requested_limit} exceeds maximum {}",
                limits::ENTITY_LIST_MAX_PAGE_SIZE
            ),
        ));
    }
    let limit = NonZeroUsize::new(requested_limit as usize)
        .ok_or_else(|| HttpError::for_bad_request(None, "limit must be at least 1".to_string()))?;

    let cursor: Option<ListState> = params.cursor.as_ref().map(decode_cursor).transpose()?;

    // Resume reads at the cursor's pinned snapshot; a bare `?snapshot=` pins a
    // fresh read; the first page reads `now()`. The log is append-only, so a
    // pinned snapshot stays readable forever — a cursor is never stale.
    let cursor_snapshot = cursor.as_ref().map(|c| c.snapshot);
    let mut view = open_read_view(&state.facts, params.snapshot, cursor_snapshot).await?;
    let page =
        match summaries_in_viewport::<ServerFactStore, _>(&mut view, &core_viewport, cursor, limit)
            .await
        {
            Ok(p) => p,
            Err(listing::ListError::Backend(e)) => return Err(fact_store_err(e)),
        };

    // The pinned snapshot the walk latched is echoed here and embedded in
    // `page.next`, so the DTO's snapshot and the cursor's can't drift apart.
    let snapshot = encode_snapshot(page.snapshot)?;
    let next = page.next.map(|c| encode_cursor(&c)).transpose()?;
    let response = EntityListPage {
        summaries: page.summaries,
        next,
        snapshot,
    };
    Ok(HttpResponseOk(response))
}

/// Query parameters for the entity detail endpoint.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetEntityQueryParams {
    /// Pin the read to a previously echoed [`Snapshot`]. Absent reads the live
    /// point. The response echoes the point it served, which a client threads
    /// into the images sub-resource so both read one state.
    #[serde(default)]
    pub snapshot: Option<Snapshot>,
}

/// Get a single entity with full detail (public, no authentication required).
///
/// The response is the fact store's typed entity projection plus its negotiated
/// display name. Over-determined date slots surface inline on the entity's own
/// fields as a disputed consensus, carrying their fighting facts. External links
/// live in `entity.external_refs`; the depicting images are the paginated
/// `GET /entities/{id}/images` sub-resource.
#[endpoint {
    method = GET,
    path = "/entities/{id}",
}]
pub async fn get_entity(
    ctx: RequestContext<Arc<AppState>>,
    path: dropshot::Path<EntityIdPath>,
    query: Query<GetEntityQueryParams>,
) -> Result<
    HttpResponseHeaders<HttpResponseOk<EntityDetail<ServerEntityId, ServerEventId, ServerImageId>>>,
    HttpError,
> {
    let state = ctx.context();
    let id = path.into_inner().id;
    let params = query.into_inner();

    let mut view = open_read_view(&state.facts, params.snapshot, None).await?;
    // An id no committed fact ever named projects as `None` — the fact store's
    // "not found", since a real entity carries at least the fact that minted it.
    let Some((class, mut projected)) =
        project_entity::<ServerFactStore, _, _>(&mut view, id, fact_lineage)
            .await
            .map_err(fact_store_err)?
    else {
        return Err(HttpError::for_not_found(
            None,
            "Entity not found".to_string(),
        ));
    };
    // The `fact_lineage` projection carries the whole facts behind each slot, so
    // one read serves both surfaces: `temporal_conflicts` reads the support for
    // cross-field contradictions, then `inject_derived_bounds` folds any inferred
    // "built by" bound into the empty construction slot, and `Entity::parse`
    // flattens the typed view (the inferred bound rides inline, marked derived).
    // The detector runs first, on the asserted slots, so an injected bound never
    // feeds back into it.
    let temporal_conflicts = solvers::temporal_conflicts::<ServerIds>(&projected);
    solvers::inject_derived_bounds::<ServerIds>(&mut projected);
    let entity = typed::Entity::parse(&projected, &class);

    let snapshot = view.snapshot().await.map_err(fact_store_err)?;
    let display_name = entity_types::negotiate_name(&entity.names, accept_language(&ctx));
    let detail = EntityDetail {
        entity,
        display_name,
        temporal_conflicts,
        snapshot: encode_snapshot(snapshot)?,
    };
    Ok(vary_language(HttpResponseOk(detail)))
}

/// Query parameters for the entity images sub-resource.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct EntityImagesQueryParams {
    /// Page size, counting distinct depicted images; required, and the server
    /// clamps it down to `limits::ENTITY_IMAGES_MAX_PAGE_SIZE`. Optional at the
    /// deserialize layer so a missing or zero value reaches the handler and 400s
    /// there, rather than a Dropshot deserialize rejection before it.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Opaque resume cursor from a previous page's `next`. Absent for the first
    /// page.
    #[serde(default)]
    pub cursor: Option<Cursor>,
    /// Pin the read to a previously echoed [`Snapshot`] (typically the entity
    /// detail's, so the grid reads the detail's state). Absent reads the live
    /// point; present alongside a `cursor` it must equal the cursor's pinned
    /// snapshot.
    #[serde(default)]
    pub snapshot: Option<Snapshot>,
}

/// Page the images depicting an entity (public, no authentication required).
///
/// Each depicted image serves its full-resolution original from our own
/// `/media/{key}` as `display_url`, keeping the upstream source URL as
/// `source_url` for the "open original" link. A depiction whose image lacks a
/// `Source` fact, or whose image the resolver never stored, contributes no grid
/// tile — a tile that can't load is worse than an absent one.
///
/// The cursor is snapshot-pinned, mirroring `/entities`: a resume re-opens the
/// view at the cursor's snapshot, so the whole walk reads one stable state. The
/// log is append-only, so that snapshot stays readable forever — a cursor never
/// goes stale.
#[endpoint {
    method = GET,
    path = "/entities/{id}/images",
}]
pub async fn get_entity_images(
    ctx: RequestContext<Arc<AppState>>,
    path: dropshot::Path<EntityIdPath>,
    query: Query<EntityImagesQueryParams>,
) -> Result<HttpResponseOk<EntityImagesPage<ServerImageId>>, HttpError> {
    let state = ctx.context();
    let id = path.into_inner().id;
    let params = query.into_inner();

    // `limit` is required, but declared `Option` so a missing/zero value reaches
    // the handler and 400s there (mirroring `/entities`), rather than a Dropshot
    // deserialize rejection before it. Over-max requests clamp down instead of
    // 400 (the intentional divergence from `/entities`), so a client can ask for
    // "as many as allowed".
    let requested_limit = params
        .limit
        .ok_or_else(|| HttpError::for_bad_request(None, "limit is required".to_string()))?;
    let effective = requested_limit.min(limits::ENTITY_IMAGES_MAX_PAGE_SIZE);
    let limit = NonZeroUsize::new(effective as usize)
        .ok_or_else(|| HttpError::for_bad_request(None, "limit must be at least 1".to_string()))?;

    let cursor: Option<ImagesListState> = params
        .cursor
        .as_ref()
        .map(decode_images_cursor)
        .transpose()?;

    // Resume reads at the cursor's pinned snapshot; a bare `?snapshot=` pins a
    // fresh read; the first page reads `now()`. The log is append-only, so a
    // pinned snapshot stays readable forever — a cursor is never stale.
    let cursor_snapshot = cursor.as_ref().map(|c| c.snapshot);
    let mut view = open_read_view(&state.facts, params.snapshot, cursor_snapshot).await?;
    let snapshot = view.snapshot().await.map_err(fact_store_err)?;
    let after = cursor.map(|c| c.walk);

    let (depictions, next_walk) =
        project_entity_images::<ServerFactStore, _>(&mut view, id, after, limit)
            .await
            .map_err(fact_store_err)?;

    // An empty page hides two cases: an entity that depicts nothing here, or an
    // id no fact ever named. A single-fact backlink probe tells them apart —
    // present facts mean an existing entity with an empty grid (200), an empty
    // probe means no such entity (a 404, mirroring `get_entity`). A non-empty
    // page already proves existence, so the probe runs only when the page comes
    // back empty.
    if depictions.is_empty() {
        let id_facts = view
            .all_facts_about_entity(&id, None, NonZeroUsize::MIN)
            .await
            .map_err(fact_store_err)?;
        if id_facts.items.is_empty() {
            return Err(HttpError::for_not_found(
                None,
                "Entity not found".to_string(),
            ));
        }
    }

    let mut images = Vec::new();
    for dep in &depictions {
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

    let next = next_walk
        .map(|walk| encode_images_cursor(&ListCursor { snapshot, walk }))
        .transpose()?;
    let response = EntityImagesPage {
        images,
        next,
        snapshot: encode_snapshot(snapshot)?,
    };
    Ok(HttpResponseOk(response))
}

// ==================== Unified Markers ====================

/// Query parameters for the unified markers endpoint.
///
/// Viewport fields are declared inline because Dropshot's query parameter
/// deserializer doesn't support `serde(flatten)`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct MarkersQueryParams {
    pub min_lat: f64,
    pub max_lat: f64,
    pub min_lon: f64,
    pub max_lon: f64,
    /// Pin the read to a previously echoed [`Snapshot`]. Absent reads the live
    /// point.
    #[serde(default)]
    pub snapshot: Option<Snapshot>,
}

/// Unified map markers endpoint (public, no authentication required).
///
/// No clustering: every placeable entity in `viewport` becomes a marker, up to
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
) -> Result<HttpResponseHeaders<HttpResponseOk<MarkersResponse<ServerEntityId>>>, HttpError> {
    let state = ctx.context();
    let params = query.into_inner();

    let core_viewport = request_viewport(
        params.min_lat,
        params.max_lat,
        params.min_lon,
        params.max_lon,
    )?;

    let limit = entity_types::max_page_limit()?;

    let mut view = open_read_view(&state.facts, params.snapshot, None).await?;
    let page =
        match summaries_in_viewport::<ServerFactStore, _>(&mut view, &core_viewport, None, limit)
            .await
        {
            Ok(p) => p,
            Err(listing::ListError::Backend(e)) => return Err(fact_store_err(e)),
        };
    let snapshot = page.snapshot;

    // `markers_from_summaries` is pure assembly; the one fact-store read is the
    // batched representative resolution below. Each summary's thumbnail id is a
    // class member; resolving it to the `SameArtifact` representative matches the
    // key the resolver stored under, and a distinct set keeps co-located markers
    // sharing a thumbnail to a single resolution. An unresolved representative or
    // missing media leaves the marker with no thumbnail.
    let assembled = entity_types::markers_from_summaries(page.summaries, accept_language(&ctx));
    let thumbnail_ids: Vec<ServerImageId> = assembled
        .iter()
        .filter_map(|(_, thumbnail)| *thumbnail)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let representatives = view
        .image_representatives(&thumbnail_ids)
        .await
        .map_err(fact_store_err)?;
    let mut markers = Vec::with_capacity(assembled.len());
    for (mut marker, thumbnail) in assembled {
        if let Some(media) = thumbnail
            .and_then(|image_id| representatives.get(&image_id))
            .and_then(|representative| state.image_media.get(representative))
        {
            marker.thumbnail_url = Some(cdn::full_url(
                &state.config.cdn_base_url,
                &media.thumbnail_key,
            ));
        }
        markers.push(marker);
    }
    let truncated = page.next.is_some();

    let response = MarkersResponse {
        markers,
        truncated,
        snapshot: encode_snapshot(snapshot)?,
    };
    Ok(vary_language(HttpResponseOk(response)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rejected cursor is client-supplied input, so it must surface as a
    /// browser-readable 400 rather than a panic or a 500.
    fn assert_rejected_400<T>(result: Result<T, HttpError>) -> Result<(), String> {
        match result {
            Ok(_) => Err("expected a rejected cursor, got a decoded state".to_string()),
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

    #[test]
    fn decode_images_cursor_rejects_an_empty_token() -> Result<(), String> {
        assert_rejected_400(decode_images_cursor(&Cursor::new("")))
    }

    #[test]
    fn decode_images_cursor_rejects_non_base64() -> Result<(), String> {
        assert_rejected_400(decode_images_cursor(&Cursor::new("!!! not base64 !!!")))
    }

    #[test]
    fn decode_images_cursor_rejects_an_unsupported_version_byte() -> Result<(), String> {
        let token = BASE64_URL_SAFE_NO_PAD.encode([IMAGES_CURSOR_VERSION.wrapping_add(1)]);
        assert_rejected_400(decode_images_cursor(&Cursor::new(token)))
    }

    #[test]
    fn decode_images_cursor_rejects_an_entity_listing_cursor() -> Result<(), String> {
        // The entity-listing and images cursors have shape-identical walk
        // payloads on the wire (entity and image ids both serialize as decimal
        // strings), so only the version byte separates them. An `/entities`
        // cursor must be refused by the images decoder, never silently misparsed
        // into an image walk position.
        // Constructed concretely: a raw id mints only from the backend type;
        // the alias flip updates this fixture alongside the store pick.
        let entity_cursor = encode_cursor(&ListCursor {
            snapshot: FactId::new(0),
            walk: (chronoscope_db::SqliteEntityId(1), FactId::new(2)),
        })
        .map_err(|e| format!("{e:?}"))?;
        assert_rejected_400(decode_images_cursor(&entity_cursor))
    }

    #[test]
    fn decode_snapshot_rejects_a_cursor_token() -> Result<(), String> {
        // A cursor and a snapshot are shape-identical opaque strings; only the
        // version byte separates them. An entity-listing cursor (version 1) fed
        // as a snapshot must be refused, never misread as a read watermark.
        // Constructed concretely: a raw id mints only from the backend type.
        let entity_cursor = encode_cursor(&ListCursor {
            snapshot: FactId::new(0),
            walk: (chronoscope_db::SqliteEntityId(1), FactId::new(2)),
        })
        .map_err(|e| format!("{e:?}"))?;
        let as_snapshot = Snapshot::new(entity_cursor.as_str().to_string());
        assert_rejected_400(decode_snapshot(&as_snapshot))
    }

    #[test]
    fn decode_cursor_rejects_a_snapshot_token() -> Result<(), String> {
        // The mirror: a snapshot token (version 3) carries no walk payload, so
        // both cursor decoders must refuse it rather than misparse it into a
        // walk position.
        let snapshot = encode_snapshot(FactId::new(7)).map_err(|e| format!("{e:?}"))?;
        let as_cursor = Cursor::new(snapshot.as_str().to_string());
        assert_rejected_400(decode_cursor(&as_cursor))?;
        assert_rejected_400(decode_images_cursor(&as_cursor))
    }

    #[tokio::test]
    async fn open_read_view_rejects_a_future_snapshot() -> Result<(), String> {
        // On an empty store `next_fact_id()` is 0, so any snapshot past it names
        // facts that don't exist yet. A pinned view echoes its requested bound,
        // so a future value would yield a view that silently grows as writes
        // land — the loud 400 stops that. Runs only on the standalone-snapshot
        // branch (no cursor).
        // An empty store through the server's backend alias — one operation,
        // no held views, so `sqlite::memory:` suffices here.
        let facts = ServerFactStore::open("sqlite::memory:")
            .await
            .map_err(|e| format!("{e:?}"))?;
        let future = encode_snapshot(FactId::new(1)).map_err(|e| format!("{e:?}"))?;
        let result = open_read_view(&facts, Some(future), None).await;
        assert_rejected_400(result.map(|_| ()))
    }
}
