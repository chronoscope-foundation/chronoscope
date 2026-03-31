//! Entity API endpoints.

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
            entity_types::entity_summary_from_stored(e)
                .map_err(|msg| HttpError::for_internal_error(msg.to_string()))
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

    let (entity_opt, links, annotations) = tokio::try_join!(
        async { state.db.find_entity_by_id(id).await.map_err(db_err) },
        async { state.db.find_entity_links(id).await.map_err(db_err) },
        async {
            state
                .db
                .find_annotations_by_entity(id)
                .await
                .map_err(db_err)
        },
    )?;

    let entity =
        entity_opt.ok_or_else(|| HttpError::for_not_found(None, "Entity not found".to_string()))?;

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
    };

    json_with_cors(&detail)
}

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
