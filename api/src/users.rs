//! User endpoints under /users

use std::sync::Arc;

use chrono::NaiveDateTime;
use dropshot::{
    ClientErrorStatusCode, EmptyScanParams, HttpError, HttpResponseDeleted, HttpResponseOk,
    HttpResponseUpdatedNoContent, PaginationParams, Query, RequestContext, ResultsPage, TypedBody,
    WhichPage, endpoint,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::auth::validate_session;
use crate::research_types::FollowedUrlSummary;
use crate::state::AppState;
use crate::types::{Email, ResearchUrlId, UserId};
use crate::validation::{is_unique_violation, validate_email, validate_username};

// ==================== Pagination Types ====================

/// Page selector for following list pagination (cursor-based).
/// Contains the followed_at and id of the last item from the previous page.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct FollowingPageSelector {
    pub followed_at: NaiveDateTime,
    pub id: ResearchUrlId,
}

// ==================== Request/Response Types ====================

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct UserResponse {
    pub user_id: UserId,
    pub username: String,
    pub email: Email,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct UpdateUserRequest {
    /// New username (if updating)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,

    /// New email (if updating)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<Email>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FollowIdPath {
    pub id: ResearchUrlId,
}

// ==================== User Endpoints ====================

/// Get current user
#[endpoint {
    method = GET,
    path = "/users/me",
}]
pub async fn get_me(
    ctx: RequestContext<Arc<AppState>>,
) -> Result<HttpResponseOk<UserResponse>, HttpError> {
    let user_id = validate_session(&ctx)?;
    let state = ctx.context();

    let user = state
        .db
        .get_user(&user_id)
        .await?
        .ok_or_else(|| HttpError::for_internal_error("User not found".to_string()))?;

    Ok(HttpResponseOk(UserResponse {
        user_id: user.id,
        username: user.username,
        email: user.email,
    }))
}

/// Update current user
#[endpoint {
    method = PATCH,
    path = "/users/me",
}]
pub async fn update_me(
    ctx: RequestContext<Arc<AppState>>,
    body: TypedBody<UpdateUserRequest>,
) -> Result<HttpResponseOk<UserResponse>, HttpError> {
    let user_id = validate_session(&ctx)?;
    let state = ctx.context();
    let req = body.into_inner();

    // Update username if provided
    if let Some(username) = &req.username {
        validate_username(username)?;

        if let Err(e) = state.db.update_username(&user_id, username).await {
            if is_unique_violation(&e) {
                return Err(HttpError::for_client_error(
                    None,
                    ClientErrorStatusCode::CONFLICT,
                    "Username already taken".to_string(),
                ));
            }
            return Err(e.into());
        }
    }

    // Update email if provided
    if let Some(email) = &req.email {
        validate_email(email.as_str())?;

        if let Err(e) = state.db.update_email(&user_id, email).await {
            if is_unique_violation(&e) {
                return Err(HttpError::for_client_error(
                    None,
                    ClientErrorStatusCode::CONFLICT,
                    "Email already registered".to_string(),
                ));
            }
            return Err(e.into());
        }
    }

    // Return updated user
    let user = state
        .db
        .get_user(&user_id)
        .await?
        .ok_or_else(|| HttpError::for_internal_error("User not found".to_string()))?;

    Ok(HttpResponseOk(UserResponse {
        user_id: user.id,
        username: user.username,
        email: user.email,
    }))
}

// ==================== Following Endpoints ====================

/// List research URLs the current user follows
#[endpoint {
    method = GET,
    path = "/users/me/following",
}]
pub async fn list_following(
    ctx: RequestContext<Arc<AppState>>,
    query: Query<PaginationParams<EmptyScanParams, FollowingPageSelector>>,
) -> Result<HttpResponseOk<ResultsPage<FollowedUrlSummary>>, HttpError> {
    let user_id = validate_session(&ctx)?;
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
    let cursor_ref = cursor.map(|s| (s.followed_at, &s.id));
    let urls = state
        .db
        .list_followed_urls(&user_id, limit_i64, cursor_ref)
        .await?;

    let items: Vec<FollowedUrlSummary> = urls.into_iter().map(FollowedUrlSummary::from).collect();

    let page = ResultsPage::new(items, &pag_params, |item: &FollowedUrlSummary, _| {
        FollowingPageSelector {
            followed_at: item.followed_at,
            id: item.research_url.id.clone(),
        }
    })
    .map_err(|e| HttpError::for_internal_error(format!("Failed to build results page: {e}")))?;

    Ok(HttpResponseOk(page))
}

/// Follow a research URL
#[endpoint {
    method = PUT,
    path = "/users/me/following/{id}",
}]
pub async fn follow_url(
    ctx: RequestContext<Arc<AppState>>,
    path: dropshot::Path<FollowIdPath>,
) -> Result<HttpResponseUpdatedNoContent, HttpError> {
    let user_id = validate_session(&ctx)?;
    let state = ctx.context();
    let id = &path.into_inner().id;

    // Check URL exists
    if state.db.get_url_by_id(id).await?.is_none() {
        return Err(HttpError::for_not_found(
            None,
            "Research URL not found".to_string(),
        ));
    }

    // Follow it (idempotent - OK if already following)
    state.db.follow_url(&user_id, id).await?;

    Ok(HttpResponseUpdatedNoContent())
}

/// Unfollow a research URL
#[endpoint {
    method = DELETE,
    path = "/users/me/following/{id}",
}]
pub async fn unfollow_url(
    ctx: RequestContext<Arc<AppState>>,
    path: dropshot::Path<FollowIdPath>,
) -> Result<HttpResponseDeleted, HttpError> {
    let user_id = validate_session(&ctx)?;
    let state = ctx.context();
    let id = &path.into_inner().id;

    let unfollowed = state.db.unfollow_url(&user_id, id).await?;

    if !unfollowed {
        return Err(HttpError::for_not_found(
            None,
            "Not following this URL".to_string(),
        ));
    }

    Ok(HttpResponseDeleted())
}
