use std::net::IpAddr;
#[cfg(feature = "embedded-media")]
use std::sync::Arc;

use async_trait::async_trait;
use chronoscope_db::Database;
#[cfg(feature = "embedded-media")]
use chronoscope_db::media_store::MediaStore;
use dropshot::HttpError;
use hickory_resolver::Resolver;
use hickory_resolver::name_server::TokioConnectionProvider;
use thiserror::Error;
use url::Url;
use webauthn_rs::prelude::*;

use crate::jwt::JwtConfig;

// ==================== DNS Resolution ====================

/// Type alias for the production DNS resolver using tokio.
pub type TokioResolver = hickory_resolver::Resolver<TokioConnectionProvider>;

/// Trait for DNS resolution, allowing mocking in tests.
#[async_trait]
pub trait DnsResolver: Send + Sync {
    /// Resolve a hostname to IP addresses.
    ///
    /// # Errors
    /// Returns `HttpError` if DNS resolution fails.
    async fn lookup_ip(&self, host: &str) -> Result<Vec<IpAddr>, HttpError>;
}

#[async_trait]
impl DnsResolver for TokioResolver {
    async fn lookup_ip(&self, host: &str) -> Result<Vec<IpAddr>, HttpError> {
        let response = hickory_resolver::Resolver::lookup_ip(self, host)
            .await
            .map_err(|e| {
                HttpError::for_bad_request(None, format!("Failed to resolve host: {e}"))
            })?;
        Ok(response.iter().collect())
    }
}

// ==================== Configuration ====================

/// Configuration for the application
pub struct Config {
    /// Database URL (e.g., "sqlite:chronoscope.db" or "`sqlite::memory`:")
    pub database_url: String,

    /// `WebAuthn` Relying Party ID (e.g., "chronoscope.io")
    pub rp_id: String,

    /// `WebAuthn` Relying Party Origin (e.g., "<https://api.chronoscope.io>")
    pub rp_origin: String,

    /// Server bind address
    pub bind_addr: std::net::SocketAddr,

    /// iOS app identifier for AASA (e.g., "ABCD1234.com.example.app")
    /// Set via `IOS_APP_ID` env var, or constructed from `APPLE_TEAM_ID` + `IOS_BUNDLE_ID`
    pub ios_app_id: Option<String>,

    /// CDN base URL for media assets (e.g., `https://cdn.chronoscope.io`)
    pub cdn_base_url: String,
}

impl Config {
    /// Load configuration from environment variables
    ///
    /// # Errors
    /// Returns `ConfigError::InvalidBindAddr` if the bind address is invalid.
    pub fn from_env() -> Result<Self, ConfigError> {
        let database_url =
            std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:chronoscope.db".to_string());

        let rp_id = std::env::var("RP_ID").unwrap_or_else(|_| "localhost".to_string());

        let rp_origin =
            std::env::var("RP_ORIGIN").unwrap_or_else(|_| "http://localhost:3000".to_string());

        let bind_addr = std::env::var("BIND_ADDR")
            .unwrap_or_else(|_| "0.0.0.0:8080".to_string())
            .parse()
            .map_err(|e| ConfigError::InvalidBindAddr(format!("{e}")))?;

        let ios_app_id = std::env::var("IOS_APP_ID").ok();

        let cdn_base_url = std::env::var("CDN_BASE_URL")
            .unwrap_or_else(|_| "https://cdn.chronoscope.io".to_string());

        Ok(Self {
            database_url,
            rp_id,
            rp_origin,
            bind_addr,
            ios_app_id,
            cdn_base_url,
        })
    }
}

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("Invalid bind address: {0}")]
    InvalidBindAddr(String),
}

#[derive(Error, Debug)]
pub enum AppStateError {
    #[error("Configuration error: {0}")]
    Config(#[from] ConfigError),

    #[error("Database error: {0}")]
    Database(#[from] chronoscope_db::DbError),

    #[error("JWT configuration error: {0}")]
    Jwt(#[from] crate::jwt::JwtError),

    #[error("WebAuthn error: {0}")]
    WebAuthn(String),

    #[error("Invalid RP origin URL: {0}")]
    InvalidOrigin(String),

    #[error("DNS resolver error: {0}")]
    DnsResolver(String),
}

/// Create a default DNS resolver using tokio.
///
/// # Errors
/// Returns `AppStateError::DnsResolver` if resolver creation fails.
pub fn default_dns_resolver() -> Result<Box<dyn DnsResolver>, AppStateError> {
    let resolver = Resolver::builder_tokio()
        .map_err(|e| AppStateError::DnsResolver(format!("{e}")))?
        .build();
    Ok(Box::new(resolver))
}

/// Shared application state
pub struct AppState {
    pub db: Database,
    pub jwt: JwtConfig,
    pub webauthn: Webauthn,
    pub dns_resolver: Box<dyn DnsResolver>,
    pub config: Config,
    #[cfg(feature = "embedded-media")]
    pub media_store: Arc<dyn MediaStore>,
}

impl AppState {
    /// Create application state with all components explicitly provided.
    ///
    /// # Errors
    /// Returns `AppStateError` if `WebAuthn` initialization fails.
    #[allow(clippy::unused_async)]
    pub async fn new(
        db: Database,
        config: Config,
        jwt: JwtConfig,
        dns_resolver: Box<dyn DnsResolver>,
        #[cfg(feature = "embedded-media")] media_store: Arc<dyn MediaStore>,
    ) -> Result<Self, AppStateError> {
        let rp_origin = Url::parse(&config.rp_origin)
            .map_err(|e| AppStateError::InvalidOrigin(format!("{e}")))?;

        let webauthn = WebauthnBuilder::new(&config.rp_id, &rp_origin)
            .map_err(|e| AppStateError::WebAuthn(format!("{e}")))?
            .rp_name("Chronoscope")
            .build()
            .map_err(|e| AppStateError::WebAuthn(format!("{e}")))?;

        Ok(Self {
            db,
            jwt,
            webauthn,
            dns_resolver,
            config,
            #[cfg(feature = "embedded-media")]
            media_store,
        })
    }
}
