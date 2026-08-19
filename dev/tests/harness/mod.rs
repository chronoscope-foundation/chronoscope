//! Test harness for browser-driven tests against the Chronoscope web frontend.
//!
//! Owns *all* JS construction. Tests in `dev/tests/web.rs` interact only with
//! typed `WebTest` methods; the two JS sites — `hook_expr` (mechanical) and
//! `WAIT_FOR_WASM_BOOT_JS` (the irreducible bootstrap) — live here.
//!
//! `Page::evaluate` is workspace-disallowed via `clippy.toml`. The three sites
//! that need it (`call_hook`, `await_hook`, `goto`) each carry a focused
//! `#[expect(clippy::disallowed_methods, …)]` with a per-site justification —
//! anywhere else, clippy fails.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use chromiumoxide::Page;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams;
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::cdp::js_protocol::runtime::EventConsoleApiCalled;
use chromiumoxide::page::ScreenshotParams;
use chronoscope_api::state::ServerIds;
use chronoscope_core::submit::Commit;
use chronoscope_dev::{
    DevServerConfig, FactsDbSource, ImageResolveMode, RunningDevServer, find_available_port,
    start_dev_server,
};
use chronoscope_workers::RetryConfig;
use dropshot::ConfigLogging;
use futures::{FutureExt, StreamExt};
use tokio::sync::Mutex;

pub type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

// ==================== Frontend Build ====================

/// Path to the prebuilt web frontend dist (with `test-hooks` feature).
///
/// Sourced from `$WEB_DIST` exported by the nix dev shell (see `flake.nix` ->
/// `WEB_DIST = web.packages.web-test`). No fallback build: if the env var
/// is missing, fail loudly so the cause is obvious.
fn web_dist() -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    let path = std::env::var("WEB_DIST")
        .map_err(|_| "WEB_DIST not set \u{2014} run inside nix develop")?;
    let dist = PathBuf::from(&path);
    if !dist.join("index.html").exists() {
        return Err(format!("WEB_DIST is set to {path} but does not contain index.html").into());
    }
    Ok(dist)
}

// ==================== Test Harness ====================

fn screenshot_dir() -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    static DIR: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or_else(|| "dev crate has no parent".to_string())?
            .join("target/web-test-screenshots");
        std::fs::create_dir_all(&dir).map_err(|e| format!("create screenshot dir: {e}"))?;
        Ok(dir)
    })
    .clone()
    .map_err(Into::into)
}

/// How many test browsers were already running when this suite started,
/// sampled once.
///
/// A `Timeout` from a wait hook says nothing about *why* the page stalled, and
/// the usual why is contention: another suite competing for the CPU starves the
/// headless event loop. That has cost real diagnosis time more than once (see
/// `chronoscope-learnings/web-test-timeout-flakes-*.md`), so the count rides
/// along in every timeout message.
///
/// Counted whoever owns them. A browser stranded by a killed run and a browser
/// belonging to another worktree's gate cost the same cores, and this machine
/// carries a dozen worktrees, so a live sibling suite is the likelier of the
/// two. An earlier version counted only processes reparented to `init`, which
/// saw the abandoned case and missed the concurrent one entirely.
///
/// Sampled by the first test to start, before any browser of this run exists,
/// which is what lets the count mean "already running".
///
/// The harness launches "Google Chrome for Testing", a separate binary from
/// Chrome or Chromium, so a browser someone is reading this in is never counted.
fn browsers_already_running() -> usize {
    static COUNT: OnceLock<usize> = OnceLock::new();
    *COUNT.get_or_init(|| {
        let Ok(out) = std::process::Command::new("ps")
            .args(["-axo", "command="])
            .output()
        else {
            return 0;
        };
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|line| line.contains("for Testing"))
            .count()
    })
}

/// Appended to every timeout so a stall names its most likely cause instead of
/// leaving the reader to guess.
fn contention_note() -> String {
    match browsers_already_running() {
        0 => String::new(),
        n => format!(
            " ({n} test browser process(es) were already running when this \
             suite started, from a concurrent run or a stranded one, which \
             starves the headless event loop; re-run on a quiet machine \
             before suspecting the code)"
        ),
    }
}

/// Launch a headless Chrome browser for a single test.
///
/// Following chromiumoxide's own test pattern: each test gets its own browser
/// instance. The handler is spawned on the test's tokio runtime. The caller
/// must call `browser.close().await` when done — this kills Chrome cleanly.
async fn launch_browser() -> Result<
    (Browser, tokio::task::JoinHandle<()>, tempfile::TempDir),
    Box<dyn std::error::Error + Send + Sync>,
> {
    // Each test gets a unique user-data-dir so multiple Chrome instances
    // can run concurrently without fighting over a SingletonLock file.
    let user_data =
        tempfile::tempdir().map_err(|e| format!("failed to create browser temp dir: {e}"))?;

    // chromiumoxide's built-in detection finds Chrome/Chromium via the CHROME
    // env var, PATH lookup (using the `which` crate), and platform-specific
    // paths (macOS /Applications, Linux /opt, Windows registry).
    let config = BrowserConfig::builder()
        .user_data_dir(user_data.path())
        .new_headless_mode()
        .no_sandbox()
        .window_size(1280, 800)
        .request_timeout(CDP_REQUEST_TIMEOUT)
        // Fail every hostname lookup so a map test reaches only the frontend and
        // API on 127.0.0.1 (literal IPs skip the resolver); a still-remote
        // basemap or engine asset fails loudly here instead of hanging the
        // idle wait on a slow OHM/unpkg fetch.
        .arg("--host-resolver-rules=MAP * ~NOTFOUND")
        .build()
        .map_err(|e| format!("failed to build browser config: {e}"))?;

    let (browser, mut handler) = Browser::launch(config)
        .await
        .map_err(|e| format!("failed to launch browser: {e}"))?;

    let handle = tokio::spawn(async move {
        while let Some(h) = handler.next().await {
            if h.is_err() {
                break;
            }
        }
    });

    Ok((browser, handle, user_data))
}

