//! Browser tests for the Chronoscope web frontend.
//!
//! These tests drive a headless Chrome browser against the built WASM frontend,
//! verifying interactive workflows end-to-end: navigation, map interaction,
//! entity detail panels, responsive layouts, accessibility, and error recovery.
//!
//! Run via: `just web-test` (builds frontend, then runs these tests)

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

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

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
        .arg("--headless=new")
        .arg("--no-sandbox")
        .arg("--disable-gpu")
        .arg("--disable-dev-shm-usage")
        .arg("--window-size=1280,800")
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
fn check(condition: bool, msg: impl std::fmt::Display) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(msg.to_string().into())
    }
}

/// Per-test context: API server + frontend file server + browser.
///
/// Each test gets its own Chrome instance (following chromiumoxide's own test
/// pattern). Call `close()` when done to kill Chrome cleanly and await the
/// handler task.
struct WebTest {
    browser: Browser,
    handler_handle: tokio::task::JoinHandle<()>,
    page: Page,
    frontend_url: String,
    console_logs: Arc<Mutex<Vec<ConsoleEntry>>>,
    screenshot_dir: PathBuf,
    #[allow(dead_code)]
    server: RunningDevServer,
    // Keep temp dirs alive for the duration of the test
    #[allow(dead_code)]
    tmp_dir: tempfile::TempDir,
    #[allow(dead_code)]
    browser_data_dir: tempfile::TempDir,
    #[allow(dead_code)]
    db_tmp: tempfile::TempDir,
}

#[derive(Debug, Clone)]
struct ConsoleEntry {
    level: String,
    text: String,
}

impl WebTest {
    /// Close the browser and await the handler task.
    /// Called by `web_test()` after each test — not called directly by tests.
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

