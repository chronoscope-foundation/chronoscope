//! Typed HTTP client for the Chronoscope API.
//!
//! Two client types:
//! - [`Client`] for unauthenticated / public endpoints
//! - [`AuthClient`] for authenticated endpoints (owns a bearer token)
//!
//! `AuthClient` derefs to `Client`, so it can call any public method.
//! Uses `reqwest` which works on both native (tokio) and WASM (web-sys fetch) targets.

use std::num::NonZeroU32;
use std::ops::Deref;
use std::pin::Pin;

use futures_util::FutureExt;
use futures_util::stream::{self, Stream};

use chronoscope_core::geo::Viewport;

use crate::auth::{
    AuthTokenResponse, LoginFinishRequest, LoginStartRequest, LoginStartResponse,
    RegisterFinishRequest, RegisterStartRequest, RegisterStartResponse,
};
use crate::entities::{Cursor, EntityDetail, EntityImagesPage, MarkersResponse};
use crate::ids::{Email, EntityId, EventId, ImageId, ResearchUrlId};
use crate::pagination::{PageToken, ResultsPage};
use crate::users::{UpdateUserRequest, UserResponse};
use crate::webauthn_types::{
    CredentialCreationOptions, CredentialRequestOptions, PublicKeyCredentialAssertion,
    PublicKeyCredentialAttestation,
};

// ==================== Error ====================

/// Client-side API error.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("HTTP request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("API error (HTTP {status}): {message}")]
    Api { status: u16, message: String },
}

// ==================== Client (unauthenticated) ====================

/// Unauthenticated HTTP client for public API endpoints.
///
/// Cheap to clone (shares the underlying `reqwest::Client` connection pool).
#[derive(Clone, Debug)]
pub struct Client {
    inner: reqwest::Client,
    base_url: String,
}

impl Client {
    /// Create a new unauthenticated client.
    #[must_use]
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            inner: reqwest::Client::new(),
            base_url: base_url.into(),
        }
    }

    /// Create a new client with an existing `reqwest::Client`.
    #[must_use]
    pub fn with_reqwest(inner: reqwest::Client, base_url: impl Into<String>) -> Self {
        Self {
            inner,
            base_url: base_url.into(),
        }
    }

    /// The base URL this client points at.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Access the underlying reqwest client. Exposed for test code that makes
    /// raw HTTP requests for endpoints not yet on the typed client (research/analysis).
    // TODO: remove once all endpoints have typed client methods.
    #[must_use]
    pub fn reqwest_client(&self) -> &reqwest::Client {
        &self.inner
    }

    // ==================== Internal helpers (no auth) ====================

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T, ApiError> {
        let resp = self.inner.get(url).send().await?;
        check_status(resp).await?.json().await.map_err(Into::into)
    }

    async fn post_json<T: serde::de::DeserializeOwned, B: serde::Serialize>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        let resp = self.inner.post(url).json(body).send().await?;
        check_status(resp).await?.json().await.map_err(Into::into)
    }

    // ==================== Entity endpoints (public) ====================

    /// Fetch a single entity by ID. The depicting images are the paginated
    /// [`Self::get_entity_images`] sub-resource, not part of this response.
    pub async fn get_entity(
        &self,
        id: &EntityId,
    ) -> Result<EntityDetail<EntityId, EventId, ImageId>, ApiError> {
        let url = format!("{}/entities/{}", self.base_url, id.as_str());
        self.get_json(&url).await
    }

    /// Fetch one page of the images depicting an entity, resolved into detail
    /// tiles. `cursor` is `None` for the first page, then the previous page's
    /// `next`. The server clamps `limit` to its own maximum.
    pub async fn get_entity_images(
        &self,
        id: &EntityId,
        limit: NonZeroU32,
        cursor: Option<&Cursor>,
    ) -> Result<EntityImagesPage<ImageId>, ApiError> {
        let mut url = format!(
            "{}/entities/{}/images?limit={limit}",
            self.base_url,
            id.as_str(),
        );
        if let Some(cursor) = cursor {
            url.push_str("&cursor=");
            url.push_str(cursor.as_str());
        }
        self.get_json(&url).await
    }

    /// Fetch map markers for a bounding box. Co-located entities (same point)
    /// collapse into one disambiguation marker.
    pub async fn list_markers(
        &self,
        viewport: &Viewport,
    ) -> Result<MarkersResponse<EntityId>, ApiError> {
        let url = format!(
            "{}/markers?min_lat={}&max_lat={}&min_lon={}&max_lon={}",
            self.base_url,
            viewport.min_lat(),
            viewport.max_lat(),
            viewport.min_lon(),
            viewport.max_lon(),
        );
        self.get_json(&url).await
    }

    // ==================== Auth endpoints (public, pre-login) ====================

    /// Start passkey registration flow.
    pub async fn register_start(
        &self,
        req: &RegisterStartRequest,
    ) -> Result<RegisterStartResponse, ApiError> {
        let url = format!("{}/auth/register/start", self.base_url);
        self.post_json(&url, req).await
    }

    /// Complete passkey registration.
    pub async fn register_finish(
        &self,
        req: &RegisterFinishRequest,
    ) -> Result<AuthTokenResponse, ApiError> {
        let url = format!("{}/auth/register/finish", self.base_url);
        self.post_json(&url, req).await
    }

    /// Start passkey login flow.
    pub async fn login_start(
        &self,
        req: &LoginStartRequest,
    ) -> Result<LoginStartResponse, ApiError> {
        let url = format!("{}/auth/login/start", self.base_url);
        self.post_json(&url, req).await
    }

    /// Complete passkey login.
    pub async fn login_finish(
        &self,
        req: &LoginFinishRequest,
    ) -> Result<AuthTokenResponse, ApiError> {
        let url = format!("{}/auth/login/finish", self.base_url);
        self.post_json(&url, req).await
    }
}

