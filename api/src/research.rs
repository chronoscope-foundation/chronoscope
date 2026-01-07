use std::sync::Arc;

use chrono::NaiveDateTime;
use dropshot::{
    Body, EmptyScanParams, HttpError, HttpResponseOk, PaginationParams, Query, RequestContext,
    ResultsPage, TypedBody, WhichPage, endpoint,
};
use http::Response;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::auth::validate_session;
use crate::state::AppState;
use crate::types::ResearchUrlId;
use crate::url_security::validate_url;

// ==================== Pagination Types ====================

/// Page selector for research URL pagination (cursor-based).
/// Contains the `created_at` and id of the last item from the previous page.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ResearchPageSelector {
    pub created_at: NaiveDateTime,
    pub id: ResearchUrlId,
}

// ==================== Request/Response Types ====================

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SubmitResearchRequest {
    /// The URL to submit
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ResearchUrlResponse {
    pub id: ResearchUrlId,
    pub url: String,
    /// When this URL was first submitted to the system
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SubmitResearchResponse {
    pub id: ResearchUrlId,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IdPath {
    pub id: ResearchUrlId,
}

// ==================== Endpoints ====================

/// Submit a research URL (creates if new, auto-follows for the submitter)
///
/// Returns 201 Created if the URL was newly added, or 200 OK if it already existed.
#[endpoint {
    method = POST,
    path = "/research",
}]
pub async fn submit_research(
    ctx: RequestContext<Arc<AppState>>,
    body: TypedBody<SubmitResearchRequest>,
) -> Result<Response<Body>, HttpError> {
    let user_id = validate_session(&ctx)?;
    let state = ctx.context();
    let req = body.into_inner();

    // Validate URL format, scheme, length, and check for SSRF (private IPs, etc.)
    let _validated_url = validate_url(&req.url, &state.dns_resolver).await?;

    let (id, created) = state.db.submit_url(&user_id, &req.url).await?;

    let response = SubmitResearchResponse { id };
    let body_bytes = serde_json::to_vec(&response)
        .map_err(|e| HttpError::for_internal_error(format!("Failed to serialize response: {e}")))?;

    let status = if created {
        http::StatusCode::CREATED
    } else {
        http::StatusCode::OK
    };

    Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(body_bytes.into())
        .map_err(|e| HttpError::for_internal_error(format!("Failed to build response: {e}")))
}

/// List all research URLs (public, no authentication required)
#[endpoint {
    method = GET,
    path = "/research",
}]
pub async fn list_research(
    ctx: RequestContext<Arc<AppState>>,
    query: Query<PaginationParams<EmptyScanParams, ResearchPageSelector>>,
) -> Result<HttpResponseOk<ResultsPage<ResearchUrlResponse>>, HttpError> {
    let state = ctx.context();
    let pag_params = query.into_inner();

    // Get limit from Dropshot's built-in limit handling (respects ?limit=N query param)
    let limit = ctx.page_limit(&pag_params)?.get();

    // Extract cursor from pagination params
    let cursor = match &pag_params.page {
        WhichPage::First(_) => None,
        WhichPage::Next(selector) => Some(selector),
    };

    let limit_i64 = i64::from(limit);

    // Fetch one extra to determine if there are more pages
    let cursor_ref = cursor.map(|s| (s.created_at, &s.id));
    let urls = state.db.list_all_urls(limit_i64 + 1, cursor_ref).await?;

    let has_more = urls.len() > limit as usize;
    let items: Vec<ResearchUrlResponse> = urls
        .into_iter()
        .take(limit as usize)
        .map(|u| ResearchUrlResponse {
            id: u.id.clone(),
            url: u.url,
            created_at: u.created_at,
        })
        .collect();

    // Build the next page token from the last item
    let page = ResultsPage::new(items, &pag_params, |item: &ResearchUrlResponse, _| {
        ResearchPageSelector {
            created_at: item.created_at,
            id: item.id.clone(),
        }
    })
    .map_err(|e| HttpError::for_internal_error(format!("Failed to build results page: {e}")))?;

    // If there are no more items, clear the next_page token
    let page = if has_more {
        page
    } else {
        ResultsPage {
            items: page.items,
            next_page: None,
        }
    };

    Ok(HttpResponseOk(page))
}

/// Get a single research URL (public, no authentication required)
#[endpoint {
    method = GET,
    path = "/research/{id}",
}]
pub async fn get_research(
    ctx: RequestContext<Arc<AppState>>,
    path: dropshot::Path<IdPath>,
) -> Result<HttpResponseOk<ResearchUrlResponse>, HttpError> {
    let state = ctx.context();
    let id = &path.into_inner().id;

    let url = state
        .db
        .get_url_by_id(id)
        .await?
        .ok_or_else(|| HttpError::for_not_found(None, "Research URL not found".to_string()))?;

    Ok(HttpResponseOk(ResearchUrlResponse {
        id: url.id,
        url: url.url,
        created_at: url.created_at,
    }))
}
