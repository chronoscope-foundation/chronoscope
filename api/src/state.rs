use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use async_trait::async_trait;
use chronoscope_core::store::{EntityIdOf, EventIdOf, FactStore, ImageIdOf};
#[cfg(feature = "postgres")]
use chronoscope_db::PostgresFactStore as PickedFactStore;
#[cfg(not(feature = "postgres"))]
use chronoscope_db::SqliteFactStore as PickedFactStore;
#[cfg(feature = "embedded-media")]
use chronoscope_db::media_store::MediaStore;
use chronoscope_db::{CredentialWatch, Database};
use dropshot::HttpError;
use hickory_resolver::Resolver;
use hickory_resolver::name_server::TokioConnectionProvider;
use thiserror::Error;
use url::Url;
use webauthn_rs::prelude::*;

use crate::jwt::JwtConfig;

// ==================== Fact-store backend ====================

/// The one place the server's fact-store backend is picked. Everything below
/// the endpoint boundary is generic over `S: FactStore` (core listing /
/// projection, ingestion, the api helpers); the handlers and [`AppState`]
/// instantiate at this alias.
///
/// The `postgres` feature picks the backend at compile time: on for the
/// production entry point, off for dev. Only construction differs between the
/// two, since Postgres connects to a URL where SQLite mounts a base and an
/// overlay, so the cfg reaches just this alias, the entry point's two `open`
/// call sites, and the one test fixture that stands a store up
/// (`tests::fresh_fact_store`). Every other module, test code included,
/// compiles under both, and the suite runs under both: default features against
/// SQLite, `checks.api-postgres` against a throwaway Postgres cluster.
pub type ServerFactStore = PickedFactStore;

/// The picked backend's id scheme — what the stored commit types instantiate at.
pub type ServerIds = <ServerFactStore as FactStore>::Ids;

/// The picked backend's entity id: URL path params and listing cursors carry
/// it. Its wire form is an opaque decimal string for every backend.
pub type ServerEntityId = EntityIdOf<ServerFactStore>;

/// The picked backend's lifetime-event id: the value slot the typed entity
/// projection is keyed by.
pub type ServerEventId = EventIdOf<ServerFactStore>;

/// The picked backend's image id: the resolved-media map's key and the
/// images-cursor payload.
pub type ServerImageId = ImageIdOf<ServerFactStore>;

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

/// Where a rejected fact-store location carried its password. Both forms reach
/// sqlx's connect options, so both have to be refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialForm {
    /// `postgres://user:secret@host/db`
    Userinfo,
    /// `postgres://user@host/db?password=secret`
    QueryParameter,
}

impl std::fmt::Display for CredentialForm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Userinfo => "a password in the URL's userinfo",
            Self::QueryParameter => "a \"password\" query parameter",
        })
    }
}

/// Where the fact store lives: a facts-database path under the SQLite backend,
/// a connection URL under the Postgres one. Holds no secret, because
/// [`new`](Self::new) refuses a location that carries one, so the value stays
/// printable wherever it is useful (startup logs, error context).
#[derive(Debug, Clone)]
pub struct FactsDatabase(String);

impl FactsDatabase {
    /// Wrap a configured location.
    ///
    /// # Errors
    /// Returns [`ConfigError::FactsDbCredentials`] if the location is a URL
    /// carrying a password, whether in the userinfo or as a `password` query
    /// parameter.
    pub fn new(raw: impl Into<String>) -> Result<Self, ConfigError> {
        let raw = raw.into();
        // A filesystem path (the SQLite case) is not a URL and has nowhere to
        // put a credential, so it passes straight through.
        if let Ok(url) = Url::parse(&raw) {
            if url.password().is_some() {
                return Err(ConfigError::FactsDbCredentials {
                    form: CredentialForm::Userinfo,
                });
            }
            if url.query_pairs().any(|(name, _)| name == "password") {
                return Err(ConfigError::FactsDbCredentials {
                    form: CredentialForm::QueryParameter,
                });
            }
        }
        Ok(Self(raw))
    }