// ==================== Auth Flows ====================

/// Error from a complete auth flow (register or login).
///
/// Separates HTTP failures from authenticator failures — the caller's closure
/// determines the authenticator error type `A`.
#[derive(Debug, thiserror::Error)]
pub enum AuthError<A: std::fmt::Debug + std::fmt::Display> {
    /// An HTTP round-trip to the server failed.
    #[error(transparent)]
    Api(#[from] ApiError),
    /// The authenticator (passkey device, browser WebAuthn API, etc.) failed.
    #[error("authenticator error: {0}")]
    Authenticator(A),
}

/// Perform the full passkey registration flow.
///
/// The `sign` closure bridges to the platform authenticator: it receives the
/// server's creation options and must return a signed attestation credential.
/// In tests this wraps `SoftPasskey`; in a browser it calls
/// `navigator.credentials.create()`.
pub async fn register<A, F, Fut>(
    client: &Client,
    username: &str,
    email: &Email,
    sign: F,
) -> Result<AuthClient, AuthError<A>>
where
    A: std::fmt::Debug + std::fmt::Display,
    F: FnOnce(CredentialCreationOptions) -> Fut,
    Fut: std::future::Future<Output = Result<PublicKeyCredentialAttestation, A>>,
{
    let start = client
        .register_start(&RegisterStartRequest {
            username: username.to_string(),
            email: email.clone(),
        })
        .await?;

    let credential = sign(start.options)
        .await
        .map_err(AuthError::Authenticator)?;

    let finish = client
        .register_finish(&RegisterFinishRequest {
            challenge_token: start.challenge_token,
            credential,
            username: username.to_string(),
            email: email.clone(),
        })
        .await?;

    Ok(AuthClient::new(client.clone(), finish.token))
}

/// Perform the full passkey login flow.
///
/// The `sign` closure bridges to the platform authenticator: it receives the
/// server's request options and must return a signed assertion credential.
/// In tests this wraps `SoftPasskey`; in a browser it calls
/// `navigator.credentials.get()`.
pub async fn login<A, F, Fut>(
    client: &Client,
    identifier: &str,
    sign: F,
) -> Result<AuthClient, AuthError<A>>
where
    A: std::fmt::Debug + std::fmt::Display,
    F: FnOnce(CredentialRequestOptions) -> Fut,
    Fut: std::future::Future<Output = Result<PublicKeyCredentialAssertion, A>>,
{
    let start = client
        .login_start(&LoginStartRequest {
            identifier: identifier.to_string(),
        })
        .await?;

    let credential = sign(start.options)
        .await
        .map_err(AuthError::Authenticator)?;

    let finish = client
        .login_finish(&LoginFinishRequest {
            challenge_token: start.challenge_token,
            credential,
        })
        .await?;

    Ok(AuthClient::new(client.clone(), finish.token))
}

// ==================== AuthClient (authenticated) ====================

/// Authenticated HTTP client. Owns a bearer token and sends it with
/// auth-required requests. Derefs to [`Client`] for public endpoints.
#[derive(Clone)]
pub struct AuthClient {
    client: Client,
    token: String,
}

impl AuthClient {
    /// Create an authenticated client from an unauthenticated one and a bearer token.
    pub fn new(client: Client, token: impl Into<String>) -> Self {
        Self {
            client,
            token: token.into(),
        }
    }