/// Return an error if a condition is false (like `assert!` but non-panicking).
pub fn check(condition: bool, msg: impl std::fmt::Display) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(msg.to_string().into())
    }
}

/// Reverse-proxy `/api/*` to the API server so the browser sees a single
/// origin — mirroring Trunk's `--proxy-rewrite` in dev and Cloudflare in prod.
/// Strips the `/api` mount prefix and forwards the method, path, query,
/// headers, and body to the API root, then relays the upstream status,
/// headers, and body back. `api_base` is the API server's `http://host:port`.
async fn proxy_api(State(api_base): State<String>, req: Request) -> Result<Response, StatusCode> {
    let path = req.uri().path();
    let rest = path.strip_prefix("/api").unwrap_or(path);
    let query = req
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let url = format!("{api_base}{rest}{query}");

    let (parts, body) = req.into_parts();
    let body_bytes = axum::body::to_bytes(body, usize::MAX)
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    let upstream = reqwest::Client::new()
        .request(parts.method, url.as_str())
        .headers(parts.headers)
        .body(body_bytes)
        .send()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    let status = upstream.status();
    let headers = upstream.headers().clone();
    let bytes = upstream
        .bytes()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    let mut builder = Response::builder().status(status);
    if let Some(dst) = builder.headers_mut() {
        *dst = headers;
    }
    builder
        .body(Body::from(bytes))
        .map_err(|_| StatusCode::BAD_GATEWAY)
}

