//! Integration tests for the Chronoscope API
//!
//! These tests use an in-memory SQLite database and simulate WebAuthn flows
//! using the SoftPasskey authenticator.

mod auth;
mod research;
mod user;
mod well_known;

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use dropshot::HttpError;

use crate::state::DnsResolver;

use dropshot::{
    ApiDescription, ConfigDropshot, ConfigLogging, ConfigLoggingLevel, HttpServerStarter,
    ResultsPage,
};
use reqwest::{Client, Response};
use serde::Serialize;
use url::Url;
use webauthn_authenticator_rs::prelude::*;
use webauthn_authenticator_rs::softpasskey::SoftPasskey;

use crate::auth::{
    AuthTokenResponse, LoginFinishRequest, LoginStartRequest, LoginStartResponse,
    RegisterFinishRequest, RegisterStartRequest, RegisterStartResponse,
};
use crate::jwt::JwtConfig;
use crate::research::{ResearchUrlResponse, SubmitResearchRequest, SubmitResearchResponse};
use crate::state::{AppState, Config};
use crate::types::{Email, ResearchUrlId, UserId};
use crate::users::FollowedUrlResponse;

// ==================== Test DNS Resolver ====================

/// A mock DNS resolver for tests. Returns a safe public IP for any hostname
/// unless explicitly configured with specific mappings.
struct TestResolver(HashMap<String, Vec<IpAddr>>);

impl TestResolver {
    /// Create a resolver that returns a safe public IP for all lookups.
    fn permissive() -> Self {
        Self(HashMap::new())
    }

    /// Create a resolver with specific hostname -> IP mappings.
    /// Hostnames not in the map will fail resolution.
    #[allow(dead_code)]
    fn with_mappings(mappings: HashMap<String, Vec<IpAddr>>) -> Self {
        Self(mappings)
    }
}

#[async_trait]
impl DnsResolver for TestResolver {
    async fn lookup_ip(&self, host: &str) -> Result<Vec<IpAddr>, HttpError> {
        if let Some(ips) = self.0.get(host) {
            Ok(ips.clone())
        } else if self.0.is_empty() {
            // Permissive mode: return example.com's IP for any host
            Ok(vec!["93.184.216.34".parse().expect("valid IP")])
        } else {
            // Strict mode: only configured hosts resolve
            Err(HttpError::for_bad_request(
                None,
                format!("Host not found: {host}"),
            ))
        }
    }
}

// ==================== Test Configuration ====================

// Test defaults (secret must be at least 32 bytes)
const TEST_SECRET: &str = "test-secret-key-for-testing-only-must-be-32-bytes";

// Atomic counter for generating unique test usernames
static TEST_USER_COUNTER: AtomicU32 = AtomicU32::new(0);
const TEST_SESSION_EXPIRY: i64 = 7 * 24 * 60 * 60; // 7 days
const TEST_CHALLENGE_EXPIRY: i64 = 120; // 2 minutes
const TEST_LEEWAY: i64 = 60; // 60 seconds

type Authenticator = WebauthnAuthenticator<SoftPasskey>;
type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

/// Test context that sets up a server with in-memory database
struct TestContext {
    base_url: String,
    client: Client,
    app_state: Arc<AppState>,
    /// Kept alive to maintain the server running for the duration of the test.
    /// The server runs in a background task and is dropped when TestContext is dropped.
    #[allow(dead_code)]
    server: dropshot::HttpServer<Arc<AppState>>,
}