    /// Create a new test backed by the Wikidata test database.
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
            tmp_dir,
            browser_data_dir,
            db_tmp,
        })
    }

    // ---- Timing helpers ----

    /// Evaluate a JS expression that returns a Promise, with a timeout.
    ///
    /// chromiumoxide's `evaluate()` automatically awaits JS Promises (via
    /// CDP's `awaitPromise`). This wraps that with a `tokio::time::timeout`
    /// so the test fails cleanly instead of hanging.
    async fn with_timeout(
        &self,
        js: &str,
        timeout: Duration,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        tokio::time::timeout(timeout, async { self.page.evaluate(js).await })
            .await
            .map_err(|_| format!("Timeout after {timeout:?} waiting for: {js}"))?
            .map_err(|e| format!("JS error in {js}: {e}"))?;
        Ok(())
    }

    /// Navigate to a path and wait for WASM test hooks to register.
    async fn goto(&self, path: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let url = format!("{}{}", self.frontend_url, path);
        self.page.goto(&url).await?.wait_for_navigation().await?;
        // Wait for WASM to boot and register test hooks (`window.__test`).
        // MutationObserver fires when Leptos modifies the DOM during mount.
        // The immediate check handles the case where WASM loaded before we
        // got here; the observer handles the case where it hasn't yet.
        self.with_timeout(
            "new Promise(resolve => { \
                if (window.__test) { resolve(); return; } \
                var obs = new MutationObserver(() => { \
                    if (window.__test) { obs.disconnect(); resolve(); } \
                }); \
                obs.observe(document.documentElement || document.body, \
                    {childList: true, subtree: true}); \
            })",
            TIMEOUT,
        )
        .await
    }

    /// Wait for a CSS selector to appear in the DOM.
    ///
    /// Uses a `MutationObserver` to detect when the element is added, avoiding
    /// poll-based sleeping.
    async fn wait_for(
        &self,
        selector: &str,
        timeout: Duration,
    ) -> Result<chromiumoxide::element::Element, Box<dyn std::error::Error + Send + Sync>> {
        // The JS Promise below checks document.querySelector immediately, then
        // falls back to a MutationObserver — no need for a separate find_element.
        let js = format!(
            "new Promise(resolve => {{ \
                var el = document.querySelector({sel}); \
                if (el) {{ resolve(true); return; }} \
                var obs = new MutationObserver(() => {{ \
                    el = document.querySelector({sel}); \
                    if (el) {{ obs.disconnect(); resolve(true); }} \
                }}); \
                obs.observe(document.body, {{childList: true, subtree: true}}); \
            }})",
            sel = serde_json::to_string(selector)?,
        );
        self.with_timeout(&js, timeout).await?;
        // Element should exist now
        self.page.find_element(selector).await.map_err(|e| {
            format!("Element {selector} not found after MutationObserver resolved: {e}").into()
        })
    }

    /// Wait until an element matching `selector` is removed from the DOM.
    async fn wait_for_removal(
        &self,
        selector: &str,
        timeout: Duration,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let js = format!(
            "new Promise(resolve => {{ \
                if (!document.querySelector({sel})) {{ resolve(); return; }} \
                var obs = new MutationObserver(() => {{ \
                    if (!document.querySelector({sel})) {{ obs.disconnect(); resolve(); }} \
                }}); \
                obs.observe(document.body, {{childList: true, subtree: true}}); \
            }})",
            sel = serde_json::to_string(selector)?,
        );
        self.with_timeout(&js, timeout).await
    }

    /// Wait for text to appear anywhere in the page body.
    ///
    /// Uses a `MutationObserver` to detect DOM changes, checking the body text
    /// after each mutation.
    async fn wait_for_text(
        &self,
        text: &str,
        timeout: Duration,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let js = format!(
            "new Promise(resolve => {{ \
                var target = {text}; \
                if ((document.body?.innerText || '').includes(target)) {{ resolve(); return; }} \
                var obs = new MutationObserver(() => {{ \
                    if ((document.body?.innerText || '').includes(target)) {{ \
                        obs.disconnect(); resolve(); \
                    }} \
                }}); \
                obs.observe(document.body, {{childList: true, subtree: true, characterData: true}}); \
            }})",
            text = serde_json::to_string(text)?,
        );
        self.with_timeout(&js, timeout).await
    }

    /// Check if text is present in the page body right now.
    async fn has_text(&self, text: &str) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        let body_text: String = self
            .page
            .evaluate("document.body?.innerText || ''")
            .await?
            .into_value()?;
        Ok(body_text.contains(text))
    }

    /// Click an element matching a CSS selector, waiting for any transition to complete.
    async fn click(&self, selector: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.wait_for(selector, Duration::from_secs(5)).await?;
        let js = format!(
            "window.__test.clickVisible({})",
            serde_json::to_string(selector)?,
        );
        self.with_timeout(&js, TIMEOUT).await
    }

    /// Get the text content of an element.
    async fn text(
        &self,
        selector: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let el = self.wait_for(selector, Duration::from_secs(5)).await?;
        let text = el.inner_text().await?.unwrap_or_default();
        Ok(text)
    }

    /// Check if an element exists in the DOM.
    async fn exists(
        &self,
        selector: &str,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.page.find_element(selector).await.is_ok())
    }

    /// Get an attribute value from an element.
    async fn attr(
        &self,
        selector: &str,
        attribute: &str,
    ) -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
        let el = self.wait_for(selector, Duration::from_secs(5)).await?;
        el.attribute(attribute).await.map_err(Into::into)
    }

    /// Execute JS and return the result as a `serde_json::Value`.
    async fn evaluate(
        &self,
        js: &str,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
        let result = self.page.evaluate(js).await?;
        Ok(result.into_value()?)
    }

    /// Execute JS and return the result as a `String`.
    async fn eval_string(
        &self,
        js: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let val = self.page.evaluate(js).await?;
        let s: String = val.into_value()?;
        Ok(s)
    }

    /// Set the viewport size for responsive testing.
    async fn set_viewport(
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
    async fn screenshot(
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

    /// Get all console log entries captured so far.
    async fn console_logs(&self) -> Vec<ConsoleEntry> {
        self.console_logs.lock().await.clone()
    }

    /// Dump console logs to stderr (for debugging).
    async fn dump_console_logs(&self) {
        let logs = self.console_logs().await;
        if logs.is_empty() {
            eprintln!("  (no console logs)");
        } else {
            for entry in &logs {
                eprintln!("  [{}] {}", entry.level, entry.text);
            }
        }
    }

    // ---- Test hook wrappers ----
    // These call functions on `window.__test` registered by the WASM-side
    // test_hooks module. No inline JS strings — the logic lives in Rust
    // compiled to WASM, sharing types with the app.

    /// Wait for the MapLibre map to mount and become idle.
    async fn wait_for_map_idle(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // waitForMapExists is registered in register_base (available immediately).
        // waitForMapIdle is registered in register_map_hooks (available after
        // Landing mounts). We chain them: first wait for the map to exist (which
        // implies Landing mounted and map hooks are registered), then wait for idle.
        self.with_timeout("window.__test.waitForMapExists()", TIMEOUT)
            .await?;
        self.with_timeout("window.__test.waitForMapIdle()", TIMEOUT)
            .await
    }

    /// Pan the map to coordinates and wait for entity fetch + map idle.
    async fn pan_map_to(
        &self,
        lng: f64,
        lat: f64,
        zoom: f64,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Register fetch listener BEFORE triggering the jump to avoid missing the event
        self.with_timeout(
            &format!(
                "(async function() {{ \
                    var p = window.__test.waitForFetchComplete(); \
                    window.__test.jumpTo({lng}, {lat}, {zoom}); \
                    await p; \
                }})()"
            ),
            TIMEOUT,
        )
        .await?;
        self.with_timeout("window.__test.waitForMapIdle()", TIMEOUT)
            .await
    }

    /// Click an element and wait for entity fetch to complete.
    /// Registers the fetch listener before clicking to avoid missing the event.
    async fn click_and_wait_for_fetch(
        &self,
        selector: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let sel = serde_json::to_string(selector)?;
        self.with_timeout(
            &format!(
                "(async function() {{ \
                    var p = window.__test.waitForFetchComplete(); \
                    window.__test.clickVisible({sel}); \
                    await p; \
                }})()"
            ),
            TIMEOUT,
        )
        .await
    }

    /// Get the number of rendered entity markers on the map.
    async fn marker_count(&self) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        let count: f64 = self
            .page
            .evaluate("window.__test.markerCount()")
            .await?
            .into_value()?;
        Ok(count as usize)
    }

    /// Fire a MapLibre click event at map coordinates programmatically.
    async fn click_map_at(
        &self,
        lng: f64,
        lat: f64,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.page
            .evaluate(format!("window.__test.fireMapClick({lng}, {lat})"))
            .await?;

        Ok(())
    }

    /// Navigate to the map page, wait for idle, and pan to coordinates.
    async fn goto_map_at(
        &self,
        lng: f64,
        lat: f64,
        zoom: f64,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.goto("/").await?;
        self.wait_for_map_idle().await?;
        self.pan_map_to(lng, lat, zoom).await
    }

    /// Count rendered thumbnail markers (entities with photo pins).
    async fn thumbnail_marker_count(
        &self,
    ) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        let count: f64 = self
            .page
            .evaluate("window.__test.thumbnailMarkerCount()")
            .await?
            .into_value()?;
        Ok(count as usize)
    }

    /// Get rendered marker properties as JSON. Each entry corresponds to one
    /// marker on the map (entity or cluster), with feature properties like
    /// `kind`, `name`, `id`, `thumbnail`.
    ///
    /// Waits two animation frames before querying to ensure MapLibre has
    /// flushed its render pipeline after the most recent setData / idle
    /// cycle. Under concurrent test load, MapLibre may fire "idle" before
    /// all features are queryable via `queryRenderedFeatures`.
    async fn marker_properties(
        &self,
    ) -> Result<Vec<serde_json::Value>, Box<dyn std::error::Error + Send + Sync>> {
        self.page
            .evaluate("new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)))")
            .await?;

        let value: serde_json::Value = self
            .page
            .evaluate("window.__test.markerProperties()")
            .await?
            .into_value()?;
        let arr = value
            .as_array()
            .ok_or("markerProperties did not return an array")?
            .clone();
        Ok(arr)
    }

    /// Return the map's layer IDs in draw order (bottom → top).
    async fn layer_order(&self) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
        let value: serde_json::Value = self
            .page
            .evaluate("window.__test.layerOrder()")
            .await?
            .into_value()?;
        let arr = value
            .as_array()
            .ok_or("layerOrder did not return an array")?
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        Ok(arr)
    }

    /// Get the map's current zoom level.
    async fn zoom(&self) -> Result<f64, Box<dyn std::error::Error + Send + Sync>> {
        let z: f64 = self
            .page
            .evaluate("window.__test.getZoom()")
            .await?
            .into_value()?;
        Ok(z)
    }

    /// Get the map's current center as (lng, lat).
    async fn center(&self) -> Result<(f64, f64), Box<dyn std::error::Error + Send + Sync>> {
        let arr: Vec<f64> = self
            .page
            .evaluate("window.__test.getCenter()")
            .await?
            .into_value()?;
        if arr.len() != 2 {
            return Err(format!("expected [lng, lat], got {arr:?}").into());
        }
        Ok((arr[0], arr[1]))
    }

    /// Navigate to map, pan to coordinates, and wait for both entities and thumbnails.
    async fn goto_map_with_thumbnails(
        &self,
        lng: f64,
        lat: f64,
        zoom: f64,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.goto("/").await?;
        self.wait_for_map_idle().await?;
        // Register thumbnail listener, pan, wait for fetch + idle, then await
        // thumbnails — all inside one async IIFE so the listener is registered
        // before any events can fire.
        self.with_timeout(
            &format!(
                "(async function() {{ \
                    var thumbs = window.__test.waitForThumbnailsLoaded(); \
                    var fetch = window.__test.waitForFetchComplete(); \
                    window.__test.jumpTo({lng}, {lat}, {zoom}); \
                    await fetch; \
                    await window.__test.waitForMapIdle(); \
                    await thumbs; \
                }})()"
            ),
            Duration::from_secs(30),
        )
        .await
    }
}