/// A freshly encoded 1x1 transparent PNG for the basemap's raster tile and
/// sprite stubs, regenerated per request (microseconds) rather than baked as a
/// byte array. It must decode: `map.loaded()` gates on `style.loaded()`, which
/// waits on the sprite/image manager, so an image that fails to decode can
/// stall idle indefinitely.
fn one_px_png() -> Response {
    let mut buf = std::io::Cursor::new(Vec::new());
    let pixel = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 0]));
    match image::DynamicImage::ImageRgba8(pixel).write_to(&mut buf, image::ImageFormat::Png) {
        Ok(()) => ([(CONTENT_TYPE, "image/png")], buf.into_inner()).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Stub the basemap's tiles same-origin, keyed by the extension the rewritten
/// style doc requests. A raster `.png` gets a 1x1 PNG; a vector `.pbf` gets an
/// empty body: MapLibre counts an empty or errored tile as loaded for
/// `areTilesLoaded()`, so the tile only needs an instant same-origin 200, not
/// real bytes.
async fn basemap_tile(Path(rest): Path<String>) -> Response {
    if rest.ends_with(".png") {
        one_px_png()
    } else {
        ([(CONTENT_TYPE, "application/x-protobuf")], Vec::<u8>::new()).into_response()
    }
}

/// Stub the basemap's sprite same-origin. The `.png` sheet gets a 1x1 PNG;
/// everything else (the `.json` manifest, including the `@2x` variant) gets an
/// empty sprite object. The sprite is the one basemap asset that must be valid:
/// `map.loaded()` gates on the image manager, so a sprite that errors or
/// SPA-falls-back to HTML can stall idle.
async fn basemap_sprite(Path(rest): Path<String>) -> Response {
    if rest.ends_with(".png") {
        one_px_png()
    } else {
        ([(CONTENT_TYPE, "application/json")], "{}").into_response()
    }
}

/// Stub the basemap's glyph ranges same-origin with an empty range. Glyphs are
/// lazy and never gate `style.loaded()`, so an empty body suffices; they exist
/// only so a glyph fetch stays on the page origin rather than reaching OHM.
async fn basemap_glyph() -> Response {
    ([(CONTENT_TYPE, "application/x-protobuf")], Vec::<u8>::new()).into_response()
}

/// Build the single JS expression used to invoke a `window.__test.<hook>(...)`
/// call. The args are serialized as a JSON array so each value is escaped
/// correctly; we slice off the outer brackets to get a comma-separated
/// argument list. Every non-bootstrap interaction with the page flows
/// through this builder.
fn hook_expr(
    hook: &str,
    args: &[serde_json::Value],
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let arr = serde_json::to_string(args)?;
    // `arr` is `[arg1, arg2, ...]`; strip the brackets for splicing.
    let inner = &arr[1..arr.len() - 1];
    Ok(format!("window.__test.{hook}({inner})"))
}

/// Generate typed `WebTest` method wrappers around `window.__test.<name>(...)`
/// calls. The method name *is* the hook name on the wire (`stringify!`'d),
/// so no second string ever needs to stay in sync.
///
/// Three entry shapes, one per call kind:
///
/// - `query name(arg: T, ...) -> Ret;` — typed return via `call_hook<Ret>`.
///   The method returns `Result<Ret, BoxedError>`.
/// - `action name(arg: T, ...);` — JS-side returns `undefined`; method
///   returns `TestResult` and discards the value.
/// - `wait name(arg: T, ...);` — JS-side returns a Promise; method
///   `await_hook`s it (`TIMEOUT` is the safety net).
///
/// Each arg is JSON-serialized via `serde_json::json!`, so it must be
/// `Serialize`. Multi-step wrappers (e.g., the ones that chain `wait_for_*`
/// + a query) stay as bespoke `pub async fn`s below this macro.
macro_rules! forward_hooks {
    () => {};

    // Entries are `[vis] query|action|wait name(args)[ -> Ret];`. Use `pub`
    // for test-facing methods; omit the visibility to generate a private
    // method (an internal building block consumed only by bespoke
    // composers on `WebTest`).

    ($vis:vis query $name:ident($($arg:ident: $ty:ty),* $(,)?) -> $ret:ty; $($rest:tt)*) => {
        $vis async fn $name(&self $(, $arg: $ty)*)
            -> Result<$ret, Box<dyn std::error::Error + Send + Sync>>
        {
            self.call_hook(
                stringify!($name),
                &[$(serde_json::json!($arg)),*],
            ).await
        }
        forward_hooks!($($rest)*);
    };

    ($vis:vis action $name:ident($($arg:ident: $ty:ty),* $(,)?); $($rest:tt)*) => {
        $vis async fn $name(&self $(, $arg: $ty)*) -> TestResult {
            self.call_hook::<serde_json::Value>(
                stringify!($name),
                &[$(serde_json::json!($arg)),*],
            ).await?;
            Ok(())
        }
        forward_hooks!($($rest)*);
    };

    ($vis:vis wait $name:ident($($arg:ident: $ty:ty),* $(,)?); $($rest:tt)*) => {
        $vis async fn $name(&self $(, $arg: $ty)*) -> TestResult {
            self.await_hook(
                stringify!($name),
                &[$(serde_json::json!($arg)),*],
            ).await
        }
        forward_hooks!($($rest)*);
    };
}

/// The single irreducible JS string in this file. `goto` uses it to detect
/// WASM boot — which has to be observable *without* calling any
/// `window.__test` hook, since those don't exist until WASM's `register_base`
/// runs. Once this Promise resolves, every other interaction goes through
/// typed `WebTest` methods + `hook_expr`.
const WAIT_FOR_WASM_BOOT_JS: &str = r#"
    new Promise(resolve => {
        if (window.__test) { resolve(); return; }
        var obs = new MutationObserver(() => { if (window.__test) { obs.disconnect(); resolve(); } });
        obs.observe(document.documentElement, {childList: true, subtree: true});
    })
"#;

/// Per-test context: API server + frontend file server + browser.
///
/// Each test gets its own Chrome instance (following chromiumoxide's own test
/// pattern). Call `close()` when done to kill Chrome cleanly and await the
/// handler task.
pub struct WebTest {
    browser: Browser,
    handler_handle: tokio::task::JoinHandle<()>,
    // `page` and `server` are deliberately private so tests cannot call
    // `t.page.evaluate(...)` or hand-construct API URLs — every interaction
    // goes through the typed methods below. Exposing them would re-open
    // the JS-string surface that this harness exists to confine.
    page: Page,
    frontend_url: String,
    console_logs: Arc<Mutex<Vec<ConsoleEntry>>>,
    screenshot_dir: PathBuf,
    server: RunningDevServer,
    // RAII guards: each holds a TempDir that's cleaned up when the WebTest
    // drops. Underscore prefix tells rustc the read-only nature is intentional.
    _browser_data_dir: tempfile::TempDir,
    _db_dir: tempfile::TempDir,
}

#[derive(Debug, Clone)]
struct ConsoleEntry {
    level: String,
    text: String,
}

impl WebTest {
    /// Close the browser and await the handler task. Called by `web_test()`
    /// after each test.
    async fn close(mut self) -> TestResult {
        self.browser
            .close()
            .await
            .map_err(|e| format!("browser close: {e}"))?;
        self.handler_handle
            .await
            .map_err(|e| format!("handler join: {e}"))?;
        self.server.shutdown().await;
        Ok(())
    }

    /// Create a new test backed by a fresh throwaway database and the curated
    /// fact store, with `seed_commits` written into the writable overlay before
    /// serving. Called by the `web_test*` runners; tests go through those rather
    /// than constructing `WebTest` directly.
    async fn new(
        seed_commits: Vec<Commit<ServerIds>>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let dist_dir = web_dist()?;
        let (browser, handler_handle, browser_data_dir) = launch_browser().await?;

        // Start API server
        let port = find_available_port()?;
        let base_url = format!("http://127.0.0.1:{port}");

        // The front-door origin the browser loads the page (and its thumbnails)
        // from. Reserved up front so the API's `cdn_base_url` can point at this
        // origin's `/api` proxy, keeping thumbnail `<img>` loads same-origin.
        let frontend_port = find_available_port()?;
        let frontend_url = format!("http://127.0.0.1:{frontend_port}");

        let log = ConfigLogging::StderrTerminal {
            level: dropshot::ConfigLoggingLevel::Warn,
        }
        .to_logger("web-test")?;

        let http_client: Arc<dyn chronoscope_workers::HttpClient> =
            Arc::new(chronoscope_workers::ReqwestClient::new()?);

        // Serve the curated facts DB (the artifact `CHRONOSCOPE_FACTS_DB` names,
        // provided by the nix test env) as the frozen read-only `base` — attached
        // `mode=ro&immutable=1`, so every test shares the one immutable pin with
        // no per-test clone — beneath a fresh per-test writable overlay. The app
        // tables and the overlay scratch get sibling files in a TempDir, whose
        // -wal/-shm siblings are cleaned up together.
        let db_dir = tempfile::tempdir()?;
        let facts_pin = chronoscope_dev::mount_facts_db("curated")?;
        let database_url = format!("sqlite:{}", db_dir.path().join("app.db").display());
        let facts_overlay = db_dir.path().join("facts-overlay.db").display().to_string();

        let server = start_dev_server(DevServerConfig {
            database_url: Some(database_url),
            facts: FactsDbSource::Mounted {
                base: facts_pin,
                overlay: facts_overlay,
            },
            seed_commits,
            http_client,
            worker_idle_backoff: Duration::from_secs(60), // Workers not needed for frontend tests
            retry_config: RetryConfig::default(),
            log,
            port,
            // Thumbnails resolve to the front door's `/api` proxy, so the browser
            // fetches them same-origin (matching the page origin) and the proxy
            // forwards to `/media`.
            cdn_base_url: format!("{frontend_url}/api"),
            // Placeholder images keep the browser thumbnail tests deterministic.
            image_resolve: ImageResolveMode::Placeholder,
            rp_id: None,
            rp_origin: None,
            ios_app_id: None,
            apify_config: None,
            dns_resolver: chronoscope_api::state::permissive_dns_resolver(),
        })
        .await?;

        // Startup dispatches the media warm without blocking; wait for the
        // background consumer to drain it so every test's thumbnails are present
        // before it asserts.
        server.await_media_warmed().await;

        // Serve the prebuilt dist directly (nothing per-test is injected into
        // it any more) with an SPA fallback: any path that doesn't match a file
        // serves index.html so client-side routing works.
        let index_html = dist_dir.join("index.html");
        let serve_dir = tower_http::services::ServeDir::new(&dist_dir)
            .append_index_html_on_directories(true)
            .fallback(tower_http::services::ServeFile::new(index_html));
        // Reverse-proxy `/api/*` to the API so the browser talks to one origin
        // (page derives its base from `window.location.origin` + `/api`). The
        // route is registered ahead of `fallback_service`, so `/api/*` never
        // falls through to the SPA index.html (which would answer JSON requests
        // with an HTML 200 and break parsing).
        // Basemap stub routes, registered ahead of `fallback_service` so the
        // SPA index.html never answers them with HTML. The webTest bundle
        // rewrites the pinned style doc's tiles, glyphs and sprite to these
        // `/basemap/...` paths, so map-idle is reached from same-origin stubs
        // rather than fetching OHM. The style doc itself still falls through to
        // ServeDir at `/basemap/ohm-historical.json`.
        let app = axum::Router::new()
            .route("/api/{*rest}", axum::routing::any(proxy_api))
            .route("/basemap/tiles/{*rest}", axum::routing::get(basemap_tile))
            .route(
                "/basemap/sprite/{*rest}",
                axum::routing::get(basemap_sprite),
            )
            .route("/basemap/fonts/{*rest}", axum::routing::get(basemap_glyph))
            .fallback_service(serve_dir)
            .with_state(base_url.clone());
        let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{frontend_port}")).await?;
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        // Wait for the static file server to be ready by probing it.
        // Generous deadline because the whole test suite spins up dozens of
        // these in parallel and axum's `serve` future can starve under load.
        // Sleep between probes (rather than busy-yielding) so we don't burn
        // CPU racing other tests that are already saturating the runtime.
        // The TCP listener is already bound, but until axum's `serve` future
        // is scheduled there's no signal we can wait on — only polling.
        let probe_url = format!("{frontend_url}/");
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if reqwest::get(&probe_url).await.is_ok() {
                    return;
                }
                #[allow(
                    clippy::disallowed_methods,
                    reason = "polling external service readiness; no sync signal available"
                )]
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .map_err(|_| "static file server did not become ready within 30s")?;

        let page = browser.new_page("about:blank").await?;

        // Set up console log capture
        let console_logs: Arc<Mutex<Vec<ConsoleEntry>>> = Arc::new(Mutex::new(Vec::new()));
        let logs_clone = console_logs.clone();
        let mut console_events = page.event_listener::<EventConsoleApiCalled>().await?;
        tokio::spawn(async move {
            while let Some(event) = console_events.next().await {
                let text = event
                    .args
                    .iter()
                    .map(|arg| {
                        arg.value
                            .as_ref()
                            .map(|v| v.to_string())
                            .or_else(|| arg.description.clone())
                            .unwrap_or_default()
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                let level = format!("{:?}", event.r#type);
                logs_clone.lock().await.push(ConsoleEntry { level, text });
            }
        });

        Ok(Self {
            browser,
            handler_handle,
            page,
            frontend_url,
            console_logs,
            screenshot_dir: screenshot_dir()?,
            server,
            _browser_data_dir: browser_data_dir,
            _db_dir: db_dir,
        })
    }

    // ---- Browser / server state ----

    /// Current page URL.
    pub async fn url(&self) -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
        self.page.url().await.map_err(Into::into)
    }

    /// Base URL of the API server backing this test (e.g.,
    /// `http://127.0.0.1:54321`). Used by tests that need to round-trip
    /// the real URL back into the page (e.g., the error→retry test that
    /// breaks then restores the API endpoint).
    pub fn api_base_url(&self) -> String {
        self.server.base_url.clone()
    }

    /// The same-origin API base the page's client actually uses: the front
    /// door's `/api` proxy, not the API server's direct address. Restoring a
    /// broken client to this (rather than [`Self::api_base_url`]) keeps the
    /// fetch on the page origin — a cross-origin restore fails now that the API
    /// serves no CORS.
    pub fn same_origin_api_url(&self) -> String {
        format!("{}/api", self.frontend_url)
    }

    /// Read a JSON document the front door serves, by the page-relative path the
    /// page itself would ask for it by.
    ///
    /// Lets a test hold the live page against a document it was built from,
    /// which is a stronger statement than holding it against the page's own
    /// bookkeeping. The SPA fallback serves `index.html` for anything that isn't
    /// a file, so a path that names nothing arrives as a JSON parse failure
    /// rather than a 404.
    pub async fn frontend_json(
        &self,
        path: &str,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
        let url = format!("{}{path}", self.frontend_url);
        let body = reqwest::get(&url)
            .await
            .map_err(|e| format!("fetching {url}: {e}"))?
            .text()
            .await
            .map_err(|e| format!("reading {url}: {e}"))?;
        serde_json::from_str(&body).map_err(|e| format!("{url} is not JSON: {e}").into())
    }

    // ---- Hook invocation primitives ----
    //
    // Every interaction with the WASM-side test surface flows through these
    // two helpers. They build the only JS expression in this file —
    // `window.__test.<hook>(<json-args>)` — mechanically from the hook name
    // and serialized args, so call sites are all typed Rust.

    /// Invoke a `window.__test.<hook>(...)` and deserialize its return value
    /// into `R`. The `?? null` coalesce normalizes `undefined` to JSON
    /// `null`; CDP may still omit the value field, so we read it directly
    /// (defaulting to Null) before deserializing — `EvaluationResult::into_value`
    /// rejects missing value fields outright.
    ///
    /// For hooks that return nothing JS-side, use `R = serde_json::Value`
    /// and discard. (Unit `()` doesn't deserialize from Null.)
    /// Use `await_hook` instead when the return is a Promise.
    #[expect(
        clippy::disallowed_methods,
        reason = "harness entry point for typed window.__test calls; the JS expression is built by hook_expr from typed Rust args"
    )]
    async fn call_hook<R: serde::de::DeserializeOwned>(
        &self,
        hook: &str,
        args: &[serde_json::Value],
    ) -> Result<R, Box<dyn std::error::Error + Send + Sync>> {
        let expr = format!("({}) ?? null", hook_expr(hook, args)?);
        let result = self.page.evaluate(expr.as_str()).await?;
        let value = result.value().cloned().unwrap_or(serde_json::Value::Null);
        serde_json::from_value(value).map_err(Into::into)
    }

    /// Invoke a `window.__test.<hook>(...)` returning a Promise and await it
    /// with `TIMEOUT` as the safety net. Used for the `waitFor*` family.
    #[expect(
        clippy::disallowed_methods,
        reason = "harness entry point for typed window.__test promise hooks; the JS expression is built by hook_expr from typed Rust args"
    )]
    async fn await_hook(&self, hook: &str, args: &[serde_json::Value]) -> TestResult {
        let expr = hook_expr(hook, args)?;
        tokio::time::timeout(TIMEOUT, async { self.page.evaluate(expr.as_str()).await })
            .await
            .map_err(|_| {
                format!(
                    "Timeout after {TIMEOUT:?} waiting for hook: {hook}{}",
                    contention_note()
                )
            })?
            .map_err(|e| format!("JS error in hook {hook}: {e}"))?;
        Ok(())
    }

    // ---- Navigation & DOM-mutation waits ----

    /// Navigate to a path and wait for WASM to boot. Uses the irreducible
    /// `WAIT_FOR_WASM_BOOT_JS` (see its docs) — typed hooks are unreachable
    /// here because they live on `window.__test`, which doesn't exist until
    /// boot completes.
    #[expect(
        clippy::disallowed_methods,
        reason = "bootstrap: window.__test doesn't exist yet, so we can't use any typed hook to wait for it"
    )]
    pub async fn goto(&self, path: &str) -> TestResult {
        let url = format!("{}{}", self.frontend_url, path);
        self.page.goto(&url).await?;
        tokio::time::timeout(TIMEOUT, async {
            self.page.evaluate(WAIT_FOR_WASM_BOOT_JS).await
        })
        .await
        .map_err(|_| {
            format!(
                "Timeout after {TIMEOUT:?} waiting for WASM boot{}",
                contention_note()
            )
        })?
        .map_err(|e| format!("WASM boot wait failed: {e}"))?;
        Ok(())
    }

    // ---- Mechanically-shaped hook wrappers (generated by macro) ----
    //
    // Each entry maps 1:1 to a `window.__test.<name>(...)` call. The Rust
    // method name *is* the hook name on the wire (stringify!'d), so adding
    // a new hook = add one line here + one line in the WASM registration.
    //
    // Only the *composed* variants of click/text/attr are exposed —
    // each waits for the selector first. The uncomposed primitives
    // (click_visible, text_of) exist as Rust helpers on the WASM side but
    // aren't registered, so tests can't reach them.
    forward_hooks! {
        // DOM actions (compositions: wait then act).
        pub wait click(selector: &str);
        pub action dispatch_error(msg: &str);
        pub action focus_element(selector: &str);

        // DOM-mutation waits.
        pub wait wait_for_selector(selector: &str);
        pub wait wait_for_selector_removal(selector: &str);
        pub wait wait_for_body_text(needle: &str);
        // Webfonts loaded and the reflow around them finished. Required before
        // any geometry assertion, which otherwise straddles the font swap.
        pub wait wait_for_fonts();
        // A media query reads `expected`. `set_viewport` returns before the
        // renderer has resized and recalculated style, so a responsive
        // assertion has to wait for the predicate the stylesheet branches on.
        pub wait wait_for_media_query(query: &str, expected: bool);
        // Every animation on the element and its subtree that can finish has.
        // Required before asserting where something a click *elsewhere* moved
        // has come to rest: `click` only waits on the element it clicked.
        pub wait wait_for_animations(selector: &str);

        // DOM queries (compositions: wait then read, where natural).
        pub query text(selector: &str) -> String;
        pub query attr(selector: &str, attribute: &str) -> Option<String>;
        pub query is_visible(selector: &str) -> bool;
        // `[x, y, width, height]`, unrounded. Empty when nothing matches.
        pub query element_rect(selector: &str) -> Vec<f64>;
        // Whether a click at the element's own centre would reach it, rather
        // than something painted over it.
        pub query is_hittable(selector: &str) -> bool;
        // Hold the element's entry animation at `progress` (0.0..=1.0) and read
        // `[animation_count, opacity, x, y, width, height]` there.
        pub query sample_animation_at(selector: &str, progress: f64) -> Vec<f64>;
        // Stretch every animation, so entry animations stay live long enough to
        // be seeked rather than finishing before the sample arrives.
        pub action slow_animations(ms: f64);
        pub query is_active_inside(selector: &str) -> bool;
        pub query active_element_attribute(attribute: &str) -> Option<String>;
        // The locale this browser reads in, exactly as `navigator.language`
        // names it. A test that wants the primary subtag reduces it itself,
        // since the reduction is the thing under test.
        pub query navigator_language() -> Option<String>;
        pub query press_key(selector: &str, key: &str) -> bool;
        // Plural: returns every match's attribute. Use only when you
        // genuinely want all matches (e.g., accessibility assertions);
        // for the singular case, use `attr`.
        pub query attributes(selector: &str, attribute: &str) -> Vec<Option<String>>;

        // Internal: consumed by the bespoke `has_text` wrapper.
        pub query body_text() -> String;
    }

    // ---- Bespoke wrappers (multi-step or type-adapter) ----

    /// Check whether `needle` is present in `document.body.innerText` right now.
    pub async fn has_text(
        &self,
        needle: &str,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.body_text().await?.contains(needle))
    }

    /// Check whether at least one element matches a selector right now.
    pub async fn exists(
        &self,
        selector: &str,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.count(selector).await? > 0)
    }

    /// Number of elements matching the selector.
    pub async fn count(
        &self,
        selector: &str,
    ) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        let n: f64 = self
            .call_hook("count", &[serde_json::json!(selector)])
            .await?;
        Ok(n as usize)
    }

    /// Set the viewport size for responsive testing.
    pub async fn set_viewport(
        &self,
        width: u32,
        height: u32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.page
            .execute(
                SetDeviceMetricsOverrideParams::builder()
                    .width(width)
                    .height(height)
                    .device_scale_factor(1.0)
                    .mobile(width < 768)
                    .build()
                    .map_err(|e| format!("viewport build: {e}"))?,
            )
            .await?;

        Ok(())
    }

    /// Capture a screenshot, saving to the screenshots dir.
    pub async fn screenshot(
        &self,
        name: &str,
    ) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
        let bytes = self
            .page
            .screenshot(
                ScreenshotParams::builder()
                    .format(CaptureScreenshotFormat::Png)
                    .full_page(true)
                    .build(),
            )
            .await?;
        let path = self.screenshot_dir.join(format!("{name}.png"));
        std::fs::write(&path, &bytes)?;
        eprintln!("Screenshot saved: {}", path.display());
        Ok(path)
    }

    /// Dump captured console logs to stderr (for debugging failed tests).
    /// Called by `web_test()` on failure.
    async fn dump_console_logs(&self) {
        let logs = self.console_logs.lock().await;
        if logs.is_empty() {
            eprintln!("  (no console logs)");
        } else {
            for entry in logs.iter() {
                eprintln!("  [{}] {}", entry.level, entry.text);
            }
        }
    }

    // ---- Map hook wrappers ----

    // Macro-generated map hooks: shape matches the WASM-side registration
    // 1:1. `wait_for_map_idle` bakes in the existence wait WASM-side;
    // `marker_properties` bakes the settled-wait; `click_map_at` bakes
    // settled-wait + fire_map_click.
    forward_hooks! {
        // Map queries (public).
        pub query map_cursor() -> String;
        pub query marker_properties() -> Vec<serde_json::Value>;
        // Cluster-badge descriptors (proximity clusters + lone server
        // `Expand` cells) — the complement of `marker_properties`.
        pub query badge_properties() -> Vec<serde_json::Value>;
        // Verdict-ring descriptors. Its own hook because a ring carries its
        // marker's feature id, which `marker_properties` dedupes against.
        pub query ring_properties() -> Vec<serde_json::Value>;
        // Whether every verdict-ring sprite the layers name is registered.
        pub query ring_sprites_registered() -> bool;
        // `[inner, outer]` CSS pixels from an undated marker's coordinate: the
        // band its verdict ring has to itself, read off the radii the renderer
        // draws it from.
        pub query unevidenced_ring_band() -> Vec<f64>;
        // How far past its coordinate a thumbnail's raster reaches, in CSS
        // pixels: the drop that lands the location dot on the point.
        pub query thumbnail_dot_drop() -> f64;
        // The coordinate `dx`/`dy` CSS pixels from the given one, so a test can
        // aim past a marker's disc and onto its ring.
        pub query offset_lnglat(lng: f64, lat: f64, dx: f64, dy: f64) -> Vec<f64>;
        // Which marker layers own the pixel at a coordinate — the set a click
        // there hit-tests against.
        pub query marker_layers_at(lng: f64, lat: f64) -> Vec<String>;
        pub query layer_order() -> Vec<String>;
        // One descriptor per style layer: `id`, `source_layer` (the basemap's
        // own layers carry one), `filter`, the named layout property's value as
        // `layout`, the whole `paint` object, `basemap_label` for the layer
        // entity labels take their font and text paint from, and
        // `label_rewrite_target` for the layers the reader's-language rewrite
        // actually wrote at style load.
        pub query style_layers(layout_property: &str) -> Vec<serde_json::Value>;
        // The page-relative path the map fetched its basemap style from.
        pub query basemap_style_url() -> String;
        // The text paint properties entity labels take off the basemap's label
        // layer, as named by the code that copies them.
        pub query copied_text_paint() -> Vec<String>;
        // Basemap layers whose own filter the style snapshot could not keep, so
        // they are filtered by the instant alone.
        pub query basemap_filters_dropped() -> Vec<String>;
        // Current map zoom — for asserting a cluster click zooms the map in.
        pub query zoom() -> f64;
        // Map canvas size in CSS pixels `[width, height]` — the frame
        // `marker_properties`/`badge_properties` project `_x`/`_y` into, so a
        // test can bound-check a rendered marker against the visible viewport.
        pub query map_canvas_size() -> Vec<f64>;
        // Fetch-settled counter sample, so a test can click a badge and then
        // wait for the expansion's re-fetch via `wait_for_fetch_settled_after`.
        pub query current_fetch_settled() -> f64;
        // Why the settled counter is where it is, for the bespoke
        // `wait_for_fetch_settled_after` below to attach to a stall.
        pub query fetch_diagnostics() -> String;

        // Actions (public). The raw `jump_to`/`fire_map_click` primitives
        // aren't exposed — tests use `pan_map_to` / `click_map_at`, which
        // bake in the appropriate waits.
        pub action fire_canvas_mousemove(lng: f64, lat: f64);
        // Put an `error` event through MapLibre's own dispatch. `source_id` is
        // the source it names; `None` is the style-level shape, which MapLibre
        // uses when no source owns the failure.
        pub action fire_map_error(message: &str, source_id: Option<&str>);
        pub action set_api_url(url: &str);

        // Wait hooks (public). Each bakes its own sequencing WASM-side so
        // it's one CDP roundtrip per call.
        pub wait wait_for_map_idle();
        pub wait click_map_at(lng: f64, lat: f64);
        pub wait pan_map_to(lng: f64, lat: f64, zoom: f64);
        pub wait click_and_wait_for_fetch(selector: &str);
        // Drive the time slider to a year and await the refetch it triggers.
        pub wait set_time_slider_year(year: f64);

        // Internal: the thumbnails-loaded counter is consumed only by the
        // bespoke `goto_map_with_thumbnails` composer below.
        pub query current_thumbnails_loaded() -> f64;
        pub wait wait_for_thumbnails_loaded_after(prev: f64);
    }

    /// Wait for the entity-fetch counter to advance past `prev`, naming what
    /// stalled if it never does.
    ///
    /// Every map test drains the mount fetch through this before it does
    /// anything else, and the bump it waits for cannot arrive at all when the
    /// pass that would produce it never runs. Bare, that reads exactly like a
    /// slow machine, which is how this suite has repeatedly spent an
    /// investigation on a 60-second stall; the diagnostics say which it was.
    pub async fn wait_for_fetch_settled_after(&self, prev: f64) -> TestResult {
        let Err(stall) = self
            .await_hook("wait_for_fetch_settled_after", &[serde_json::json!(prev)])
            .await
        else {
            return Ok(());
        };
        // Bounded separately and tightly. A wedged renderer is one of the
        // things this explains, and in that state the read is answered by
        // nothing until `CDP_REQUEST_TIMEOUT` gives up 90 s later — turning the
        // worst failure from 60 s into 150 s to say the same thing. The read is
        // a few cell loads, so anything but a prompt answer is that case.
        let why = match tokio::time::timeout(DIAGNOSTICS_TIMEOUT, self.fetch_diagnostics()).await {
            Ok(Ok(diagnostics)) => diagnostics,
            Ok(Err(err)) => format!("diagnostics unavailable: {err}"),
            Err(_) => format!("diagnostics unanswered within {DIAGNOSTICS_TIMEOUT:?}"),
        };
        Err(format!("{stall} [{why}]").into())
    }

    /// Number of rendered entity markers (across circle + thumbnail layers).
    pub async fn marker_count(&self) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        let n: f64 = self.call_hook("marker_count", &[]).await?;
        Ok(n as usize)
    }

    /// Number of rendered thumbnail markers (entities with photo pins).
    pub async fn thumbnail_marker_count(
        &self,
    ) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        let n: f64 = self.call_hook("thumbnail_marker_count", &[]).await?;
        Ok(n as usize)
    }

    /// Navigate to the map page, drain the mount fetch, then pan.
    pub async fn goto_map_at(&self, lng: f64, lat: f64, zoom: f64) -> TestResult {
        self.goto("/").await?;
        self.wait_for_map_idle().await?;
        // Drain the mount fetch so the upcoming sample isn't racing it.
        self.wait_for_fetch_settled_after(0.0).await?;
        self.pan_map_to(lng, lat, zoom).await
    }

    /// Navigate to map, pan, and wait for both entity-fetch settle and a
    /// thumbnails-loaded event. Sample-then-await pattern (mirror of
    /// `fetch_around`): sample the thumbnails counter before the pan, then
    /// wait for it to advance — race-free, no listener-attach timing concern.
    ///
    /// The thumbnails-loaded event fires only when a thumbnail *renders*, so use
    /// this at a zoom where at least one thumbnailed marker is unfolded. Where
    /// they all fold into a badge, none renders — sample the badge via
    /// `goto_map_at` + `badge_properties` rather than waiting here.
    pub async fn goto_map_with_thumbnails(&self, lng: f64, lat: f64, zoom: f64) -> TestResult {
        self.goto("/").await?;
        self.wait_for_map_idle().await?;
        self.wait_for_fetch_settled_after(0.0).await?;
        let prev_thumbs = self.current_thumbnails_loaded().await?;
        self.pan_map_to(lng, lat, zoom).await?;
        self.wait_for_thumbnails_loaded_after(prev_thumbs).await
    }

    /// Poll until at least one entity marker has rendered, bounded by [`TIMEOUT`].
    ///
    /// A retry re-fetch re-renders through a data-only `setData` with no camera
    /// move, so `wait_for_map_idle` resolves eagerly (the map isn't moving)
    /// before the reloaded features paint — a bare `marker_count` read then races
    /// the render and sees zero. `marker_properties` bakes the settle wait (idle +
    /// two RAF), so each poll is render-safe and frame-paced; the loop returns the
    /// moment the reloaded markers become queryable.
    pub async fn wait_for_markers(&self) -> TestResult {
        let poll = async {
            loop {
                if !self.marker_properties().await?.is_empty() {
                    return Ok::<(), Box<dyn std::error::Error + Send + Sync>>(());
                }
            }
        };
        tokio::time::timeout(TIMEOUT, poll).await.map_err(|_| {
            format!(
                "no entity markers rendered within {TIMEOUT:?}{}",
                contention_note()
            )
        })?
    }
}

