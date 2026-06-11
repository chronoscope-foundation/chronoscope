//! Browser tests for the Chronoscope web frontend.
//!
//! These tests drive a headless Chrome browser against the built WASM frontend,
//! verifying interactive workflows end-to-end: navigation, map interaction,
//! entity detail panels, responsive layouts, accessibility, and error recovery.
//!
//! All JS construction lives in `harness::`; tests below interact only with
//! typed `WebTest` methods. Run via: `just web-test`.

mod harness;

use harness::{TestResult, WebTest, check, web_test};

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
        t.wait_for_body_text("About Chronoscope").await?;

        // Sidebar navigation links
        check_nav_links(t).await?;

        // Wordmark in sidebar
        t.wait_for_body_text("Chronoscope").await?;
        t.wait_for_body_text("Explore places through time").await?;

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

        let url = t.url().await?.ok_or("no URL")?;
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

        let url = t.url().await?.ok_or("no URL")?;
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

        let url = t.url().await?.ok_or("no URL")?;
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
        let article_text = t.text("article, .prose-chronoscope").await?;
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
        t.wait_for_selector("h2").await?;

        t.screenshot("test_faq_accordion_loaded").await?;

        // Should have at least one category heading (h2)
        let category = t.text("h2").await?;
        check(!category.is_empty(), "FAQ should have category headings")?;

        // First FAQ button should have a question as its text
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
        // The animation container is the next element-sibling of the expanded
        // button (see `FaqItem` in web/src/pages/faq.rs).
        let style = t
            .attr("#main-content button[aria-expanded='true'] + div", "style")
            .await?
            .unwrap_or_default();
        check(
            style.contains("1fr"),
            format!("Grid should have 1fr rows when expanded, got style: {style}"),
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
    })
    .await
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
        t.wait_for_selector("[role='complementary']").await?;

        // The panel renders "Loading..." while fetching the entity detail.
        // Wait for a loaded-state token instead of polling for absence of
        // "Loading..." — "Construction completed" is asserted on below, so
        // its presence proves the fetch settled and rendered.
        t.wait_for_body_text("Construction completed").await?;

        let panel_text = t.text("[role='complementary']").await?;

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
        t.wait_for_selector("[role='complementary']").await?;

        let panel_text = t.text("[role='complementary']").await?;

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
        // buttons have aria-labels containing the entity name.
        t.click("[role='complementary'] button[aria-label*='Cathedral']")
            .await?;
        // Wait for entity detail to load (async API fetch) — the timeline
        // section only appears in the detail view, not the picker.
        t.wait_for_body_text("Timeline").await?;

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
            t.wait_for_body_text("Multiple entities").await?;
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
        t.wait_for_body_text("No entities in this area").await?;

        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_map_hover_cursor() -> TestResult {
    web_test(async |t| {
        t.goto_map_at(HAGIA_SOPHIA.0, HAGIA_SOPHIA.1, 14.0).await?;

        // Fire a mousemove at entity coordinates.
        t.fire_canvas_mousemove(HAGIA_SOPHIA.0, HAGIA_SOPHIA.1)
            .await?;

        let cursor = t.map_cursor().await?;
        check(
            cursor == "pointer",
            format!("Cursor should be pointer over markers, got: {cursor}"),
        )?;

        // Fire mousemove at a point far from markers
        t.fire_canvas_mousemove(28.97, 41.00).await?;

        let cursor = t.map_cursor().await?;
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
        t.wait_for_body_text("Every place has layers").await?;

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
        t.click("button[aria-label='About Chronoscope']").await?;

        // Info card should reappear with its content
        t.wait_for_body_text("Every place has layers").await?;

        Ok(())
    })
    .await
}

