use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use async_trait::async_trait;
use chronoscope_core::store::memory::{MemoryFactStore, MemoryImageId};
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
    pub cdn_base_url: Url,
}

impl Config {
    /// Load configuration from environment variables
    ///
    /// # Errors
    /// Returns `ConfigError::InvalidBindAddr` if the bind address is invalid, or
    /// `ConfigError::InvalidCdnUrl` if `CDN_BASE_URL` is not a valid base URL.
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

        let cdn_base_url_raw = std::env::var("CDN_BASE_URL")
            .unwrap_or_else(|_| "https://cdn.chronoscope.io".to_string());
        let cdn_base_url = Url::parse(&cdn_base_url_raw)
            .map_err(|e| ConfigError::InvalidCdnUrl(format!("{e}")))?;
        if cdn_base_url.cannot_be_a_base() {
            return Err(ConfigError::InvalidCdnUrl(format!(
                "CDN base URL must be a base URL: {cdn_base_url_raw}"
            )));
        }

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

/// The media-store keys a fact-store image resolved to: the full-resolution
/// original and its JPEG thumbnail. Both are served from our own host via
/// `GET /media/{key}`, so the read path builds `display_url`/`thumbnail_url`
/// with [`crate::cdn::full_url`] over these keys rather than pointing the
/// browser at the upstream source. Built at startup (see the `dev` crate's
/// resolver) and shared read-only through [`AppState::image_media`].
#[derive(Debug, Clone)]
pub struct ResolvedImageMedia {
    pub storage_key: String,
    pub thumbnail_key: String,
}

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("Invalid bind address: {0}")]
    InvalidBindAddr(String),

    #[error("Invalid CDN base URL: {0}")]
    InvalidCdnUrl(String),
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

/// A safe public IP address (example.com) for use in test resolvers.
/// This is non-private and passes SSRF validation.
pub const SAFE_PUBLIC_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34));

/// Create a default DNS resolver using tokio and system configuration.
///
/// # Errors
/// Returns `AppStateError::DnsResolver` if resolver creation fails
/// (e.g., missing `/etc/resolv.conf`).
pub fn default_dns_resolver() -> Result<Box<dyn DnsResolver>, AppStateError> {
    let resolver = Resolver::builder_tokio()
        .map_err(|e| AppStateError::DnsResolver(format!("{e}")))?
        .build();
    Ok(Box::new(resolver))
}

/// A DNS resolver that returns [`SAFE_PUBLIC_IP`] for any hostname.
/// Useful for testing and development environments without system DNS.
struct PermissiveDnsResolver;

#[async_trait]
impl DnsResolver for PermissiveDnsResolver {
    async fn lookup_ip(&self, _host: &str) -> Result<Vec<IpAddr>, HttpError> {
        Ok(vec![SAFE_PUBLIC_IP])
    }
}

/// Create a DNS resolver that returns a safe public IP for any hostname,
/// bypassing real DNS resolution entirely. Suitable for offline tests
/// and environments without `/etc/resolv.conf`.
pub fn permissive_dns_resolver() -> Box<dyn DnsResolver> {
    Box::new(PermissiveDnsResolver)
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
    /// In-memory fact store of submitted entity and image facts.
    pub facts: MemoryFactStore,
    /// Resolved media keys for every fact-store image, keyed by image id. The
    /// entity read path serves thumbnails and detail images from these keys; an
    /// image absent from the map is unresolved and contributes no thumbnail/tile.
    pub image_media: Arc<HashMap<MemoryImageId, ResolvedImageMedia>>,
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
        facts: MemoryFactStore,
        image_media: Arc<HashMap<MemoryImageId, ResolvedImageMedia>>,
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
            facts,
            image_media,
        })
    }
}