/// Run a browser test with automatic setup, teardown, and diagnostics.
///
/// Follows chromiumoxide's own test pattern: launches a fresh Chrome, runs the
/// test closure, captures diagnostics on failure, then closes Chrome and awaits
/// the handler.
pub async fn web_test(test: impl AsyncFnOnce(&WebTest) -> TestResult) -> TestResult {
    run_web_test(Vec::new(), test).await
}

/// Like [`web_test`], but writes `seed_commits` into the fact store before the
/// server serves a request. Tests seed entities at chosen (open-ocean)
/// coordinates so the map geometry under test — proximity clustering,
/// badge-vs-pin classification, expansion — is fully controlled and isolated
/// from the curated data.
pub async fn web_test_seeded(
    seed_commits: Vec<Commit<ServerIds>>,
    test: impl AsyncFnOnce(&WebTest) -> TestResult,
) -> TestResult {
    run_web_test(seed_commits, test).await
}

async fn run_web_test(
    seed_commits: Vec<Commit<ServerIds>>,
    test: impl AsyncFnOnce(&WebTest) -> TestResult,
) -> TestResult {
    // Sample before this test's browser exists, so the count means what its
    // message says. The first test to arrive takes it for the whole suite.
    let _ = browsers_already_running();

    let t = WebTest::new(seed_commits).await?;

    // A panicking test must still reach the graceful `close()` below, so the
    // panic is caught and turned into an ordinary failure.
    //
    // Without this the unwind skips `close()`, and chromiumoxide's fallback is
    // tokio's `kill_on_drop` — a SIGKILL to Chrome's top-level process. Chrome
    // reaps its own renderers when asked to shut down and cannot when it is
    // killed outright, so every panicking test used to strand a handful of
    // renderer processes. They accumulate across runs, and a later gate
    // competing with them starves the headless event loop until a wait times
    // out, which reads as a flaky browser test rather than as a leak.
    let result = match std::panic::AssertUnwindSafe(test(&t)).catch_unwind().await {
        Ok(result) => result,
        Err(panic) => {
            // `Box<dyn Any>`: the message is behind one of two concrete types.
            let message = panic
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "panic with a non-string payload".to_string());
            Err(format!("test panicked: {message}").into())
        }
    };

    // Capture diagnostics before closing if the test failed
    if result.is_err() {
        let test_name = std::thread::current()
            .name()
            .unwrap_or("unknown")
            .to_string();
        eprintln!("--- Test {test_name} failed — capturing diagnostics ---");
        t.dump_console_logs().await;
        match t.screenshot(&format!("{test_name}_FAILED")).await {
            Ok(path) => eprintln!("  Failure screenshot: {}", path.display()),
            Err(e) => eprintln!("  Failed to capture screenshot: {e}"),
        }
        eprintln!("--- End diagnostics ---");
    }

    let close_result = t.close().await;
    // Prefer the test error over the close error
    result.and(close_result)
}

/// Deadline for test-hook async helpers — the harness's governing timeout.
///
/// Sized to absorb a slow headless-Chrome operation while still surfacing a
/// genuine hang. Wait helpers short-circuit the instant their condition is met,
/// so this only bounds the failure path — the happy path stays fast.
pub const TIMEOUT: Duration = Duration::from_secs(60);

/// chromiumoxide's per-command eviction budget. Held above [`TIMEOUT`] so the
/// harness wrapper is what fires first: a timeout then names the hook and its
/// context instead of surfacing chromiumoxide's bare "Request timed out."
const CDP_REQUEST_TIMEOUT: Duration = Duration::from_secs(90);

/// Deadline for reading diagnostics off an already-failed wait.
///
/// Well under [`TIMEOUT`], because this runs *after* a stall has already cost
/// its full budget and only reads a handful of cells. Left to [`TIMEOUT`] or to
/// [`CDP_REQUEST_TIMEOUT`], a renderer wedged badly enough to answer nothing
/// would more than double the cost of the worst failure to report the same
/// stall.
const DIAGNOSTICS_TIMEOUT: Duration = Duration::from_secs(5);
