//! Entity API endpoints.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::NaiveDateTime;
use chronoscope_db::EntityId;
use dropshot::{
    Body, HttpError, PaginationParams, Query, RequestContext, ResultsPage, WhichPage, endpoint,
};
use http::Response;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use chronoscope_api_client::Bbox;

use crate::entity_types::{self, EntityResponse, EntitySummary};
use crate::limits;
use crate::state::AppState;
use crate::validation::{cors_preflight, db_err, error_with_cors, json_with_cors};

// ==================== Pagination ====================

/// Page selector for entity pagination (cursor-based).
///
/// Encodes both the keyset cursor and the bbox, since Dropshot only provides
/// scan params on the first page. Subsequent pages need the bbox to filter.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct EntityPageSelector {
    pub updated_at: NaiveDateTime,
    pub id: EntityId,
    #[serde(flatten)]
    pub bbox: Bbox,
}

// ==================== Path params ====================

// EntityIdPath is required by Dropshot — Path<T> needs a struct with named
// fields matching the URL template parameter.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct EntityIdPath {
    pub id: EntityId,
}

// ==================== Endpoints ====================

/// List entities within a geographic bounding box (public, no authentication required).
///
/// All four bounding box parameters are required. Returns entities that have
/// coordinates within the specified rectangle, ordered by most recently updated.
#[endpoint {
    method = GET,
    path = "/entities",
}]
pub async fn list_entities(
    ctx: RequestContext<Arc<AppState>>,
    query: Query<PaginationParams<Bbox, EntityPageSelector>>,
) -> Result<Response<Body>, HttpError> {
    let state = ctx.context();
    let pag_params = query.into_inner();

    let limit = ctx.page_limit(&pag_params)?.get();
    if limit > limits::ENTITY_LIST_MAX_PAGE_SIZE {
        return error_with_cors(
            http::StatusCode::BAD_REQUEST,
            &format!(
                "Requested page size {} exceeds maximum {}",
                limit,
                limits::ENTITY_LIST_MAX_PAGE_SIZE
            ),
        );
    }
    let limit_i64 = i64::from(limit);

    let (bbox, cursor) = match &pag_params.page {
        WhichPage::First(bbox) => (bbox.clone(), None),
        WhichPage::Next(selector) => (
            selector.bbox.clone(),
            Some((selector.updated_at, &selector.id)),
        ),
    };

    let entities = state
        .db
        .list_entities_in_bbox(&bbox, limit_i64, cursor)
        .await
        .map_err(db_err)?;

    let items: Vec<EntitySummary> = entities
        .iter()
        .map(|e| {
            e.to_summary().ok_or_else(|| {
                HttpError::for_internal_error("entity missing coordinates".to_string())
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let page = ResultsPage::new(items, &pag_params, |item: &EntitySummary, _| {
        EntityPageSelector {
            updated_at: item.updated_at,
            id: item.id.clone(),
            bbox: bbox.clone(),
        }
    })
    .map_err(|e| HttpError::for_internal_error(format!("Failed to build results page: {e}")))?;

    json_with_cors(&page)
}

/// Get a single entity with full detail (public, no authentication required).
#[endpoint {
    method = GET,
    path = "/entities/{id}",
}]
pub async fn get_entity(
    ctx: RequestContext<Arc<AppState>>,
    path: dropshot::Path<EntityIdPath>,
) -> Result<Response<Body>, HttpError> {
    let state = ctx.context();
    let id = &path.into_inner().id;

    let (entity_opt, links, annotations, media) = tokio::try_join!(
        async { state.db.find_entity_by_id(id).await.map_err(db_err) },
        async { state.db.find_entity_links(id).await.map_err(db_err) },
        async {
            state
                .db
                .find_annotations_by_entity(id)
                .await
                .map_err(db_err)
        },
        async { state.db.find_media_by_entity(id).await.map_err(db_err) },
    )?;

    let entity =
        entity_opt.ok_or_else(|| HttpError::for_not_found(None, "Entity not found".to_string()))?;

    let cdn_base = &state.config.cdn_base_url;
    let detail = EntityResponse {
        id: entity.id,
        created_at: entity.created_at,
        updated_at: entity.updated_at,
        entity: entity.entity,
        links: links
            .into_iter()
            .map(entity_types::entity_link_summary)
            .collect(),
        annotations: annotations
            .into_iter()
            .map(entity_types::annotation_summary)
            .collect(),
        media: media
            .into_iter()
            .map(|m| entity_types::media_summary(m, cdn_base))
            .collect(),
    };

    json_with_cors(&detail)
}

// ==================== Unified Markers ====================

/// Maximum number of individual entity markers before switching to clusters.
/// Keep low during development (test corpus has 22 entities in ~13 Italian
/// regions); tune upward with real data density.
const ENTITY_MARKER_THRESHOLD: i64 = 10;

/// Maximum number of clusters before trying a coarser zone type.
const CLUSTER_MARKER_LIMIT: i64 = 50;

/// Zone types ordered from coarsest to finest.
const ZONE_TYPES: &[chronoscope_api_client::ZoneType] = &[
    chronoscope_api_client::ZoneType::Country,
    chronoscope_api_client::ZoneType::State,
    chronoscope_api_client::ZoneType::StateDistrict,
    chronoscope_api_client::ZoneType::City,
];

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
/// The server decides whether to return individual entities or region clusters
/// based on data density in the requested bounding box:
/// - If the bbox contains fewer than 10 entities, return them individually.
/// - Otherwise, try cluster zone types from coarsest to finest until one
///   fits under 50 clusters.
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

    let bbox = match Bbox::new(
        params.min_lat,
        params.max_lat,
        params.min_lon,
        params.max_lon,
    ) {
        Ok(b) => b,
        Err(e) => {
            return error_with_cors(http::StatusCode::BAD_REQUEST, &format!("Invalid bbox: {e}"));
        }
    };

    // Check entity density with a cheap threshold count (no full rows).
    let entity_count = state
        .db
        .count_entities_in_bbox(&bbox, ENTITY_MARKER_THRESHOLD)
        .await
        .map_err(db_err)?;

    let (mut markers, rep_map, truncated) = if entity_count < ENTITY_MARKER_THRESHOLD {
        // Few enough — fetch the actual entities as markers.
        let (markers, rep_map) = state
            .db
            .list_entities_as_markers(&bbox, ENTITY_MARKER_THRESHOLD)
            .await
            .map_err(db_err)?;
        (markers, rep_map, false)
    } else {
        // Too many entities — find the finest cluster granularity that fits
        // under the limit. Iterate coarse→fine, tracking the last level that
        // fits. Stop when a level exceeds the limit (finer levels will only
        // produce more clusters). This gives the finest useful granularity.
        // TODO: this is up to 4 sequential count queries. Could be optimized
        // with a single query that counts all zone types at once, or by
        // caching zone type distributions per bbox.
        let mut chosen: Option<chronoscope_api_client::ZoneType> = None;
        for &zone_type in ZONE_TYPES {
            let count = state
                .db
                .count_clusters_in_bbox(&zone_type, &bbox, CLUSTER_MARKER_LIMIT)
                .await
                .map_err(db_err)?;
            if count < CLUSTER_MARKER_LIMIT {
                chosen = Some(zone_type);
            } else {
                break;
            }
        }

        // If no zone type fits (even country has too many clusters), use the
        // coarsest level anyway — it'll be truncated but better than nothing.
        let zone_type = chosen.unwrap_or(ZONE_TYPES[0]);
        let (markers, rep_map) = state
            .db
            .list_clusters_as_markers(&zone_type, &bbox)
            .await
            .map_err(db_err)?;
        let truncated = markers.len() as i64 >= CLUSTER_MARKER_LIMIT;
        (markers, rep_map, truncated)
    };

    // Resolve thumbnail URLs for all markers (entity and cluster alike).
    let entity_ids: Vec<&str> = rep_map.values().map(|id| id.as_str()).collect();
    if !entity_ids.is_empty() {
        let ids_json = serde_json::to_string(&entity_ids)
            .map_err(|e| HttpError::for_internal_error(format!("Failed to serialize IDs: {e}")))?;

        let thumbnails = state
            .db
            .find_thumbnails_for_entities(&ids_json)
            .await
            .map_err(db_err)?;

        let cdn_base = &state.config.cdn_base_url;
        let thumb_map: HashMap<String, String> = thumbnails
            .into_iter()
            .map(|t| {
                let (eid, info) = entity_types::thumbnail_entry(t, cdn_base);
                (eid.to_string(), info.url)
            })
            .collect();

        // Assign thumbnail URLs to markers.
        for marker in &mut markers {
            if let Some(entity_id) = rep_map.get(&marker.id) {
                marker.thumbnail_url = thumb_map.get(&entity_id.to_string()).cloned();
            }
        }
    }

    let response = chronoscope_api_client::MarkersResponse { markers, truncated };
    json_with_cors(&response)
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