    /// The raw location, for the backend that connects with it.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for FactsDatabase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Configuration for the application
pub struct Config {
    /// App database URL (e.g., "sqlite:chronoscope.db" or "`sqlite::memory`:").
    /// Holds the app tables (auth, research, media); the fact tables live in
    /// the separate [`facts_database`](Self::facts_database).
    pub database_url: String,

    /// Where the fact tables live. Required, set via `CHRONOSCOPE_FACTS_DB`,
    /// and read by whichever backend [`ServerFactStore`] names: SQLite pins it
    /// as the frozen read-only base (a completed, codec-stamped build) beneath
    /// a fresh writable overlay, so it is validated rather than created or
    /// migrated; Postgres connects to it as a URL whose schema `ingest build-db`
    /// has already migrated.
    pub facts_database: FactsDatabase,

    /// `WebAuthn` Relying Party ID (e.g., "chronoscope.io")
    pub rp_id: String,

    /// `WebAuthn` Relying Party Origin (e.g., "<https://api.chronoscope.io>")
    pub rp_origin: String,

    /// Server bind address. `PORT` (what Cloud Run injects) wins when set and
    /// binds every interface; `BIND_ADDR` carries the full address otherwise.
    pub bind_addr: std::net::SocketAddr,

    /// iOS app identifier for AASA (e.g., "ABCD1234.com.example.app")
    /// Set via `IOS_APP_ID` env var, or constructed from `APPLE_TEAM_ID` + `IOS_BUNDLE_ID`
    pub ios_app_id: Option<String>,

    /// CDN base URL for media assets (e.g., `https://cdn.chronoscope.io`)
    pub cdn_base_url: Url,

    /// The Cloudflare queue a mirror sweep enqueues to, when configured. Present
    /// only when the account, queue, and API token are all set in the
    /// environment — locally, to warm the corpus. Its absence is what keeps the
    /// unauthenticated `/mirror/sweep` endpoint from doing anything where no
    /// token is set (production today), since it refuses before touching the
    /// store.
    pub mirror_queue: Option<crate::mirror::sweep::QueueTarget>,

    /// The shared secret `/mirror/sweep` matches its header against, from
    /// `MIRROR_SWEEP_TOKEN`. Absent, the endpoint refuses every request: with
    /// nothing to match, there is no one it can let through.
    pub mirror_sweep_token: Option<String>,
}

impl Config {
    /// Load configuration from environment variables
    ///
    /// # Errors
    /// Returns `ConfigError::MissingFactsDb` if `CHRONOSCOPE_FACTS_DB` is
    /// unset, `ConfigError::FactsDbCredentials` if it carries a password,
    /// `ConfigError::InvalidBindAddr` if the bind address is invalid, or
    /// `ConfigError::InvalidCdnUrl` if `CDN_BASE_URL` is not a valid base URL.
    pub fn from_env() -> Result<Self, ConfigError> {
        let database_url =
            std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:chronoscope.db".to_string());

        // A required, explicit input so the fact store's source is never
        // inferred. Parsed here, at the edge where it is read, so the
        // credential rule holds for everything downstream.
        let facts_location =
            std::env::var("CHRONOSCOPE_FACTS_DB").map_err(|_| ConfigError::MissingFactsDb)?;
        let facts_database = FactsDatabase::new(facts_location)?;

        let rp_id = std::env::var("RP_ID").unwrap_or_else(|_| "localhost".to_string());

        let rp_origin =
            std::env::var("RP_ORIGIN").unwrap_or_else(|_| "http://localhost:3000".to_string());

        // Cloud Run injects PORT and routes to whatever it names, so reading it
        // makes the image correct on its own rather than through a deploy that
        // passes a matching `--port`. BIND_ADDR stays the knob for every other
        // runtime, where the interface matters as much as the port.
        let bind_addr_raw = match std::env::var("PORT") {
            Ok(port) => format!("0.0.0.0:{port}"),
            Err(_) => std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string()),
        };
        let bind_addr = bind_addr_raw
            .parse()
            .map_err(|e| ConfigError::InvalidBindAddr(format!("{bind_addr_raw}: {e}")))?;

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
            facts_database,
            rp_id,
            rp_origin,
            bind_addr,
            ios_app_id,
            cdn_base_url,
            mirror_queue: mirror_queue_from_env(),
            mirror_sweep_token: std::env::var("MIRROR_SWEEP_TOKEN")
                .ok()
                .filter(|value| !value.is_empty()),
        })
    }
}