    /// The bearer token. Exposed for test code that makes raw HTTP requests
    /// for endpoints not yet on the typed client (research/analysis).
    // TODO: remove once all endpoints have typed client methods.
    pub fn token(&self) -> &str {
        &self.token
    }

    // ==================== Internal helpers (with auth) ====================

    /// Attach the bearer token to a request builder.
    fn authed(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        builder.bearer_auth(&self.token)
    }

    async fn get_json_auth<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
    ) -> Result<T, ApiError> {
        let resp = self.authed(self.client.inner.get(url)).send().await?;
        check_status(resp).await?.json().await.map_err(Into::into)
    }

    #[allow(dead_code)] // will be used when more authenticated endpoints are added
    async fn post_json_auth<T: serde::de::DeserializeOwned, B: serde::Serialize>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        let resp = self
            .authed(self.client.inner.post(url))
            .json(body)
            .send()
            .await?;
        check_status(resp).await?.json().await.map_err(Into::into)
    }

    async fn patch_json_auth<T: serde::de::DeserializeOwned, B: serde::Serialize>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        let resp = self
            .authed(self.client.inner.patch(url))
            .json(body)
            .send()
            .await?;
        check_status(resp).await?.json().await.map_err(Into::into)
    }

    async fn put_auth(&self, url: &str) -> Result<reqwest::Response, ApiError> {
        let resp = self.authed(self.client.inner.put(url)).send().await?;
        check_status(resp).await
    }

    async fn delete_auth(&self, url: &str) -> Result<reqwest::Response, ApiError> {
        let resp = self.authed(self.client.inner.delete(url)).send().await?;
        check_status(resp).await
    }

    // ==================== Auth-required endpoints ====================

    /// Get the current authenticated user.
    pub async fn get_me(&self) -> Result<UserResponse, ApiError> {
        let url = format!("{}/users/me", self.client.base_url);
        self.get_json_auth(&url).await
    }

    /// Update the current authenticated user.
    pub async fn update_me(&self, req: &UpdateUserRequest) -> Result<UserResponse, ApiError> {
        let url = format!("{}/users/me", self.client.base_url);
        self.patch_json_auth(&url, req).await
    }

    /// Follow a research URL.
    pub async fn follow(&self, id: &ResearchUrlId) -> Result<(), ApiError> {
        let url = format!("{}/users/me/following/{id}", self.client.base_url);
        self.put_auth(&url).await?;
        Ok(())
    }

    /// Unfollow a research URL.
    pub async fn unfollow(&self, id: &ResearchUrlId) -> Result<(), ApiError> {
        let url = format!("{}/users/me/following/{id}", self.client.base_url);
        self.delete_auth(&url).await?;
        Ok(())
    }
}