impl TestContext {
    async fn new() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Self::with_options(None, None, None).await
    }

    async fn with_ios_app_id(
        app_id: &str,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Self::with_options(Some(app_id.to_string()), None, None).await
    }

    async fn with_jwt_config(
        jwt: JwtConfig,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Self::with_options(None, Some(jwt), None).await
    }

    #[allow(dead_code)]
    async fn with_dns_resolver(
        resolver: TestResolver,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Self::with_options(None, None, Some(resolver)).await
    }

    async fn with_options(
        ios_app_id: Option<String>,
        jwt_config: Option<JwtConfig>,
        dns_resolver: Option<TestResolver>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let jwt = jwt_config.unwrap_or_else(|| {
            JwtConfig::new(
                TEST_SECRET,
                TEST_SESSION_EXPIRY,
                TEST_CHALLENGE_EXPIRY,
                TEST_LEEWAY,
            )
        });
        let resolver = dns_resolver.unwrap_or_else(TestResolver::permissive);

        // Find an available port
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        drop(listener);

        let config = Config {
            database_url: "sqlite::memory:".to_string(),
            rp_id: "localhost".to_string(),
            rp_origin: format!("http://localhost:{}", addr.port()),
            bind_addr: addr,
            ios_app_id,
        };

        let app_state =
            Arc::new(AppState::new_with_resolver(config, jwt, Box::new(resolver)).await?);

        let config_dropshot = ConfigDropshot {
            bind_address: addr,
            default_request_body_max_bytes: 1024 * 1024,
            default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
            ..Default::default()
        };

        let config_logging = ConfigLogging::StderrTerminal {
            level: ConfigLoggingLevel::Error,
        };
        let log = config_logging.to_logger("test")?;

        let mut api = ApiDescription::new();
        crate::register_api(&mut api)?;

        let server =
            HttpServerStarter::new(&config_dropshot, api, Arc::clone(&app_state), &log)?.start();

        // Use localhost to match RP origin (not 127.0.0.1)
        let base_url = format!("http://localhost:{}", addr.port());
        let client = Client::new();

        Ok(Self {
            base_url,
            client,
            app_state,
            server,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn origin(&self) -> Result<Url, url::ParseError> {
        Url::parse(&self.base_url)
    }

    // ==================== HTTP Helpers ====================

    async fn get(&self, path: &str) -> reqwest::Result<Response> {
        self.client.get(self.url(path)).send().await
    }

    async fn get_auth(&self, path: &str, token: &str) -> reqwest::Result<Response> {
        self.client
            .get(self.url(path))
            .bearer_auth(token)
            .send()
            .await
    }

    async fn post_json<T: Serialize>(&self, path: &str, body: &T) -> reqwest::Result<Response> {
        self.client.post(self.url(path)).json(body).send().await
    }

    async fn post_auth<T: Serialize>(
        &self,
        path: &str,
        token: &str,
        body: &T,
    ) -> reqwest::Result<Response> {
        self.client
            .post(self.url(path))
            .bearer_auth(token)
            .json(body)
            .send()
            .await
    }

    async fn delete_auth(&self, path: &str, token: &str) -> reqwest::Result<Response> {
        self.client
            .delete(self.url(path))
            .bearer_auth(token)
            .send()
            .await
    }

    async fn patch_auth<T: Serialize>(
        &self,
        path: &str,
        token: &str,
        body: &T,
    ) -> reqwest::Result<Response> {
        self.client
            .patch(self.url(path))
            .bearer_auth(token)
            .json(body)
            .send()
            .await
    }

    // ==================== Auth Helpers ====================

    fn new_authenticator() -> Authenticator {
        // falsify_uv=true bypasses user verification requirement
        WebauthnAuthenticator::new(SoftPasskey::new(true))
    }

    /// Generate a unique username for testing
    fn unique_username() -> String {
        let n = TEST_USER_COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("testuser{n}")
    }

    /// Register with the given authenticator and username, return the session token
    async fn register_with_username(
        &self,
        authenticator: &mut Authenticator,
        username: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let start_req = RegisterStartRequest {
            username: username.to_string(),
            email: Email::new(format!("{username}@test.example.com")),
        };
        let start_resp: RegisterStartResponse = self
            .post_json("/auth/register/start", &start_req)
            .await?
            .json()
            .await?;

        let ccr: webauthn_rs::prelude::CreationChallengeResponse =
            serde_json::from_value(serde_json::to_value(&start_resp.options)?)?;

        let credential = authenticator
            .do_registration(self.origin()?, ccr)
            .map_err(|e| format!("Registration failed: {e:?}"))?;

        let finish_req = RegisterFinishRequest {
            challenge_token: start_resp.challenge_token,
            credential: serde_json::from_value(serde_json::to_value(&credential)?)?,
            username: username.to_string(),
            email: Email::new(format!("{username}@test.example.com")),
        };

        let resp = self.post_json("/auth/register/finish", &finish_req).await?;
        assert_eq!(resp.status(), 200);

        Ok(resp.json::<AuthTokenResponse>().await?.token)
    }

    /// Register with the given authenticator (auto-generated username), return the session token
    async fn register(
        &self,
        authenticator: &mut Authenticator,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.register_with_username(authenticator, &Self::unique_username())
            .await
    }

    /// Login with username/email and authenticator, return the session token
    async fn login(
        &self,
        identifier: &str,
        authenticator: &mut Authenticator,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let start_req = LoginStartRequest {
            identifier: identifier.to_string(),
        };
        let start_resp: LoginStartResponse = self
            .post_json("/auth/login/start", &start_req)
            .await?
            .json()
            .await?;

        let rcr: webauthn_rs::prelude::RequestChallengeResponse =
            serde_json::from_value(serde_json::to_value(&start_resp.options)?)?;

        let auth_credential = authenticator
            .do_authentication(self.origin()?, rcr)
            .map_err(|e| format!("Authentication failed: {e:?}"))?;

        let finish_req = LoginFinishRequest {
            challenge_token: start_resp.challenge_token,
            credential: serde_json::from_value(serde_json::to_value(&auth_credential)?)?,
        };

        let resp = self.post_json("/auth/login/finish", &finish_req).await?;
        assert_eq!(resp.status(), 200);

        Ok(resp.json::<AuthTokenResponse>().await?.token)
    }

    /// Convenience: register with a new authenticator and return just the token
    async fn register_and_get_token(
        &self,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.register(&mut Self::new_authenticator()).await
    }

    // ==================== Research Helpers ====================

    async fn add_research(
        &self,
        token: &str,
        url: &str,
    ) -> Result<ResearchUrlId, Box<dyn std::error::Error + Send + Sync>> {
        let req = SubmitResearchRequest {
            url: url.to_string(),
        };
        let resp = self.post_auth("/research", token, &req).await?;
        // 201 = newly created, 200 = already existed
        assert!(
            resp.status() == 200 || resp.status() == 201,
            "Expected 200 or 201, got {}",
            resp.status()
        );
        Ok(resp.json::<SubmitResearchResponse>().await?.id)
    }

    /// List all research URLs (public endpoint)
    async fn list_research(
        &self,
        query: &str,
    ) -> Result<ResultsPage<ResearchUrlResponse>, Box<dyn std::error::Error + Send + Sync>> {
        let path = if query.is_empty() {
            "/research".to_string()
        } else {
            format!("/research?{query}")
        };
        Ok(self.get(&path).await?.json().await?)
    }

    /// List URLs the user is following
    async fn list_following(
        &self,
        token: &str,
        query: &str,
    ) -> Result<ResultsPage<FollowedUrlResponse>, Box<dyn std::error::Error + Send + Sync>> {
        let path = if query.is_empty() {
            "/users/me/following".to_string()
        } else {
            format!("/users/me/following?{query}")
        };
        Ok(self.get_auth(&path, token).await?.json().await?)
    }

    /// Create an authenticated client for a new user
    async fn new_user(&self) -> Result<TestClient<'_>, Box<dyn std::error::Error + Send + Sync>> {
        let mut authenticator = Self::new_authenticator();
        let token = self.register(&mut authenticator).await?;
        Ok(TestClient {
            ctx: self,
            token,
            authenticator,
        })
    }

    /// Set the created_at timestamp for a research URL (for testing pagination ordering)
    async fn set_research_timestamp(
        &self,
        id: &ResearchUrlId,
        timestamp: chrono::NaiveDateTime,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        sqlx::query("UPDATE research_urls SET created_at = ? WHERE id = ?")
            .bind(timestamp)
            .bind(id.as_str())
            .execute(self.app_state.db.pool())
            .await?;
        Ok(())
    }
}

/// An authenticated test client for a single user
struct TestClient<'a> {
    ctx: &'a TestContext,
    token: String,
    /// Kept alive because the authenticator maintains state (private keys, counters)
    /// that must persist across multiple WebAuthn operations in the same test.
    #[allow(dead_code)]
    authenticator: Authenticator,
}

impl TestClient<'_> {
    async fn add_research(
        &self,
        url: &str,
    ) -> Result<ResearchUrlId, Box<dyn std::error::Error + Send + Sync>> {
        self.ctx.add_research(&self.token, url).await
    }

    async fn list_following(
        &self,
        query: &str,
    ) -> Result<ResultsPage<FollowedUrlResponse>, Box<dyn std::error::Error + Send + Sync>> {
        self.ctx.list_following(&self.token, query).await
    }

    async fn follow(&self, id: &ResearchUrlId) -> reqwest::Result<Response> {
        self.ctx
            .client
            .put(self.ctx.url(&format!("/users/me/following/{id}")))
            .bearer_auth(&self.token)
            .send()
            .await
    }

    async fn unfollow(&self, id: &ResearchUrlId) -> reqwest::Result<Response> {
        self.ctx
            .delete_auth(&format!("/users/me/following/{id}"), &self.token)
            .await
    }
}