/// The mirror queue target, built from the environment when the account, queue,
/// and token are all set and non-empty. All three or nothing: a partial set
/// leaves it `None`, so the sweep endpoint refuses rather than half-run. An
/// empty value counts as unset, so `CLOUDFLARE_API_TOKEN=` does not build a
/// target with an empty bearer token that would fail every send. A partial set
/// is logged, since it is a misconfiguration a local warm needs to see.
fn mirror_queue_from_env() -> Option<crate::mirror::sweep::QueueTarget> {
    let present = |key: &str| std::env::var(key).ok().filter(|value| !value.is_empty());
    let account_id = present("CLOUDFLARE_ACCOUNT_ID");
    let queue_id = present("MIRROR_QUEUE_ID");
    let token = present("CLOUDFLARE_API_TOKEN");
    match (account_id, queue_id, token) {
        (Some(account_id), Some(queue_id), Some(token)) => {
            Some(crate::mirror::sweep::QueueTarget {
                account_id,
                queue_id,
                token,
            })
        }
        (account_id, queue_id, token) => {
            if account_id.is_some() || queue_id.is_some() || token.is_some() {
                // Straight to stderr: `Config::from_env` runs before the slog
                // logger exists, and a startup misconfiguration should be loud.
                eprintln!(
                    "warning: mirror queue partially configured (account_id={}, queue_id={}, token={}); /mirror/sweep will refuse until CLOUDFLARE_ACCOUNT_ID, MIRROR_QUEUE_ID, and CLOUDFLARE_API_TOKEN are all set and non-empty",
                    account_id.is_some(),
                    queue_id.is_some(),
                    token.is_some(),
                );
            }
            None
        }
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

/// Placeholder-mode media key for a fact-store image's original — the layout
/// the dev resolver writes and `GET /media/{key}` serves back. A single path
/// segment under `media/`, so the route matches it. Exported so the resolver
/// and its test mirrors share one definition.
pub fn placeholder_storage_key(image_id: impl std::fmt::Display) -> String {
    format!("media/factimg-{image_id}.jpg")
}

/// Placeholder-mode media key for a fact-store image's thumbnail — the twin
/// of [`placeholder_storage_key`].
pub fn placeholder_thumbnail_key(image_id: impl std::fmt::Display) -> String {
    format!("media/factimg-{image_id}-thumb.jpg")
}

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("Invalid bind address: {0}")]
    InvalidBindAddr(String),

    #[error("Invalid CDN base URL: {0}")]
    InvalidCdnUrl(String),

    #[error(
        "CHRONOSCOPE_FACTS_DB must be set to the fact store's location: with the \
         SQLite backend, the path of a facts database built by `ingest build-db`; \
         with the Postgres backend, a connection URL"
    )]
    MissingFactsDb,

    #[error(
        "CHRONOSCOPE_FACTS_DB carries {form}. The fact store's location is \
         logged at startup and quoted in errors, so it must hold no \
         secret: put the password in PGPASSWORD (sqlx reads the standard libpq \
         environment: PGPASSWORD, PGHOST, PGUSER, PGDATABASE), or use IAM \
         database authentication, which replaces the password with a \
         short-lived token"
    )]
    FactsDbCredentials { form: CredentialForm },
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
    /// The fact store of submitted entity and image facts — the
    /// [`ServerFactStore`] backend over its own pool, a separate database from
    /// the app `db` above.
    pub facts: ServerFactStore,
    /// Where the loss of a credential this process needs is reported.
    ///
    /// A fact-store pool authenticated with short-lived tokens keeps answering
    /// from the connections it has open once renewal stops working, so both
    /// store checks in the readiness probe can pass while the instance is
    /// minutes from serving nothing. Taken from the store at startup, so the
    /// probe and the shutdown path read the same signal.
    pub credentials: CredentialWatch,
    /// Resolved media keys for every fact-store image, keyed by image id. The
    /// entity read path serves thumbnails and detail images from these keys; an
    /// image absent from the map is unresolved and contributes no thumbnail/tile.
    pub image_media: Arc<HashMap<ServerImageId, ResolvedImageMedia>>,
}