/// Run a browser test with automatic setup, teardown, and diagnostics.
///
/// Follows chromiumoxide's own test pattern: launches a fresh Chrome, runs the
/// test closure, captures diagnostics on failure, then closes Chrome and awaits
/// the handler.
async fn web_test(test: impl AsyncFnOnce(&WebTest) -> TestResult) -> TestResult {
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

// ==================== Helper ====================

/// Wait timeout for test-hook async helpers.
///
/// Sized for the worst case: `cargo llvm-cov` instrumentation under
/// parallel test load on a saturated machine. Wait helpers short-circuit
/// as soon as their condition is met, so this only affects the failure
/// path — the happy path is still fast.
const TIMEOUT: Duration = Duration::from_secs(60);

/// Hagia Sophia, Istanbul — single entity, good for detail panel tests (lng, lat).
const HAGIA_SOPHIA: (f64, f64) = (28.979917, 41.008528);
/// Torcello Cathedral, Venice — two co-located entities for disambiguation (lng, lat).
const TORCELLO: (f64, f64) = (12.27725, 45.217056);

/// Assert the three sidebar/nav links (Explore, About, FAQ) are present.
async fn check_nav_links(t: &WebTest) -> TestResult {
    let explore = t.text("nav a[href='/']").await?;
    check(
        explore.contains("Explore"),
        format!("Nav should show Explore, got: {explore}"),
    )?;
    let about = t.text("nav a[href='/about']").await?;
    check(
        about.contains("About"),
        format!("Nav should show About, got: {about}"),
    )?;
    let faq = t.text("nav a[href='/faq']").await?;
    check(
        faq.contains("FAQ"),
        format!("Nav should show FAQ, got: {faq}"),
    )
}

// ==================== Navigation & Rendering Tests ====================

#[tokio::test]
async fn test_landing_page_renders() -> TestResult {
    web_test(async |t| {
        t.goto("/").await?;

        // Main content area exists
        check(
            t.exists("#main-content").await?,
            "main-content should exist",
        )?;

        // Info card with "About Chronoscope" should be visible on first load
        t.wait_for_text("About Chronoscope", TIMEOUT).await?;

        // Sidebar navigation links
        check_nav_links(t).await?;

        // Wordmark in sidebar
        t.wait_for_text("Chronoscope", TIMEOUT).await?;
        t.wait_for_text("Explore places through time", TIMEOUT)
            .await?;

        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_navigation_between_pages() -> TestResult {
    web_test(async |t| {
        t.goto("/").await?;

        // Navigate to About via sidebar link
        t.click("a[href='/about']").await?;

        let url = t.page.url().await?.ok_or("no URL")?;
        check(
            url.ends_with("/about"),
            format!("URL should end with /about, got: {url}"),
        )?;

        // Verify heading rendered from markdown
        let heading = t.text("h1").await?;
        check(
            heading.contains("About Chronoscope"),
            format!("About page h1 should say 'About Chronoscope', got: {heading}"),
        )?;

        // Navigate to FAQ
        t.click("a[href='/faq']").await?;

        let url = t.page.url().await?.ok_or("no URL")?;
        check(
            url.ends_with("/faq"),
            format!("URL should end with /faq, got: {url}"),
        )?;

        // FAQ page should have category headings
        let heading = t.text("h1").await?;
        check(
            !heading.is_empty(),
            "FAQ page should have an h1 heading, got empty",
        )?;

        // Navigate back to Explore
        t.click("a[href='/']").await?;

        let url = t.page.url().await?.ok_or("no URL")?;
        check(
            url.ends_with('/') && !url.ends_with("/faq") && !url.ends_with("/about"),
            format!("URL should be root, got: {url}"),
        )?;

        // Map container should be present on landing page
        check(
            t.exists("#main-content").await?,
            "Landing page should have main-content",
        )?;

        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_about_page_content() -> TestResult {
    web_test(async |t| {
        t.goto("/about").await?;

        // Should have the heading inside an h1
        let heading = t.text("h1").await?;
        check(
            heading.contains("About Chronoscope"),
            format!("h1 should say 'About Chronoscope', got: {heading}"),
        )?;

        // Should have rendered markdown content (prose container) with substance
        let article_text = t
            .eval_string("document.querySelector('article, .prose-chronoscope')?.innerText || ''")
            .await?;
        check(
            !article_text.is_empty(),
            "Should have article or prose container with content",
        )?;
        check(
            article_text.len() > 50,
            format!(
                "Article should have substantial content, got {} chars",
                article_text.len()
            ),
        )?;

        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_faq_accordion() -> TestResult {
    web_test(async |t| {
        t.goto("/faq").await?;

        // Wait for FAQ content to render
        t.wait_for("h2", TIMEOUT).await?;

        t.screenshot("test_faq_accordion_loaded").await?;

        // Should have at least one category heading (h2)
        let category = t.text("h2").await?;
        check(!category.is_empty(), "FAQ should have category headings")?;

        // First FAQ button should have a question as its text
        t.wait_for("#main-content button[aria-expanded]", TIMEOUT)
            .await?;
        let question_text = t.text("#main-content button[aria-expanded]").await?;
        check(
            !question_text.is_empty(),
            "FAQ question button should have text, got empty",
        )?;

        // Should be collapsed initially
        let expanded = t
            .attr("#main-content button[aria-expanded]", "aria-expanded")
            .await?;
        check(
            expanded.as_deref() == Some("false"),
            "FAQ item should be collapsed initially",
        )?;

        // Click to expand
        t.click("#main-content button[aria-expanded]").await?;

        let expanded = t
            .attr("#main-content button[aria-expanded]", "aria-expanded")
            .await?;
        check(
            expanded.as_deref() == Some("true"),
            "FAQ item should be expanded after click",
        )?;

        // The answer uses a CSS grid transition: grid-template-rows 0fr → 1fr.
        // Dump the DOM around the expanded button for debugging, then check the
        // grid container's style.
        let grid_rows = t
            .eval_string(
                "(function() { \
                    var btn = document.querySelector('#main-content button[aria-expanded=\"true\"]'); \
                    if (!btn) return 'no-button'; \
                    var parent = btn.closest('div'); \
                    if (!parent) return 'no-parent'; \
                    var divs = parent.querySelectorAll('div[style]'); \
                    for (var d of divs) { \
                        if (d.style.gridTemplateRows) return d.style.gridTemplateRows; \
                    } \
                    return 'no-grid-found:' + parent.innerHTML.substring(0, 200); \
                })()",
            )
            .await?;
        check(
            grid_rows.contains("1fr"),
            format!("Grid should have 1fr rows when expanded, got: {grid_rows}"),
        )?;

        // Click again to collapse
        t.click("#main-content button[aria-expanded='true']")
            .await?;

        let expanded = t
            .attr("#main-content button[aria-expanded]", "aria-expanded")
            .await?;
        check(
            expanded.as_deref() == Some("false"),
            "FAQ item should collapse on second click",
        )?;

        Ok(())
    }).await
}

// ==================== Map & Entity Tests ====================

#[tokio::test]
async fn test_map_loads_entities() -> TestResult {
    web_test(async |t| {
        t.goto_map_at(HAGIA_SOPHIA.0, HAGIA_SOPHIA.1, 14.0).await?;

        // Check that markers appeared
        t.screenshot("test_map_loads_entities").await?;

        let count = t.marker_count().await?;
        check(
            count > 0,
            format!("should have entity markers after panning to Hagia Sophia, got {count}"),
        )?;
        Ok(())
    })
    .await
}

/// Locks in the entity-thumbnails layer being drawn above entity-labels.
/// Without this ordering, label text from one cluster occludes the
/// thumbnail of an adjacent cluster (visually broken). The fix is in
/// `init_source_and_layers`; this test catches the regression if anyone
/// reorders the `add_layer` calls.
#[tokio::test]
async fn test_thumbnails_layer_above_labels() -> TestResult {
    web_test(async |t| {
        t.goto("/").await?;
        t.wait_for_map_idle().await?;

        let layers = t.layer_order().await?;
        let labels_idx = layers
            .iter()
            .position(|l| l == "entity-labels")
            .ok_or("entity-labels layer not found")?;
        let thumbs_idx = layers
            .iter()
            .position(|l| l == "entity-thumbnails")
            .ok_or("entity-thumbnails layer not found")?;
        check(
            thumbs_idx > labels_idx,
            format!(
                "entity-thumbnails ({thumbs_idx}) must be drawn above entity-labels \
                 ({labels_idx}); layer order: {layers:?}"
            ),
        )?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_entity_click_opens_detail() -> TestResult {
    web_test(async |t| {
        t.goto_map_at(HAGIA_SOPHIA.0, HAGIA_SOPHIA.1, 14.0).await?;

        // Click on the marker via programmatic map click event
        t.click_map_at(HAGIA_SOPHIA.0, HAGIA_SOPHIA.1).await?;

        // Detail panel should appear — Hagia Sophia is a single entity at these coords
        t.wait_for("[role='complementary']", TIMEOUT).await?;

        // The panel renders "Loading..." while fetching the entity detail.
        // Under parallel test load (cargo llvm-cov etc.) this would otherwise
        // race the assertions below. Wait for the content to populate.
        t.with_timeout(
            "(async function() { \
                var deadline = Date.now() + 30000; \
                while (Date.now() < deadline) { \
                    var el = document.querySelector(\"[role='complementary']\"); \
                    if (el && !el.textContent.includes('Loading...')) return true; \
                    await new Promise(r => setTimeout(r, 50)); \
                } \
                throw new Error('panel still loading after 30s'); \
            })()",
            TIMEOUT,
        )
        .await?;

        let panel_text = t.text("[role='complementary']").await?;

        // Verify entity type is displayed (the DB has entity_type = "building")
        check(
            panel_text.to_lowercase().contains("building"),
            format!("Panel should show entity type 'building', got: {panel_text}"),
        )?;

        // Panel should have substantial content (names, timeline, links, etc.)
        check(
            panel_text.len() > 20,
            format!("Panel should have content after clicking entity, got: {panel_text}"),
        )?;

        // Verify timeline section — Hagia Sophia has Constructed transitions
        check(
            panel_text.contains("Timeline") || panel_text.contains("constructed"),
            format!("Panel should show timeline section, got: {panel_text}"),
        )?;

        // Hagia Sophia's Constructed has only `completed_at` (year 0537).
        // The year must reach the panel and be labeled as the completion
        // endpoint, not the bare verb.
        check(
            panel_text.contains("537"),
            format!(
                "Panel should show Hagia Sophia's construction completion year 537, got: {panel_text}"
            ),
        )?;
        check(
            panel_text.contains("Construction completed"),
            format!(
                "Panel should label Hagia Sophia's dated construction row as \
                 'Construction completed' (not the bare 'Constructed'), got: {panel_text}"
            ),
        )?;

        // Verify links section — entity has Wikidata links
        check(
            panel_text.contains("Links")
                || panel_text.contains("Wikidata")
                || panel_text.contains("wikidata"),
            format!("Panel should show external links section, got: {panel_text}"),
        )?;

        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_disambiguation_picker() -> TestResult {
    web_test(async |t| {
        t.goto_map_at(TORCELLO.0, TORCELLO.1, 14.0).await?;

        t.click_map_at(TORCELLO.0, TORCELLO.1).await?;

        // Wait for the panel
        t.wait_for("[role='complementary']", TIMEOUT).await?;

        let panel_text = t
            .eval_string("document.querySelector('[role=complementary]')?.innerText || ''")
            .await?;

        // Torcello has two co-located entities — should get disambiguation picker
        check(
            panel_text.contains("Multiple entities"),
            format!("Expected disambiguation picker at Torcello, got: {panel_text}"),
        )?;

        // Should list co-located entities (both are Cathedrals at this location)
        check(
            panel_text.contains("Cathedral"),
            format!("Picker should list co-located entities, got: {panel_text}"),
        )?;

        t.screenshot("test_disambiguation_picker").await?;

        // Click the first picker entry (not the dismiss button). Picker
        // buttons have aria-labels containing the entity name and type.
        t.click("[role='complementary'] button[aria-label*='Cathedral']")
            .await?;
        // Wait for entity detail to load (async API fetch)
        t.wait_for_text("building", TIMEOUT).await?;

        let detail_text = t.text("[role='complementary']").await?;
        check(
            !detail_text.contains("Multiple entities") && detail_text.len() > 20,
            format!("Should show entity detail after picker selection, got: {detail_text}"),
        )?;

        // The first picker entry is v1 Chioggia (earliest_date = 1623, the
        // demolition year): a dateless `Constructed` plus `Demolished
        // completed 1623-12-26`. The dateless Constructed must still render
        // *before* the dated Demolished — lifecycle phase order, not date
        // order.
        check(
            detail_text.contains("date unknown"),
            format!(
                "v1 Chioggia Cathedral should surface its dateless Constructed \
                 row as 'date unknown', got: {detail_text}"
            ),
        )?;
        // The detail panel should show timeline events for this Chioggia
        // Cathedral variant. Verify it has at least a Construction entry.
        check(
            detail_text.contains("Construct"),
            format!(
                "Expected a Constructed/Construction row in Chioggia detail, got: {detail_text}"
            ),
        )?;

        t.screenshot("test_disambiguation_picker_detail").await?;

        // Click back button to return to picker
        if t.exists("button[aria-label*='Back']").await? {
            t.click("button[aria-label*='Back']").await?;

            // Should be back at the picker
            t.wait_for_text("Multiple entities", TIMEOUT).await?;
        }

        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_map_empty_state() -> TestResult {
    web_test(async |t| {
        // Pan to the middle of the Pacific — no entities anywhere near here
        t.goto_map_at(0.0, 0.0, 10.0).await?;

        // Wait for loading to complete and empty state to appear
        t.wait_for_text("No entities in this area", TIMEOUT).await?;

        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_map_hover_cursor() -> TestResult {
    web_test(async |t| {
        t.goto_map_at(HAGIA_SOPHIA.0, HAGIA_SOPHIA.1, 14.0).await?;

        // Fire a mousemove at entity coordinates via WASM test hook.
        t.page
            .evaluate(format!(
                "window.__test.fireCanvasMousemove({}, {})",
                HAGIA_SOPHIA.0, HAGIA_SOPHIA.1
            ))
            .await?;

        // Check cursor style
        let cursor = t.eval_string("window.__test.mapCursor()").await?;
        check(
            cursor == "pointer",
            format!("Cursor should be pointer over markers, got: {cursor}"),
        )?;

        // Fire mousemove at a point far from markers
        t.page
            .evaluate("window.__test.fireCanvasMousemove(28.97, 41.00)")
            .await?;

        let cursor = t.eval_string("window.__test.mapCursor()").await?;
        check(cursor != "pointer", "Cursor should reset after moving away")?;

        Ok(())
    })
    .await
}

// ==================== UI Chrome & Error Recovery Tests ====================

#[tokio::test]
async fn test_info_card_dismiss_restore() -> TestResult {
    web_test(async |t| {
        t.goto("/").await?;

        // Info card should be visible
        t.wait_for_text("Every place has layers", TIMEOUT).await?;

        // Find and click the dismiss button (DismissButton defaults to aria-label="Close")
        t.click("button[aria-label='Close']").await?;

        // Info card text should be gone
        check(
            !t.has_text("Every place has layers").await?,
            "Info card should be dismissed",
        )?;

        // The "?" restore button should appear (aria-label="About Chronoscope")
        check(
            t.exists("button[aria-label='About Chronoscope']").await?,
            "Restore '?' button should appear after dismissing info card",
        )?;

        t.screenshot("test_info_card_dismissed").await?;

        // Reload and check persistence
        t.goto("/").await?;

        // Should still be dismissed (localStorage)
        check(
            !t.has_text("Every place has layers").await?,
            "Info card should remain dismissed after reload",
        )?;

        // Click restore button via WASM test hook (Leptos event handlers may
        // not fire via CDP's native click)
        t.page
            .evaluate("window.__test.clickVisible('button[aria-label=\"About Chronoscope\"]')")
            .await?;

        // Info card should reappear with its content
        t.wait_for_text("Every place has layers", TIMEOUT).await?;

        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_error_banner_custom_event() -> TestResult {
    web_test(async |t| {
        t.goto("/").await?;

        // Dispatch a custom error event via WASM test hook
        t.page
            .evaluate("window.__test.dispatchError('Test error message from browser test')")
            .await?;

        // Error banner should appear with role=alert
        t.wait_for("[role='alert']", TIMEOUT).await?;
        t.wait_for_text("Test error message from browser test", TIMEOUT)
            .await?;

        t.screenshot("test_error_banner_visible").await?;

        // Click dismiss via WASM test hook (the error banner's container has
        // pointer-events-none which blocks chromiumoxide's native click)
        t.page
            .evaluate("window.__test.clickVisible('[role=alert] button[aria-label=Dismiss]')")
            .await?;

        // Banner should be gone
        check(
            !t.has_text("Test error message from browser test").await?,
            "Error should be dismissed",
        )?;

        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_fetch_error_retry_button() -> TestResult {
    // Error→retry→recovery without touching the server at all:
    // 1. Load page, verify entities appear (API works)
    // 2. Swap API URL to a bogus value → next fetch fails
    // 3. Pan map → error state with retry button
    // 4. Swap API URL back to the real server
    // 5. Click retry → entities reload
    web_test(async |t| {
        // Step 1: Verify entities load normally
        t.goto_map_at(HAGIA_SOPHIA.0, HAGIA_SOPHIA.1, 14.0).await?;
        let count = t.marker_count().await?;
        check(
            count > 0,
            format!("entities should load initially, got {count}"),
        )?;

        // Step 2: Break the API by pointing at a bogus URL, then trigger a
        // re-fetch at the SAME viewport (where entities exist) via the retry
        // signal. This way we don't change the viewport — we stay right where
        // the entities are, but the fetch fails because the URL is wrong.
        t.page
            .evaluate("window.__test.setApiUrl('http://127.0.0.1:1')")
            .await?;

        // Nudge the map to trigger a fetch that will fail against the bogus URL.
        t.page
            .evaluate("window.__test.jumpTo(28.9800, 41.0086, 14)")
            .await?;
        t.wait_for_text("Retry", TIMEOUT).await?;

        t.screenshot("test_retry_step2_error").await?;

        // Step 3: Restore the real API URL
        let real_url = format!("http://127.0.0.1:{}", t.server.port);
        t.page
            .evaluate(format!("window.__test.setApiUrl('{real_url}')"))
            .await?;

        // Step 4: Click retry and wait for fetch to complete.
        t.click_and_wait_for_fetch("button[aria-label*=\"Retry\"]")
            .await?;

        // Verify recovery — entities should be back at the same viewport
        let count = t.marker_count().await?;
        check(
            count > 0,
            format!("entities should reload after clicking retry, got {count}"),
        )?;

        Ok(())
    })
    .await
}

// ==================== Responsive Layout Tests ====================

#[tokio::test]
async fn test_mobile_layout() -> TestResult {
    web_test(async |t| {
        // Set mobile viewport
        t.set_viewport(375, 667).await?;
        t.goto("/").await?;

        // Mobile top bar should be visible — check for the hamburger toggle
        let has_toggle = t.exists("button[aria-label='Toggle menu']").await?;
        check(has_toggle, "Mobile hamburger should be visible")?;

        // Check initial state: menu closed
        let expanded = t
            .attr("button[aria-label='Toggle menu']", "aria-expanded")
            .await?;
        check(
            expanded.as_deref() == Some("false"),
            "Menu should be closed initially",
        )?;

        // Click hamburger to open drawer
        t.click("button[aria-label='Toggle menu']").await?;

        let expanded = t
            .attr("button[aria-label='Toggle menu']", "aria-expanded")
            .await?;
        check(
            expanded.as_deref() == Some("true"),
            "Menu should be open after click",
        )?;

        t.screenshot("test_mobile_layout_drawer_open").await?;

        // Click a visible nav link (the drawer's, not the hidden desktop sidebar's)
        t.page
            .evaluate("window.__test.clickVisible('a[href=\"/about\"]')")
            .await?;

        let expanded = t
            .attr("button[aria-label='Toggle menu']", "aria-expanded")
            .await?;
        check(
            expanded.as_deref() == Some("false"),
            "Menu should close after navigation",
        )?;

        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_desktop_layout() -> TestResult {
    web_test(async |t| {
        // Set desktop viewport
        t.set_viewport(1280, 800).await?;
        t.goto("/").await?;

        // Sidebar navigation links
        check_nav_links(t).await?;

        // Wordmark and tagline
        t.wait_for_text("Chronoscope", TIMEOUT).await?;
        t.wait_for_text("Explore places through time", TIMEOUT)
            .await?;

        // Desktop sidebar should NOT have a hamburger toggle visible
        // (it exists in DOM but is hidden via md:hidden)
        let toggle_visible: bool = t
            .evaluate(
                "(function() { \
                    var btn = document.querySelector('button[aria-label=\"Toggle menu\"]'); \
                    if (!btn) return false; \
                    return btn.offsetParent !== null; \
                })()",
            )
            .await?
            .as_bool()
            .unwrap_or(true);
        check(
            !toggle_visible,
            "Hamburger toggle should be hidden on desktop",
        )?;

        Ok(())
    })
    .await
}

// ==================== Accessibility Tests ====================

#[tokio::test]
async fn test_skip_link() -> TestResult {
    web_test(async |t| {
        t.goto("/").await?;

        // The skip link should exist but be visually hidden
        check(
            t.exists("a[href='#main-content']").await?,
            "Skip link should exist in DOM",
        )?;

        // Simulate Tab key press to focus the skip link
        t.page
            .evaluate("document.querySelector('a[href=\"#main-content\"]').focus()")
            .await?;

        // Verify focus is on the skip link
        let focused_href: serde_json::Value = t
            .evaluate("document.activeElement?.getAttribute('href')")
            .await?;
        check(
            focused_href.as_str() == Some("#main-content"),
            "Skip link should be focusable",
        )?;

        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_detail_panel_focus() -> TestResult {
    web_test(async |t| {
        t.goto_map_at(HAGIA_SOPHIA.0, HAGIA_SOPHIA.1, 14.0).await?;

        t.click_map_at(HAGIA_SOPHIA.0, HAGIA_SOPHIA.1).await?;

        // Panel should open — wait for it to appear in the DOM
        t.wait_for("[role='complementary']", TIMEOUT).await?;

        // Check that the panel or an element within it has focus
        let active_in_panel: bool = t
            .evaluate(
                "document.querySelector('[role=complementary]')?.contains(document.activeElement) || \
                 document.activeElement === document.querySelector('[role=complementary]')",
            )
            .await?
            .as_bool()
            .unwrap_or(false);

        check(active_in_panel, "Panel should capture focus when opened")?;

        // Close the panel via its dismiss button
        t.click("[role='complementary'] button[aria-label='Close']").await?;

        // The panel div stays in DOM but becomes translated off-screen when
        // selection is None. Check that the selection was cleared by verifying
        // the entity type is no longer visible (building type from Hagia Sophia).
        // Verify the panel content is gone by checking that the entity-specific
        // "Timeline" or "Links" headings are no longer in the body text.
        let body = t.eval_string("document.body?.innerText || ''").await?;
        check(
            !body.contains("Timeline") && !body.contains("Links"),
            format!("Panel content should be gone after dismiss, body: {body}"),
        )?;

        Ok(())
    }).await
}

// ==================== Image & Thumbnail Tests ====================
//
// These tests rely on seed_test_media() having resolved pending research URLs
// with placeholder images during WebTest setup.

/// Find an entity that has resolved media via the typed API client, returning its (lng, lat).
///
/// Uses the markers endpoint to find entities with thumbnails, then fetches
/// detail to find one with media.
async fn find_entity_with_media(
    t: &WebTest,
) -> Result<(f64, f64), Box<dyn std::error::Error + Send + Sync>> {
    use chronoscope_api_client::{Bbox, Client};

    let client = Client::new(format!("http://127.0.0.1:{}", t.server.port));
    // Use a bbox small enough that the server returns individual entity
    // markers (not clusters). Centered on Rome where we have 4 entities
    // — well below the ENTITY_MARKER_THRESHOLD.
    let bbox = Bbox::new(41.5, 42.5, 12.0, 13.0)?;
    let response = client.list_markers(&bbox).await?;

    // 2. Find markers with thumbnails (indicating resolved media)
    let mut best: Option<(f64, f64, usize, String)> = None;
    for marker in &response.markers {
        if marker.thumbnail_url.is_some() {
            // Get the entity ID from the click action
            let entity_id = match &marker.click_action {
                chronoscope_api_client::ClickAction::Select { entity_id, .. } => entity_id.clone(),
                _ => continue,
            };
            let detail = client.get_entity(&entity_id).await?;
            let count = detail.media.len();
            if best.as_ref().is_none_or(|b| count > b.2) {
                best = Some((
                    marker.longitude,
                    marker.latitude,
                    count,
                    entity_id.to_string(),
                ));
            }
        }
    }

    let (lng, lat, count, id) =
        best.ok_or("No entity with resolved media found — did seed_test_media run?")?;
    eprintln!("Found entity {id} with {count} media at ({lng}, {lat})");
    Ok((lng, lat))
}

#[tokio::test]
async fn test_detail_panel_shows_images() -> TestResult {
    web_test(async |t| {
        // Find an entity that actually has media via the API (coordinates vary per run)
        let (lng, lat) = find_entity_with_media(t).await?;

        t.goto_map_at(lng, lat, 14.0).await?;
        t.click_map_at(lng, lat).await?;

        // Wait for the image grid to appear (entity detail loads media async)
        t.wait_for("[role='complementary'] ul[role='list']", TIMEOUT)
            .await?;

        let panel_text = t.text("[role='complementary']").await?;
        check(
            panel_text.contains("Images"),
            format!("Panel should show Images section, got: {panel_text}"),
        )?;

        // Verify image elements exist
        let img_count = t
            .evaluate(
                "document.querySelectorAll(\
                    '[role=complementary] ul[role=list] li button img'\
                ).length",
            )
            .await?
            .as_f64()
            .unwrap_or(0.0);
        check(
            img_count > 0.0,
            format!("Should have image thumbnails in grid, got {img_count}"),
        )?;

        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_image_grid_accessibility() -> TestResult {
    web_test(async |t| {
        let (lng, lat) = find_entity_with_media(t).await?;
        t.goto_map_at(lng, lat, 14.0).await?;
        t.click_map_at(lng, lat).await?;
        t.wait_for("[role='complementary'] ul[role='list']", TIMEOUT)
            .await?;

        let all_have_labels = t
            .evaluate(
                "Array.from(document.querySelectorAll(\
                    '[role=complementary] ul[role=list] li button'\
                )).every(b => b.getAttribute('aria-label')?.includes('view'))",
            )
            .await?
            .as_bool()
            .unwrap_or(false);
        check(
            all_have_labels,
            "Every image button should have an aria-label containing 'view'",
        )?;

        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_lightbox_opens_and_shows_content() -> TestResult {
    web_test(async |t| {
        let (lng, lat) = find_entity_with_media(t).await?;
        t.goto_map_at(lng, lat, 14.0).await?;
        t.click_map_at(lng, lat).await?;
        t.wait_for("[role='complementary'] ul[role='list'] li button", TIMEOUT)
            .await?;

        t.click("[role='complementary'] ul[role='list'] li button")
            .await?;

        t.wait_for("[role='dialog'][aria-label='Image preview']", TIMEOUT)
            .await?;

        let img_src = t
            .eval_string("document.querySelector('[role=dialog] img')?.src || ''")
            .await?;
        check(!img_src.is_empty(), "Lightbox image should have a src")?;

        check(
            t.exists("[role='dialog'] button[aria-label='Close preview']")
                .await?,
            "Lightbox should have a close button",
        )?;

        let original_href = t
            .eval_string("document.querySelector('[role=dialog] a[target=_blank]')?.href || ''")
            .await?;
        check(
            original_href.starts_with("http"),
            format!("'Open original' should link to upstream URL, got: {original_href}"),
        )?;

        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_lightbox_dismiss_escape() -> TestResult {
    web_test(async |t| {
        let (lng, lat) = find_entity_with_media(t).await?;
        t.goto_map_at(lng, lat, 14.0).await?;
        t.click_map_at(lng, lat).await?;
        t.wait_for("[role='complementary'] ul[role='list'] li button", TIMEOUT)
            .await?;

        t.click("[role='complementary'] ul[role='list'] li button")
            .await?;
        t.wait_for("[role='dialog']", TIMEOUT).await?;

        t.page
            .evaluate(
                "document.querySelector('[role=dialog]')\
                 .dispatchEvent(new KeyboardEvent('keydown', {key: 'Escape', bubbles: true}))",
            )
            .await?;
        // Wait for the dialog to disappear (reactive update after signal change).
        t.wait_for_removal("[role='dialog']", TIMEOUT).await?;

        check(
            !t.exists("[role='dialog']").await?,
            "Lightbox should close on Escape",
        )?;

        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_lightbox_dismiss_close_button() -> TestResult {
    web_test(async |t| {
        let (lng, lat) = find_entity_with_media(t).await?;
        t.goto_map_at(lng, lat, 14.0).await?;
        t.click_map_at(lng, lat).await?;
        t.wait_for("[role='complementary'] ul[role='list'] li button", TIMEOUT)
            .await?;

        t.click("[role='complementary'] ul[role='list'] li button")
            .await?;
        t.wait_for("[role='dialog']", TIMEOUT).await?;

        t.click("[role='dialog'] button[aria-label='Close preview']")
            .await?;
        // Wait for the dialog to disappear (reactive update after signal change).
        t.wait_for_removal("[role='dialog']", TIMEOUT).await?;

        check(
            !t.exists("[role='dialog']").await?,
            "Lightbox should close on close button click",
        )?;

        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_map_shows_thumbnail_markers() -> TestResult {
    web_test(async |t| {
        let (lng, lat) = find_entity_with_media(t).await?;
        t.goto_map_with_thumbnails(lng, lat, 14.0).await?;

        let thumb_count = t.thumbnail_marker_count().await?;
        check(
            thumb_count > 0,
            format!("Should have thumbnail markers, got {thumb_count}"),
        )?;

        t.screenshot("test_map_shows_thumbnail_markers").await?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_thumbnail_click_opens_detail() -> TestResult {
    web_test(async |t| {
        let (lng, lat) = find_entity_with_media(t).await?;
        t.goto_map_with_thumbnails(lng, lat, 14.0).await?;

        t.click_map_at(lng, lat).await?;
        t.wait_for("[role='complementary']", TIMEOUT).await?;

        let panel_text = t.text("[role='complementary']").await?;
        check(
            panel_text.len() > 20,
            format!("Panel should have content after clicking thumbnail, got: {panel_text}"),
        )?;

        Ok(())
    })
    .await
}

// ==================== Cluster Tests ====================
//
// These tests rely on the curated wikidata bundle (see nix/wikidata.nix), which
// includes 13 Italian entities spread across 8 regions plus the existing
// Chioggia Cathedral. The expected counts and region names match the data
// produced by `cosmogony` from a pinned OSM Italy snapshot.

/// Italy center — for cluster tests at country/state zoom levels.
const ITALY_LNG: f64 = 12.5;
const ITALY_LAT: f64 = 42.5;
/// Rome — for testing zoomed-in views with 4 distinct entities.
const ROME_LNG: f64 = 12.48;
const ROME_LAT: f64 = 41.9;

/// Get markers of a given kind ("entity" or "cluster"), keyed by their `name`.
fn markers_by_name(
    markers: &[serde_json::Value],
    kind: &str,
) -> std::collections::HashMap<String, serde_json::Value> {
    markers
        .iter()
        .filter(|m| m.get("kind").and_then(|k| k.as_str()) == Some(kind))
        .filter_map(|m| {
            let name = m.get("name")?.as_str()?.to_string();
            Some((name, m.clone()))
        })
        .collect()
}

#[tokio::test]
async fn test_clusters_have_expected_properties() -> TestResult {
    web_test(async |t| {
        // Wide viewport over Italy at zoom 5 — enough entities (>10) to
        // trigger server-side clustering at some granularity.
        t.goto_map_at(ITALY_LNG, ITALY_LAT, 5.0).await?;

        let markers = t.marker_properties().await?;
        let clusters = markers_by_name(&markers, "cluster");
        let entities = markers_by_name(&markers, "entity");

        check(
            entities.is_empty(),
            format!("expected only clusters (no entities) in wide viewport, got {entities:?}"),
        )?;
        check(
            !clusters.is_empty(),
            "expected clusters in wide viewport, got none".to_string(),
        )?;

        // Each cluster needs a bbox for fitBounds when clicked.
        for (name, cluster) in &clusters {
            check(
                cluster.get("bbox_min_lat").is_some(),
                format!("cluster '{name}' should have bbox for fitBounds"),
            )?;
        }
        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_cluster_click_zooms_in() -> TestResult {
    web_test(async |t| {
        t.goto_map_at(ITALY_LNG, ITALY_LAT, 5.0).await?;
        let zoom_before = t.zoom().await?;

        // Find any cluster's rendered coordinates. The server picks the
        // finest granularity that fits — we don't assume a specific level.
        let markers = t.marker_properties().await?;
        let first_cluster = markers
            .iter()
            .find(|m| m.get("kind").and_then(|k| k.as_str()) == Some("cluster"))
            .ok_or("no clusters found at zoom 5")?;
        let lng = first_cluster
            .get("_lng")
            .and_then(|v| v.as_f64())
            .ok_or("cluster has no _lng")?;
        let lat = first_cluster
            .get("_lat")
            .and_then(|v| v.as_f64())
            .ok_or("cluster has no _lat")?;

        // Register a fetch-complete listener BEFORE triggering the click,
        // so we don't miss the event the moveend debounce dispatches after
        // the flyTo. Then wait for both the fetch and the post-flyTo idle.
        t.with_timeout(
            &format!(
                "(async function() {{ \
                    var p = window.__test.waitForFetchComplete(); \
                    window.__test.fireMapClick({lng}, {lat}); \
                    await p; \
                }})()"
            ),
            TIMEOUT,
        )
        .await?;
        t.wait_for_map_idle().await?;

        let zoom_after = t.zoom().await?;
        check(
            zoom_after > zoom_before,
            format!(
                "clicking cluster should increase zoom, before={zoom_before}, after={zoom_after}"
            ),
        )?;

        let (cx, cy) = t.center().await?;
        // After fitBounds, the center should be within the clicked cluster's region.
        // The fit targets the region's bbox, so the center won't be exactly
        // at the clicked centroid — allow generous slack.
        check(
            (cx - lng).abs() < 5.0 && (cy - lat).abs() < 5.0,
            format!("after click, map center should be near ({lng}, {lat}), got ({cx}, {cy})"),
        )?;

        // After fitBounds into cluster, we should see markers (either
        // deeper clusters at state_district/city level, or entities if the
        // region is small enough to land past the cluster threshold).
        let after_markers = t.marker_properties().await?;
        check(
            !after_markers.is_empty(),
            "expected markers after clicking cluster".to_string(),
        )?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_zoom_in_to_rome_shows_4_entities() -> TestResult {
    web_test(async |t| {
        // Zoom 13 over Rome — few enough entities for the server to return individuals.
        t.goto_map_at(ROME_LNG, ROME_LAT, 13.0).await?;

        let markers = t.marker_properties().await?;
        let clusters = markers_by_name(&markers, "cluster");
        let entities = markers_by_name(&markers, "entity");

        check(
            clusters.is_empty(),
            format!("expected no cluster markers at high zoom, got {clusters:?}"),
        )?;

        // The Wikidata Pantheon entity (Q99309) has multiple English labels:
        // both "Pantheon" and "Pantheon, Rome" appear in the names list,
        // and `best_name("en")` returns whichever comes first. The order is
        // determined by how Wikidata's labels deserialize and isn't strictly
        // pinned, so accept either form via a prefix match.
        //
        // Other entities have a single canonical English label and are
        // matched exactly.
        let exact_names = ["Castel Sant'Angelo", "Trajan's Column", "Colosseum"];
        for name in exact_names {
            check(
                entities.contains_key(name),
                format!(
                    "expected entity '{name}' at Rome zoom 13, got: {:?}",
                    entities.keys()
                ),
            )?;
        }
        check(
            entities.keys().any(|k| k.starts_with("Pantheon")),
            format!(
                "expected a 'Pantheon*' entity at Rome zoom 13, got: {:?}",
                entities.keys()
            ),
        )?;

        check(
            entities.len() == 4,
            format!(
                "expected exactly 4 Rome entities, got {}: {:?}",
                entities.len(),
                entities.keys()
            ),
        )?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_rome_entities_have_thumbnails() -> TestResult {
    web_test(async |t| {
        t.goto_map_with_thumbnails(ROME_LNG, ROME_LAT, 13.0).await?;

        let markers = t.marker_properties().await?;
        let entities = markers_by_name(&markers, "entity");
        check(
            !entities.is_empty(),
            "expected entity markers in Rome at zoom 13",
        )?;

        let with_thumbs: Vec<&String> = entities
            .iter()
            .filter(|(_, m)| m.get("thumbnail").is_some())
            .map(|(name, _)| name)
            .collect();
        check(
            !with_thumbs.is_empty(),
            format!(
                "expected at least one Rome entity with a thumbnail, got: {:?}",
                entities.keys()
            ),
        )?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_cluster_thumbnail_shows_representative() -> TestResult {
    web_test(async |t| {
        // At zoom 5, the Lazio cluster's representative is one of the 4 Roman
        // entities. Since Rome has the most entities, at least one should have
        // resolved media (via seed_test_media), giving Lazio a thumbnail.
        t.goto_map_with_thumbnails(ITALY_LNG, ITALY_LAT, 5.0)
            .await?;

        let markers = t.marker_properties().await?;
        let clusters = markers_by_name(&markers, "cluster");
        // Find any cluster with a thumbnail (representative has resolved media).
        let with_thumbnail = clusters.values().find(|c| c.get("thumbnail").is_some());
        check(
            with_thumbnail.is_some(),
            format!(
                "expected at least one cluster with a thumbnail, got: {:?}",
                clusters.keys()
            ),
        )?;
        Ok(())
    })
    .await
}

/// Verify the cluster→entity mode transition.
///
/// At zoom 9 over Rome the server should return clusters (many entities
/// in the wider bbox); at zoom 11 centered tightly on Rome it should
/// return individual entities (few enough in the bbox). This test ensures
/// the transition happens without errors and the marker types change.
#[tokio::test]
async fn test_density_based_cluster_transition() -> TestResult {
    web_test(async |t| {
        // Wide viewport over Italy — server returns clusters (>10 entities).
        t.goto_map_at(ITALY_LNG, ITALY_LAT, 5.0).await?;

        let wide_markers = t.marker_properties().await?;
        let wide_clusters = markers_by_name(&wide_markers, "cluster");
        check(
            !wide_clusters.is_empty(),
            format!(
                "expected clusters in wide viewport, got: {:?}",
                wide_markers
            ),
        )?;

        // Narrow viewport over Rome — server returns entities (4 < threshold).
        t.goto_map_at(ROME_LNG, ROME_LAT, 13.0).await?;

        let narrow_markers = t.marker_properties().await?;
        let narrow_entities = markers_by_name(&narrow_markers, "entity");
        let narrow_clusters = markers_by_name(&narrow_markers, "cluster");
        check(
            narrow_clusters.is_empty(),
            format!(
                "expected no clusters in narrow viewport, got: {:?}",
                narrow_clusters.keys()
            ),
        )?;
        check(
            !narrow_entities.is_empty(),
            format!(
                "expected entities in narrow viewport, got: {:?}",
                narrow_markers
            ),
        )?;
        Ok(())
    })
    .await
}
