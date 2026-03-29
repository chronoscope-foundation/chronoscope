//! Entity API endpoints.

use std::sync::Arc;

use chrono::NaiveDateTime;
use chronoscope_db::EntityDbId;
use dropshot::{
    Body, HttpError, PaginationParams, Query, RequestContext, ResultsPage, WhichPage, endpoint,
};
use http::Response;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::entity_types::{EntityDetail, EntitySummary};
use crate::limits;
use crate::state::AppState;
use crate::validation::{error_with_cors, cors_preflight, db_err, json_with_cors};

// ==================== Bbox ====================

/// Private deserialization target for bounding box parameters.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
struct RawBbox {
    min_lat: f64,
    max_lat: f64,
    min_lon: f64,
    max_lon: f64,
}

/// A bounding box that has been validated for geographic correctness.
///
/// Can only be constructed via deserialization (which validates automatically)
/// or via the page selector on subsequent pages.
///
/// Invariants:
/// - Latitudes are in `[-90, 90]` and longitudes in `[-180, 180]`
/// - `min_lat <= max_lat` (latitude is never inverted)
/// - `min_lon > max_lon` is allowed (antimeridian crossing)
#[derive(Debug, Clone, Serialize)]
pub struct Bbox {
    min_lat: f64,
    max_lat: f64,
    min_lon: f64,
    max_lon: f64,
}

impl TryFrom<RawBbox> for Bbox {
    type Error = String;

    fn try_from(raw: RawBbox) -> Result<Self, String> {
        if !(-90.0..=90.0).contains(&raw.min_lat) || !(-90.0..=90.0).contains(&raw.max_lat) {
            return Err(format!(
                "Latitudes must be in [-90, 90], got min_lat={} max_lat={}",
                raw.min_lat, raw.max_lat
            ));
        }
        if !(-180.0..=180.0).contains(&raw.min_lon) || !(-180.0..=180.0).contains(&raw.max_lon) {
            return Err(format!(
                "Longitudes must be in [-180, 180], got min_lon={} max_lon={}",
                raw.min_lon, raw.max_lon
            ));
        }
        if raw.min_lat > raw.max_lat {
            return Err(format!(
                "min_lat ({}) must be <= max_lat ({})",
                raw.min_lat, raw.max_lat
            ));
        }
        // min_lon > max_lon is valid — it means the bbox crosses the antimeridian
        Ok(Self {
            min_lat: raw.min_lat,
            max_lat: raw.max_lat,
            min_lon: raw.min_lon,
            max_lon: raw.max_lon,
        })
    }
}

impl<'de> Deserialize<'de> for Bbox {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawBbox::deserialize(deserializer)?;
        Self::try_from(raw).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Bbox {
    fn schema_name() -> String {
        "Bbox".to_string()
    }

    fn json_schema(generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        RawBbox::json_schema(generator)
    }
}

impl Bbox {
    pub fn min_lat(&self) -> f64 {
        self.min_lat
    }
    pub fn max_lat(&self) -> f64 {
        self.max_lat
    }
    pub fn min_lon(&self) -> f64 {
        self.min_lon
    }
    pub fn max_lon(&self) -> f64 {
        self.max_lon
    }
}

// ==================== Pagination ====================

/// Page selector for entity pagination (cursor-based).
///
/// Encodes both the keyset cursor and the bbox, since Dropshot only provides
/// scan params on the first page. Subsequent pages need the bbox to filter.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct EntityPageSelector {
    pub updated_at: NaiveDateTime,
    pub id: EntityDbId,
    #[serde(flatten)]
    pub bbox: Bbox,
}

// ==================== Path params ====================

// EntityIdPath is required by Dropshot — Path<T> needs a struct with named
// fields matching the URL template parameter.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct EntityIdPath {
    pub id: EntityDbId,
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
        WhichPage::Next(selector) => {
            (selector.bbox.clone(), Some((selector.updated_at, &selector.id)))
        }
    };

    let entities = state
        .db
        .list_entities_in_bbox(
            bbox.min_lat(),
            bbox.max_lat(),
            bbox.min_lon(),
            bbox.max_lon(),
            limit_i64,
            cursor,
        )
        .await
        .map_err(db_err)?;

    let items: Vec<EntitySummary> = entities
        .iter()
        .map(|e| {
            EntitySummary::try_from_stored(e)
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

    let detail = EntityDetail {
        id: entity.id,
        earliest_date: entity.temporal_bounds.as_ref().map(|b| b.earliest),
        latest_date: entity.temporal_bounds.as_ref().map(|b| b.latest),
        created_at: entity.created_at,
        updated_at: entity.updated_at,
        entity: entity.entity,
        links: links.into_iter().map(Into::into).collect(),
        annotations: annotations.into_iter().map(Into::into).collect(),
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
