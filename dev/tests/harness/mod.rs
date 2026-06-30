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

use chromiumoxide::Page;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams;
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::cdp::js_protocol::runtime::EventConsoleApiCalled;
use chromiumoxide::page::ScreenshotParams;
use chronoscope_dev::{DevServerConfig, RunningDevServer, find_available_port, start_dev_server};
use chronoscope_workers::RetryConfig;
use dropshot::ConfigLogging;
use futures::StreamExt;
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
    _tmp_dir: tempfile::TempDir,
    _browser_data_dir: tempfile::TempDir,
    _db_tmp: tempfile::TempDir,
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
        Ok(())
    }

    /// Create a new test backed by the Wikidata test database. Called by
    /// `web_test()`; tests go through that runner rather than constructing
    /// `WebTest` directly.
    async fn new() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let dist_dir = web_dist()?;
        let (browser, handler_handle, browser_data_dir) = launch_browser().await?;

        // Copy the Wikidata test DB to a writable temp dir (Nix store is read-only,
        // SQLite needs write access for WAL).
        let db_tmp = tempfile::tempdir()?;
        let wikidata_db = std::env::var("WIKIDATA_TEST_DB")
            .map_err(|_| "WIKIDATA_TEST_DB not set \u{2014} run inside nix develop")?;
        let src_db = PathBuf::from(&wikidata_db).join("wikidata.db");
        let dst_db = db_tmp.path().join("wikidata.db");
        std::fs::copy(&src_db, &dst_db)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dst_db, std::fs::Permissions::from_mode(0o644))?;
        }
        let database_url = Some(format!("sqlite:{}", dst_db.display()));

        // Start API server
        let port = find_available_port()?;
        let base_url = format!("http://127.0.0.1:{port}");

        let log = ConfigLogging::StderrTerminal {
            level: dropshot::ConfigLoggingLevel::Warn,
        }
        .to_logger("web-test")?;

        let http_client: Arc<dyn chronoscope_workers::HttpClient> =
            Arc::new(chronoscope_workers::ReqwestClient::new()?);

        let server = start_dev_server(DevServerConfig {
            database_url,
            http_client,
            worker_idle_backoff: Duration::from_secs(60), // Workers not needed for frontend tests
            retry_config: RetryConfig::default(),
            log,
            port,
            cdn_base_url: base_url.clone(),
            rp_id: None,
            rp_origin: None,
            ios_app_id: None,
            apify_config: None,
            triton: None,
            dns_resolver: chronoscope_api::state::permissive_dns_resolver(),
        })
        .await?;

        // Seed placeholder images for all annotated URLs so image tests
        // have resolved media to work with.
        let seeded = server.seed_test_media().await?;
        eprintln!("Seeded {seeded} test media items");

        // Create temp dir with config.json + symlinks to dist/
        let tmp_dir = tempfile::tempdir()?;
        let config_json = format!(r#"{{"api_url":"{base_url}"}}"#);
        std::fs::write(tmp_dir.path().join("config.json"), config_json)?;

        // Symlink all dist files except config.json
        for entry in std::fs::read_dir(dist_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            if name != "config.json" {
                #[cfg(unix)]
                std::os::unix::fs::symlink(entry.path(), tmp_dir.path().join(&name))?;
            }
        }

        // Start static file server with SPA fallback (serve index.html for
        // any path that doesn't match a file, so client-side routing works).
        let frontend_port = find_available_port()?;
        let serve_dir = tmp_dir.path().to_path_buf();
        let index_html = serve_dir.join("index.html");
        let app = axum::Router::new().fallback_service(
            tower_http::services::ServeDir::new(serve_dir)
                .append_index_html_on_directories(true)
                .fallback(tower_http::services::ServeFile::new(index_html)),
        );
        let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{frontend_port}")).await?;
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        let frontend_url = format!("http://127.0.0.1:{frontend_port}");

        // Wait for the static file server to be ready by probing it.
        // Generous deadline because the whole test suite spins up dozens of
        // these in parallel and axum's `serve` future can starve under load.
        // Sleep between probes (rather than busy-yielding) so we don't burn
        // CPU racing other tests that are already saturating the runtime.
        // The TCP listener is already bound, but until axum's `serve` future
        // is scheduled there's no signal we can wait on — only polling.
        let config_url = format!("{frontend_url}/config.json");
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if reqwest::get(&config_url).await.is_ok() {
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
            _tmp_dir: tmp_dir,
            _browser_data_dir: browser_data_dir,
            _db_tmp: db_tmp,
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
            .map_err(|_| format!("Timeout after {TIMEOUT:?} waiting for hook: {hook}"))?
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
        .map_err(|_| format!("Timeout after {TIMEOUT:?} waiting for WASM boot"))?
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

        // DOM queries (compositions: wait then read, where natural).
        pub query text(selector: &str) -> String;
        pub query attr(selector: &str, attribute: &str) -> Option<String>;
        pub query is_visible(selector: &str) -> bool;
        pub query is_active_inside(selector: &str) -> bool;
        pub query active_element_attribute(attribute: &str) -> Option<String>;
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
        pub query layer_order() -> Vec<String>;
        pub query zoom() -> f64;

        // Actions (public). The raw `jump_to`/`fire_map_click` primitives
        // aren't exposed — tests use `pan_map_to` / `click_map_at`, which
        // bake in the appropriate waits.
        pub action fire_canvas_mousemove(lng: f64, lat: f64);
        pub action set_api_url(url: &str);

        // Wait hooks (public). Each bakes its own sequencing WASM-side so
        // it's one CDP roundtrip per call.
        pub wait wait_for_map_idle();
        pub wait click_map_at(lng: f64, lat: f64);
        pub wait pan_map_to(lng: f64, lat: f64, zoom: f64);
        pub wait click_and_wait_for_fetch(selector: &str);

        // Internal: consumed only by bespoke composers below.
        // `get_center` returns a Vec<f64>; the bespoke `center()` unpacks
        // it into a `(f64, f64)` tuple. The counter family is invoked via
        // `fetch_around` / `goto_map_with_thumbnails`.
        pub query get_center() -> Vec<f64>;
        pub query current_fetch_settled() -> f64;
        pub query current_thumbnails_loaded() -> f64;
        pub wait wait_for_fetch_settled_after(prev: f64);
        pub wait wait_for_thumbnails_loaded_after(prev: f64);
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

    /// Map center as (lng, lat). The hook returns the pair as a `Vec<f64>`
    /// for FFI simplicity; this wrapper unpacks to a typed tuple.
    pub async fn center(&self) -> Result<(f64, f64), Box<dyn std::error::Error + Send + Sync>> {
        let arr = self.get_center().await?;
        if arr.len() != 2 {
            return Err(format!("expected [lng, lat], got {arr:?}").into());
        }
        Ok((arr[0], arr[1]))
    }

    /// Run `action` between sampling the fetch-settled counter and awaiting
    /// the next settle event. Closes the listener-attach race (the early
    /// return in `wait_for_fetch_settled_after` covers any settle that lands
    /// between the sample and the await). Used by tests that need to compose
    /// a dynamic action (closure) with the fetch-settle wait — for the fixed
    /// `pan` and `click+fetch` patterns, see `pan_map_to` and
    /// `click_and_wait_for_fetch` which are baked WASM-side.
    pub async fn fetch_around<F>(&self, action: F) -> TestResult
    where
        F: AsyncFnOnce(&Self) -> TestResult,
    {
        let prev = self.current_fetch_settled().await?;
        action(self).await?;
        self.wait_for_fetch_settled_after(prev).await
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
    pub async fn goto_map_with_thumbnails(&self, lng: f64, lat: f64, zoom: f64) -> TestResult {
        self.goto("/").await?;
        self.wait_for_map_idle().await?;
        self.wait_for_fetch_settled_after(0.0).await?;
        let prev_thumbs = self.current_thumbnails_loaded().await?;
        self.pan_map_to(lng, lat, zoom).await?;
        self.wait_for_thumbnails_loaded_after(prev_thumbs).await
    }
}

/// Run a browser test with automatic setup, teardown, and diagnostics.
///
/// Follows chromiumoxide's own test pattern: launches a fresh Chrome, runs the
/// test closure, captures diagnostics on failure, then closes Chrome and awaits
/// the handler.
pub async fn web_test(test: impl AsyncFnOnce(&WebTest) -> TestResult) -> TestResult {
    let t = WebTest::new().await?;
    let result = test(&t).await;

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
