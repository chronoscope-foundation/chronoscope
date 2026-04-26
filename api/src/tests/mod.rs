//! Integration tests for the Chronoscope API
//!
//! These tests use an in-memory SQLite database and simulate WebAuthn flows
//! using the `SoftPasskey` authenticator.

mod auth;
#[cfg(feature = "embedded-media")]
mod media;
mod research;
mod user;
mod well_known;

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chronoscope_api_client::client::{ApiError, AuthClient};
#[cfg(feature = "embedded-media")]
use chronoscope_db::media_store::InMemoryMediaStore;
use chronoscope_db::{
    Database, Email, MediaData, MediaId, MediaSlot, MediaType, PageData, PageId, ResearchUrlId,
    ResearchUrlStatus, UserId,
};
use chronoscope_integrations::IntegrationName;
use dropshot::{
    ApiDescription, ConfigDropshot, ConfigLogging, ConfigLoggingLevel, HttpError,
    HttpServerStarter, ResultsPage,
};
use reqwest::Response;

use serde::Serialize;
use url::Url;
use webauthn_authenticator_rs::prelude::*;
use webauthn_authenticator_rs::softpasskey::SoftPasskey;

use crate::auth::{
    LoginFinishRequest, LoginStartRequest, RegisterFinishRequest, RegisterStartRequest,
};
use crate::jwt::JwtConfig;
use crate::research::{SubmitResearchRequest, SubmitResearchResponse};
use crate::research_types::{FollowedUrlSummary, ResearchUrlDossier, ResearchUrlSummary};
use crate::state::{AppState, Config, DnsResolver, SAFE_PUBLIC_IP};

// ==================== Test Utilities ====================