/// Everything [`AppState`] is assembled from: its fields, minus the WebAuthn
/// setup that [`AppState::new`] derives from `config`.
///
/// Named fields because the list is long enough that positional arguments stop
/// being readable, and because the one field a caller can get wrong,
/// `credentials`, reads next to the `facts` it was taken from. The
/// `embedded-media` field is `cfg`'d here instead of at every call site, so a
/// caller states its parts once whichever way the feature lands.
pub struct AppStateParts {
    pub db: Database,
    pub config: Config,
    pub jwt: JwtConfig,
    pub dns_resolver: Box<dyn DnsResolver>,
    #[cfg(feature = "embedded-media")]
    pub media_store: Arc<dyn MediaStore>,
    pub facts: ServerFactStore,
    pub credentials: CredentialWatch,
    pub image_media: Arc<HashMap<ServerImageId, ResolvedImageMedia>>,
}

impl AppState {
    /// Create application state with all components explicitly provided.
    ///
    /// # Errors
    /// Returns `AppStateError` if `WebAuthn` initialization fails.
    #[allow(clippy::unused_async)]
    pub async fn new(parts: AppStateParts) -> Result<Self, AppStateError> {
        let rp_origin = Url::parse(&parts.config.rp_origin)
            .map_err(|e| AppStateError::InvalidOrigin(format!("{e}")))?;

        let webauthn = WebauthnBuilder::new(&parts.config.rp_id, &rp_origin)
            .map_err(|e| AppStateError::WebAuthn(format!("{e}")))?
            .rp_name("Chronoscope")
            .build()
            .map_err(|e| AppStateError::WebAuthn(format!("{e}")))?;

        Ok(Self {
            db: parts.db,
            jwt: parts.jwt,
            webauthn,
            dns_resolver: parts.dns_resolver,
            config: parts.config,
            #[cfg(feature = "embedded-media")]
            media_store: parts.media_store,
            facts: parts.facts,
            credentials: parts.credentials,
            image_media: parts.image_media,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{ConfigError, CredentialForm, FactsDatabase};

    fn rejection_form(raw: &str) -> Result<CredentialForm, String> {
        match FactsDatabase::new(raw) {
            Err(ConfigError::FactsDbCredentials { form }) => Ok(form),
            Err(other) => Err(format!("expected a credential rejection, got {other}")),
            Ok(accepted) => Err(format!("expected a rejection, got {accepted}")),
        }
    }

    fn accepted(raw: &str) -> Result<FactsDatabase, String> {
        FactsDatabase::new(raw).map_err(|e| format!("expected acceptance, got {e}"))
    }

    /// The startup log prints this value, so a location that hands sqlx a
    /// password has to fail at boot instead of reaching the log.
    #[test]
    fn a_password_in_the_userinfo_is_rejected() -> Result<(), String> {
        let form = rejection_form("postgres://facts:hunter2@db.internal:5432/facts")?;
        assert_eq!(form, CredentialForm::Userinfo);
        Ok(())
    }

    /// sqlx honors `?password=` exactly as it honors the userinfo form, so this
    /// spelling is a credential too.
    #[test]
    fn a_password_query_parameter_is_rejected() -> Result<(), String> {
        let form = rejection_form("postgres://facts@db.internal:5432/facts?password=hunter2")?;
        assert_eq!(form, CredentialForm::QueryParameter);
        Ok(())
    }

    /// A facts-file path is not a URL at all, and the operator needs to see
    /// which artifact booted.
    #[test]
    fn a_facts_file_path_renders_verbatim() -> Result<(), String> {
        const PATH: &str = "/nix/store/abcdef-facts-db/facts.db";
        assert_eq!(accepted(PATH)?.to_string(), PATH);
        Ok(())
    }

    /// The shape the Postgres deployment is expected to configure: host, user,
    /// and TLS mode legible, authentication supplied out of band.
    #[test]
    fn a_credential_free_postgres_url_renders_verbatim() -> Result<(), String> {
        const URL: &str = "postgres://facts@db.internal:5432/facts?sslmode=require";
        assert_eq!(accepted(URL)?.to_string(), URL);
        Ok(())
    }
}
