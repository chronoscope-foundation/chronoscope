use std::sync::Arc;

use dropshot::{HttpError, HttpResponseOk, RequestContext, endpoint};
use schemars::JsonSchema;
use serde::Serialize;

use crate::state::AppState;

// ==================== Apple App Site Association ====================
//
// This endpoint enables iOS passkeys (ASAuthorizationController) to work with our API.
// The file must be served at /.well-known/apple-app-site-association with Content-Type: application/json
// Apple requires this to be served over HTTPS with no redirects.
//
// See: https://developer.apple.com/documentation/xcode/supporting-associated-domains

#[derive(Debug, Serialize, JsonSchema)]
pub struct WebCredentials {
    /// List of app identifiers in format "TEAMID.bundleIdentifier"
    pub apps: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AppleAppSiteAssociation {
    pub webcredentials: WebCredentials,
}

/// Apple App Site Association for passkey support
///
/// This endpoint tells iOS which apps are allowed to use passkeys
/// with this domain. Required for `ASAuthorizationController` to work.
#[endpoint {
    method = GET,
    path = "/.well-known/apple-app-site-association",
}]
pub async fn apple_app_site_association(
    ctx: RequestContext<Arc<AppState>>,
) -> Result<HttpResponseOk<AppleAppSiteAssociation>, HttpError> {
    let state = ctx.context();

    let apps = state
        .config
        .ios_app_id
        .as_ref()
        .map(|id| vec![id.clone()])
        .unwrap_or_default();

    Ok(HttpResponseOk(AppleAppSiteAssociation {
        webcredentials: WebCredentials { apps },
    }))
}