/// Build a path with optional query string for test requests.
fn path_with_query(base: &str, query: &str) -> String {
    if query.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{query}")
    }
}

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
            // Permissive mode: return a safe public IP for any host
            Ok(vec![SAFE_PUBLIC_IP])
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
    client: chronoscope_api_client::Client,
    app_state: Arc<AppState>,
    /// Kept alive to maintain the server running for the duration of the test.
    /// The server runs in a background task and is dropped when `TestContext` is dropped.
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

    #[allow(dead_code)]
    async fn with_dns_resolver(
        resolver: TestResolver,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Self::with_options(None, None, Some(resolver)).await
    }

    /// Create a test context with an in-memory media store for testing /media endpoints.
    #[cfg(feature = "embedded-media")]
    async fn with_media_store()
    -> Result<(Self, Arc<InMemoryMediaStore>), Box<dyn std::error::Error + Send + Sync>> {
        let media_store = Arc::new(InMemoryMediaStore::new());
        let ctx = Self::with_options_and_media(None, None, None, Some(media_store.clone())).await?;
        Ok((ctx, media_store))
    }

    async fn with_options(
        ios_app_id: Option<String>,
        jwt_config: Option<JwtConfig>,
        dns_resolver: Option<TestResolver>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        #[cfg(feature = "embedded-media")]
        return Self::with_options_and_media(ios_app_id, jwt_config, dns_resolver, None).await;
        #[cfg(not(feature = "embedded-media"))]
        return Self::with_options_and_media(ios_app_id, jwt_config, dns_resolver).await;
    }

    async fn with_options_and_media(
        ios_app_id: Option<String>,
        jwt_config: Option<JwtConfig>,
        dns_resolver: Option<TestResolver>,
        #[cfg(feature = "embedded-media")] media_store: Option<Arc<InMemoryMediaStore>>,
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

        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        drop(listener);

        let config = Config {
            database_url: "sqlite::memory:".to_string(),
            rp_id: "localhost".to_string(),
            rp_origin: format!("http://localhost:{}", addr.port()),
            bind_addr: addr,
            ios_app_id,
            cdn_base_url: crate::cdn::tests::TEST_CDN_BASE_URL.to_string(),
        };

        let db =
            Database::new(&config.database_url, &chronoscope_db::resolve_regions_db()?).await?;

        #[cfg(feature = "embedded-media")]
        let app_state = {
            let store = media_store.unwrap_or_else(|| Arc::new(InMemoryMediaStore::new()));
            AppState::new(db, config, jwt, Box::new(resolver), store).await?
        };
        #[cfg(not(feature = "embedded-media"))]
        let app_state = AppState::new(db, config, jwt, Box::new(resolver)).await?;

        let app_state = Arc::new(app_state);

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

        let base_url = format!("http://localhost:{}", addr.port());
        let client = chronoscope_api_client::Client::new(base_url);

        Ok(Self {
            client,
            app_state,
            server,
        })
    }

    fn origin(&self) -> Result<Url, url::ParseError> {
        Url::parse(self.client.base_url())
    }

    /// Direct access to the database for testing DB layer error paths.
    fn db(&self) -> &Database {
        &self.app_state.db
    }

    // ==================== Raw HTTP Helpers ====================
    //
    // These are used for endpoints not yet on the typed client, and for tests
    // that need to check raw HTTP status codes on error responses.

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.client.base_url(), path)
    }

    async fn get(&self, path: &str) -> reqwest::Result<Response> {
        self.client
            .reqwest_client()
            .get(self.url(path))
            .send()
            .await
    }

    async fn get_auth(&self, path: &str, auth: &AuthClient) -> reqwest::Result<Response> {
        self.client
            .reqwest_client()
            .get(self.url(path))
            .bearer_auth(auth.token())
            .send()
            .await
    }

    async fn post_auth<T: Serialize>(
        &self,
        path: &str,
        auth: &AuthClient,
        body: &T,
    ) -> reqwest::Result<Response> {
        self.client
            .reqwest_client()
            .post(self.url(path))
            .bearer_auth(auth.token())
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

    /// Generate a unique email for testing, derived from a unique username.
    fn unique_email() -> Email {
        Email::new(format!("{}@test.example.com", Self::unique_username()))
    }

    /// Register with the given authenticator and username, return an `AuthClient`
    async fn register_with_username(
        &self,
        authenticator: &mut Authenticator,
        username: &str,
    ) -> Result<AuthClient, Box<dyn std::error::Error + Send + Sync>> {
        let email = Email::new(format!("{username}@test.example.com"));
        let origin = self.origin()?;
        let auth = chronoscope_api_client::register(
            &self.client,
            username,
            &email,
            |options| async move {
                let ccr: webauthn_rs::prelude::CreationChallengeResponse =
                    serde_json::from_value(serde_json::to_value(&options)?)?;
                let credential = authenticator
                    .do_registration(origin.clone(), ccr)
                    .map_err(|e| format!("Registration failed: {e:?}"))?;
                let result = serde_json::from_value(serde_json::to_value(&credential)?)?;
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(result)
            },
        )
        .await?;
        Ok(auth)
    }

    /// Register with the given authenticator (auto-generated username), return an `AuthClient`
    async fn register(
        &self,
        authenticator: &mut Authenticator,
    ) -> Result<AuthClient, Box<dyn std::error::Error + Send + Sync>> {
        self.register_with_username(authenticator, &Self::unique_username())
            .await
    }

    /// Login with username/email and authenticator, return an `AuthClient`
    async fn login(
        &self,
        identifier: &str,
        authenticator: &mut Authenticator,
    ) -> Result<AuthClient, Box<dyn std::error::Error + Send + Sync>> {
        let origin = self.origin()?;
        let auth = chronoscope_api_client::login(&self.client, identifier, |options| async move {
            let rcr: webauthn_rs::prelude::RequestChallengeResponse =
                serde_json::from_value(serde_json::to_value(&options)?)?;
            let credential = authenticator
                .do_authentication(origin.clone(), rcr)
                .map_err(|e| format!("Authentication failed: {e:?}"))?;
            let result = serde_json::from_value(serde_json::to_value(&credential)?)?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(result)
        })
        .await?;
        Ok(auth)
    }

    /// Convenience: register with a new authenticator and return an `AuthClient`
    async fn register_and_get_auth(
        &self,
    ) -> Result<AuthClient, Box<dyn std::error::Error + Send + Sync>> {
        self.register(&mut Self::new_authenticator()).await
    }

    // ==================== Research Helpers ====================

    async fn add_research(
        &self,
        auth: &AuthClient,
        url: &str,
    ) -> Result<ResearchUrlId, Box<dyn std::error::Error + Send + Sync>> {
        let req = SubmitResearchRequest {
            url: url.to_string(),
        };
        let resp = self.post_auth("/research", auth, &req).await?;
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
    ) -> Result<ResultsPage<ResearchUrlSummary>, Box<dyn std::error::Error + Send + Sync>> {
        let path = path_with_query("/research", query);
        Ok(self.get(&path).await?.json().await?)
    }

    /// List URLs the user is following
    async fn list_following(
        &self,
        auth: &AuthClient,
        query: &str,
    ) -> Result<ResultsPage<FollowedUrlSummary>, Box<dyn std::error::Error + Send + Sync>> {
        let path = path_with_query("/users/me/following", query);
        Ok(self.get_auth(&path, auth).await?.json().await?)
    }

    /// Create an authenticated client for a new user
    async fn new_user(&self) -> Result<TestClient<'_>, Box<dyn std::error::Error + Send + Sync>> {
        let mut authenticator = Self::new_authenticator();
        let auth = self.register(&mut authenticator).await?;
        Ok(TestClient {
            ctx: self,
            auth,
            authenticator,
        })
    }

    /// Set the `created_at` timestamp for a research URL (for testing pagination ordering)
    async fn set_research_timestamp(
        &self,
        id: &ResearchUrlId,
        timestamp: chrono::NaiveDateTime,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        sqlx::query("UPDATE research_urls SET created_at = ? WHERE id = ?")
            .bind(timestamp)
            .bind(id.as_str())
            .execute(self.app_state.db.pool_ref())
            .await?;
        Ok(())
    }

    // ==================== Content Creation Helpers (for testing dossier) ====================
    //
    // NOTE: These helpers call the database directly rather than through HTTP endpoints
    // because the worker APIs don't exist yet. When worker endpoints are added, consider
    // updating these tests to use the actual API flow.
    // TODO: Revisit when implementing worker HTTP endpoints

    /// Create a test page and link it to a research URL
    async fn create_page_for_url(
        &self,
        url_id: &ResearchUrlId,
        page_data: &PageData,
    ) -> Result<PageId, Box<dyn std::error::Error + Send + Sync>> {
        let page_id = self.app_state.db.create_page(page_data).await?;
        self.app_state
            .db
            .mark_url_resolved_to_page(url_id, &page_id)
            .await?;
        Ok(page_id)
    }

    /// Create test media and link it to a research URL
    async fn create_media_for_url(
        &self,
        url_id: &ResearchUrlId,
        media_data: &MediaData,
    ) -> Result<MediaId, Box<dyn std::error::Error + Send + Sync>> {
        let media_id = self.app_state.db.get_or_create_media(media_data).await?;
        self.app_state
            .db
            .mark_url_resolved_to_media(url_id, &media_id)
            .await?;
        Ok(media_id)
    }

    /// Mark a research URL as failed
    async fn mark_url_failed(
        &self,
        url_id: &ResearchUrlId,
        error_message: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.app_state
            .db
            .url_queue_generic
            .mark_failed(url_id, error_message, None)
            .await?;
        Ok(())
    }

    /// Helper to create a simple test page data
    fn test_page_data(source_type: IntegrationName, media_urls: &[&str]) -> PageData {
        PageData {
            source_type,
            title: Some("Test Post".to_string()),
            author: Some("testuser".to_string()),
            published: None,
            content: Some("This is test content".to_string()),
            fetched_at: chrono::Utc::now().naive_utc(),
            media: media_urls
                .iter()
                .map(|url| MediaSlot::pending(*url))
                .collect(),
        }
    }

    /// Helper to create simple test media data
    fn test_media_data(hash: &[u8]) -> MediaData {
        MediaData {
            exact_hash: hash.to_vec(),
            perceptual_hash: None,
            storage_key: format!("test/{}.jpg", URL_SAFE_NO_PAD.encode(hash)),
            media_type: MediaType::Image,
            width: 800,
            height: 600,
            duration_seconds: None,
            captured: None,
            location: None,
            source_metadata: None,
            fetched_at: chrono::Utc::now().naive_utc(),
        }
    }
}

/// An authenticated test client for a single user
struct TestClient<'a> {
    ctx: &'a TestContext,
    auth: AuthClient,
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
        self.ctx.add_research(&self.auth, url).await
    }

    async fn list_following(
        &self,
        query: &str,
    ) -> Result<ResultsPage<FollowedUrlSummary>, Box<dyn std::error::Error + Send + Sync>> {
        self.ctx.list_following(&self.auth, query).await
    }

    async fn follow(&self, id: &ResearchUrlId) -> Result<(), ApiError> {
        self.auth.follow(id).await
    }

    async fn unfollow(&self, id: &ResearchUrlId) -> Result<(), ApiError> {
        self.auth.unfollow(id).await
    }
}