impl Deref for AuthClient {
    type Target = Client;
    fn deref(&self) -> &Client {
        &self.client
    }
}

impl std::fmt::Debug for AuthClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthClient")
            .field("client", &self.client)
            .field("token", &"[redacted]")
            .finish()
    }
}

// ==================== Pagination ====================

/// Where the pagination stream is in the fetch cycle.
enum PageFetch {
    /// Haven't fetched anything yet — fetch the first page.
    First,
    /// Fetch the next page using this token.
    Next(PageToken),
    /// No more pages to fetch.
    Done,
}

/// Create a stream of individual items from a paginated API endpoint.
///
/// `fetch_page` is called with `None` for the first page, then `Some(&PageToken)`
/// for subsequent pages. The stream yields individual items and stops when there
/// are no more pages or an error occurs.
///
/// On native targets the returned stream and all closure/future bounds must be
/// `Send` so callers can pass the stream to `tokio::spawn`. On WASM, the async
/// runtime (`wasm_bindgen_futures`) uses `Rc<RefCell<>>` internally, which is
/// `!Send`, so any `Send` requirement on futures makes the whole thing fail to
/// compile.
///
/// We use a macro rather than a helper function because:
///   - A `+ Send` bound on a `dyn` return type is part of the *type*, not just
///     a where-clause. `Box<dyn Stream + Send>` is a different type from
///     `Box<dyn Stream>` — you can't coerce the latter into the former.
///   - A shared inner function without `Send` would return `Box<dyn Stream>`,
///     which the native wrapper can't upcast to `Box<dyn Stream + Send>`.
///   - Duplicating the function body in two `cfg` blocks is fragile and hard to
///     keep in sync.
///
/// The macro stamps out one copy of the body with the right bounds per target.
macro_rules! define_paginate {
    ( $( + $marker:ident )? ) => {
        pub fn paginate<'a, T, F, Fut>(
            fetch_page: F,
        ) -> Pin<Box<dyn Stream<Item = Result<T, ApiError>> $( + $marker )? + 'a>>
        where
            T: $( $marker + )? 'a,
            F: Fn(Option<&PageToken>) -> Fut + $( $marker + )? 'a,
            Fut: std::future::Future<Output = Result<ResultsPage<T>, ApiError>> + $( $marker + )? 'a,
        {
            type State<T> = (std::vec::IntoIter<T>, PageFetch);

            Box::pin(stream::try_unfold(
                (Vec::new().into_iter(), PageFetch::First) as State<T>,
                move |(mut items, fetch_state)| {
                    // If we have buffered items, yield the next one.
                    if let Some(item) = items.next() {
                        return std::future::ready(Ok(Some((item, (items, fetch_state))))).left_future();
                    }

                    // Items exhausted — fetch the next page if available.
                    let page_token = match &fetch_state {
                        PageFetch::First => None,
                        PageFetch::Next(token) => Some(token),
                        PageFetch::Done => {
                            return std::future::ready(Ok(None)).left_future();
                        }
                    };

                    let fut = fetch_page(page_token);
                    async move {
                        let page = fut.await?;
                        let next = match page.next_page {
                            Some(token) => PageFetch::Next(token),
                            None => PageFetch::Done,
                        };
                        let mut items = page.items.into_iter();
                        match items.next() {
                            Some(item) => Ok(Some((item, (items, next)))),
                            None => Ok(None), // empty page = done
                        }
                    }
                    .right_future()
                },
            ))
        }
    };
}

#[cfg(not(target_arch = "wasm32"))]
define_paginate!(+ Send);

#[cfg(target_arch = "wasm32")]
define_paginate!();

// ==================== Helpers ====================

/// Check HTTP response status and return an error for non-success codes.
async fn check_status(resp: reqwest::Response) -> Result<reqwest::Response, ApiError> {
    if resp.status().is_success() {
        Ok(resp)
    } else {
        Err(ApiError::Api {
            status: resp.status().as_u16(),
            message: resp.text().await.unwrap_or_default(),
        })
    }
}