#[tokio::test]
async fn test_error_banner_custom_event() -> TestResult {
    web_test(async |t| {
        t.goto("/").await?;

        // Dispatch a custom error event via WASM test hook
        t.dispatch_error("Test error message from browser test")
            .await?;

        // Error banner should appear with role=alert
        t.wait_for_selector("[role='alert']").await?;
        t.wait_for_body_text("Test error message from browser test")
            .await?;

        t.screenshot("test_error_banner_visible").await?;

        // Click dismiss via WASM test hook (the error banner's container has
        // pointer-events-none which blocks chromiumoxide's native click)
        t.click("[role=alert] button[aria-label=Dismiss]").await?;

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
        t.set_api_url("http://127.0.0.1:1").await?;

        // Nudge the map to trigger a fetch that will fail against the bogus URL.
        // `pan_map_to` works for failing fetches too — the fetch-settled
        // counter advances on both success and failure paths.
        t.pan_map_to(28.9800, 41.0086, 14.0).await?;
        t.wait_for_body_text("Retry").await?;

        t.screenshot("test_retry_step2_error").await?;

        // Step 3: Restore the real API URL
        let real_url = t.api_base_url();
        t.set_api_url(&real_url).await?;

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
        t.click("a[href='/about']").await?;

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
        t.wait_for_body_text("Chronoscope").await?;
        t.wait_for_body_text("Explore places through time").await?;

        // Desktop sidebar should NOT have a hamburger toggle visible
        // (it exists in DOM but is hidden via md:hidden)
        let toggle_visible = t.is_visible("button[aria-label='Toggle menu']").await?;
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
        t.focus_element("a[href='#main-content']").await?;

        // Verify focus is on the skip link
        let focused_href = t.active_element_attribute("href").await?;
        check(
            focused_href.as_deref() == Some("#main-content"),
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
        t.wait_for_selector("[role='complementary']").await?;

        // Check that the panel or an element within it has focus
        let active_in_panel = t.is_active_inside("[role='complementary']").await?;
        check(active_in_panel, "Panel should capture focus when opened")?;

        // Close the panel via its dismiss button
        t.click("[role='complementary'] button[aria-label='Close']")
            .await?;

        // The panel div stays in DOM but becomes translated off-screen when
        // selection is None. Verify the panel content is gone by checking
        // that the entity-specific "Timeline" or "Links" headings are no
        // longer in the body text.
        check(
            !t.has_text("Timeline").await? && !t.has_text("Links").await?,
            "Panel content should be gone after dismiss",
        )?;

        Ok(())
    })
    .await
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

    let client = Client::new(t.api_base_url());
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
        t.wait_for_selector("[role='complementary'] ul[role='list']")
            .await?;

        let panel_text = t.text("[role='complementary']").await?;
        check(
            panel_text.contains("Images"),
            format!("Panel should show Images section, got: {panel_text}"),
        )?;

        // Verify image elements exist
        let img_count = t
            .count("[role=complementary] ul[role=list] li button img")
            .await?;
        check(
            img_count > 0,
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
        t.wait_for_selector("[role='complementary'] ul[role='list']")
            .await?;

        let labels = t
            .attributes("[role=complementary] ul[role=list] li button", "aria-label")
            .await?;
        let all_have_labels = !labels.is_empty()
            && labels
                .iter()
                .all(|l| l.as_deref().is_some_and(|s| s.contains("view")));
        check(
            all_have_labels,
            format!(
                "Every image button should have an aria-label containing 'view', got {labels:?}"
            ),
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
        t.click("[role='complementary'] ul[role='list'] li button")
            .await?;
        t.wait_for_selector("[role='dialog'][aria-label='Image preview']")
            .await?;

        let img_src = t
            .attr("[role=dialog] img", "src")
            .await?
            .unwrap_or_default();
        check(!img_src.is_empty(), "Lightbox image should have a src")?;

        check(
            t.exists("[role='dialog'] button[aria-label='Close preview']")
                .await?,
            "Lightbox should have a close button",
        )?;

        let original_href = t
            .attr("[role=dialog] a[target=_blank]", "href")
            .await?
            .unwrap_or_default();
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

        t.click("[role='complementary'] ul[role='list'] li button")
            .await?;
        t.wait_for_selector("[role='dialog']").await?;

        t.press_key("[role=dialog]", "Escape").await?;
        // Wait for the dialog to disappear (reactive update after signal change).
        t.wait_for_selector_removal("[role='dialog']").await?;

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

        t.click("[role='complementary'] ul[role='list'] li button")
            .await?;
        t.wait_for_selector("[role='dialog']").await?;

        t.click("[role='dialog'] button[aria-label='Close preview']")
            .await?;
        // Wait for the dialog to disappear (reactive update after signal change).
        t.wait_for_selector_removal("[role='dialog']").await?;

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
        t.wait_for_selector("[role='complementary']").await?;

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

        t.fetch_around(async |t| t.click_map_at(lng, lat).await)
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

        let exact_names = [
            "Castel Sant'Angelo",
            "Trajan's Column",
            "Colosseum",
            "Pantheon",
        ];
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
