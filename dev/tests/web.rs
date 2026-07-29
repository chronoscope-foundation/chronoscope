//! Browser tests for the Chronoscope web frontend.
//!
//! These tests drive a headless Chrome browser against the built WASM frontend,
//! verifying interactive workflows end-to-end: navigation, map interaction,
//! entity detail panels, responsive layouts, accessibility, and error recovery.
//!
//! All JS construction lives in `harness::`; tests below interact only with
//! typed `WebTest` methods. Run via: `just web-test`.

mod harness;

use harness::{TestResult, WebTest, check, web_test, web_test_seeded};

use chronoscope_api::state::ServerIds;
use chronoscope_core::date::{DatePrecision, UncertainDate};
use chronoscope_core::geo::{GeoPoint, Meters};
use chronoscope_core::grammar::assertions::FactualAssertion;
use chronoscope_core::grammar::attribute::{self, NameText, NameType};
use chronoscope_core::grammar::bookend::{ConstructionFact, DemolitionFact};
use chronoscope_core::grammar::citations::{Excerpt, ExternalSource, FactualCitation, Language};
use chronoscope_core::grammar::ids::UserId;
use chronoscope_core::location::{Location, UnresolvedLocation};
use chronoscope_core::submit::{Commit, CommitAuthor, Decl, EntityIdx, SubmitFact};

type SeedResult = Result<Commit<ServerIds>, Box<dyn std::error::Error + Send + Sync>>;

/// Hagia Sophia, Istanbul — single entity, good for detail panel tests (lng, lat).
const HAGIA_SOPHIA: (f64, f64) = (28.979917, 41.008528);
/// Chioggia Cathedral, Venice lagoon (lng, lat). Demolished 1623 and refounded
/// 1633 with no rebuild construction, so it projects as one entity carrying an
/// existence-after-demolition conflict.
const CHIOGGIA_CATHEDRAL: (f64, f64) = (12.27725, 45.217056);

// ==================== Fact seeding helpers ====================
//
// Browser clustering tests seed entities at open-ocean coordinates (far from
// any curated entity) so the map geometry under test is fully controlled. The
// prime meridian (lon 0) is a quadkey tile boundary at *every* level, so two
// entities placed a few meters apart across it always land in distinct server
// cells — exactly the near-overlap case client proximity clustering must fold.

fn seed_citation(url: &str) -> Result<FactualCitation, Box<dyn std::error::Error + Send + Sync>> {
    Ok(FactualCitation::new(
        ExternalSource::Url {
            url: url::Url::parse(url)?,
            published: None,
        },
        vec![Excerpt::new("seed")?],
    )?)
}

/// The name + construction-location facts that make an entity nameable and
/// placeable — the minimum for a rendered tile marker.
fn name_and_location_facts(
    name: &str,
    lat: f64,
    lon: f64,
) -> Result<Vec<SubmitFact>, Box<dyn std::error::Error + Send + Sync>> {
    let location = UnresolvedLocation::Resolved(Location::circle(
        GeoPoint::new(lat, lon)?,
        Meters::try_new(10.0)?,
    )?);
    Ok(vec![
        SubmitFact::Factual {
            assertion: FactualAssertion::Attribute {
                fact: attribute::Fact::Name {
                    entity: EntityIdx(0),
                    name: NameText::new(name)?,
                    language: Language::new("en")?,
                    name_type: NameType::Common,
                    valid_from: None,
                    valid_to: None,
                },
            },
            citation: seed_citation("https://example.com/seed-name")?,
        },
        SubmitFact::Factual {
            assertion: FactualAssertion::Construction {
                fact: ConstructionFact::Location {
                    entity: EntityIdx(0),
                    location,
                },
            },
            citation: seed_citation("https://example.com/seed-location")?,
        },
    ])
}

/// A commit placing one named entity at `(lat, lon)` that was built in `built`
/// and demolished in `demolished` — so it exists at instants between them and
/// nowhere else.
fn seed_demolished_entity_at(
    name: &str,
    lat: f64,
    lon: f64,
    built: i32,
    demolished: i32,
) -> SeedResult {
    let year = |y: i32| -> Result<UncertainDate, Box<dyn std::error::Error + Send + Sync>> {
        Ok(UncertainDate::with_precision(
            chrono::NaiveDate::from_ymd_opt(y, 1, 1).ok_or("valid year")?,
            DatePrecision::Year,
        )?)
    };
    let mut facts = name_and_location_facts(name, lat, lon)?;
    facts.push(SubmitFact::Factual {
        assertion: FactualAssertion::Construction {
            fact: ConstructionFact::Started {
                entity: EntityIdx(0),
                bound: year(built)?,
            },
        },
        citation: seed_citation("https://example.com/seed-built")?,
    });
    facts.push(SubmitFact::Factual {
        assertion: FactualAssertion::Demolition {
            fact: DemolitionFact::Completed {
                entity: EntityIdx(0),
                bound: year(demolished)?,
            },
        },
        citation: seed_citation("https://example.com/seed-demolished")?,
    });
    Ok(Commit::<ServerIds> {
        author: CommitAuthor::User(UserId::new("seed")?),
        recorded_at: chrono::Utc::now(),
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: facts.into_iter().collect(),
    })
}

/// A commit placing one named entity at `(lat, lon)`.
fn seed_entity_at(name: &str, lat: f64, lon: f64) -> SeedResult {
    Ok(Commit::<ServerIds> {
        author: CommitAuthor::User(UserId::new("seed")?),
        recorded_at: chrono::Utc::now(),
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: name_and_location_facts(name, lat, lon)?
            .into_iter()
            .collect(),
    })
}

/// A rendered marker's projected screen position (`_x`/`_y`, CSS pixels), if
/// the descriptor carries one.
fn screen_xy(marker: &serde_json::Value) -> Option<(f64, f64)> {
    Some((marker.get("_x")?.as_f64()?, marker.get("_y")?.as_f64()?))
}

/// Whether a projected screen position falls within the map-canvas rectangle
/// `[0, width] × [0, height]`. A rendered marker whose `_x`/`_y` is inside these
/// bounds is genuinely on-screen — the direction-B guarantee that a populated
/// viewport shows a marker the user can see, not one whose representative fell
/// off the visible edge.
fn in_canvas(xy: (f64, f64), width: f64, height: f64) -> bool {
    (0.0..=width).contains(&xy.0) && (0.0..=height).contains(&xy.1)
}

/// The map canvas size in CSS pixels as `(width, height)` — the frame
/// `marker_properties`/`badge_properties` project into.
async fn canvas_size(t: &WebTest) -> Result<(f64, f64), Box<dyn std::error::Error + Send + Sync>> {
    let size = t.map_canvas_size().await?;
    Ok((
        size.first().copied().unwrap_or(0.0),
        size.get(1).copied().unwrap_or(0.0),
    ))
}

/// Every rendered map feature — individual markers plus cluster badges. Both
/// `marker_properties` and `badge_properties` are QRF-backed (they return only
/// in-viewport features), so this is exactly what the user currently sees.
async fn rendered_features(
    t: &WebTest,
) -> Result<Vec<serde_json::Value>, Box<dyn std::error::Error + Send + Sync>> {
    let mut features = t.marker_properties().await?;
    features.extend(t.badge_properties().await?);
    Ok(features)
}

/// The time slider rewinds the map: an entity demolished long ago is absent
/// today and so draws nothing, appears when the slider reaches a year inside its
/// life, and vanishes again on the return to now.
///
/// This is the whole feature end to end — the slider's DOM event, the refetch it
/// debounces, the server's verdict at that instant, and the render's decision to
/// draw or drop the marker.
#[tokio::test]
async fn test_time_slider_reveals_a_demolished_entity() -> TestResult {
    let seeds = vec![seed_demolished_entity_at(
        "Old Lighthouse",
        25.0,
        -40.0,
        1700,
        1800,
    )?];
    web_test_seeded(seeds, async |t| {
        t.goto_map_at(-40.0, 25.0, 11.0).await?;

        // Today: the lighthouse came down in 1800, so nothing is drawn.
        check(
            rendered_features(t).await?.is_empty(),
            "an entity demolished in 1800 must not render on the present-day map",
        )?;

        // Rewound inside its life: the marker appears.
        t.set_time_slider_year(1750.0).await?;
        check(
            !rendered_features(t).await?.is_empty(),
            "rewinding to 1750 must reveal an entity that stood from 1700 to 1800",
        )?;

        // Before it was built: gone again — the slider reads both directions.
        t.set_time_slider_year(1650.0).await?;
        check(
            rendered_features(t).await?.is_empty(),
            "rewinding past the construction date must hide it again",
        )?;

        // Back into its life, so the reset below has something to take away —
        // asserting emptiness from an already-empty view would pass no matter
        // where the reset landed.
        t.set_time_slider_year(1750.0).await?;
        check(
            !rendered_features(t).await?.is_empty(),
            "returning to 1750 must reveal the entity again",
        )?;

        // Scrubbing to the right edge lands on the present, where the entity is
        // long gone: the marker must disappear, and the readout must show that
        // year. This is the gesture that replaced a "Now" button — the button did
        // nothing the track's own end doesn't.
        //
        // The target comes from the track's own `max`, not from `Utc::now()`.
        // The component derives its maximum from the *browser's local* date, so
        // across a UTC/local year boundary a UTC-derived year exceeds `max`, the
        // input silently clamps, and the assertion fails on the wall clock —
        // inside the commit gate, which is exactly where that is disqualifying.
        let max_year: i32 = t
            .attr("#time-slider", "max")
            .await?
            .unwrap_or_default()
            .parse()
            .map_err(|e| format!("the slider should carry a numeric max: {e}"))?;
        let this_year = max_year;
        t.set_time_slider_year(f64::from(this_year)).await?;
        check(
            rendered_features(t).await?.is_empty(),
            "scrubbing to the present must show the map as it is today, where the entity is absent",
        )?;
        let shown_year = t.text("#time-slider-year").await?;
        check(
            shown_year.trim() == this_year.to_string(),
            format!(
                "at the track's right edge the slider must read {this_year}, got: {shown_year:?}"
            ),
        )?;
        Ok(())
    })
    .await
}

/// Every disclosure widget on the page, found by what one already is rather
/// than by a hand-kept list, so a new overlay inherits its coverage.
const DISCLOSURE: &str = "[aria-expanded][aria-controls]";

/// The image lightbox, named specifically.
///
/// The nav drawer is also a `role="dialog"` and stays mounted so its slide has
/// something to animate, so a bare `[role='dialog']` matches it too — and a
/// removal wait on that selector can never be satisfied.
const LIGHTBOX: &str = "[role='dialog'][aria-label='Image preview']";

/// Open the nav drawer and assert its links are present.
///
/// The links live only in the drawer now, at every viewport, so reaching them
/// means opening it first — there is no always-visible copy to read.
async fn check_nav_links(t: &WebTest) -> TestResult {
    t.click("button[aria-label='Toggle menu']").await?;

    // The drawer stays mounted so its slide can animate, so its links read fine
    // through `translateX(-100%)` and every assertion below would pass with a
    // toggle that does nothing at all. Reachability is what distinguishes an
    // open drawer from a closed one: a link that is off-screen or `inert` is not
    // the topmost thing at its own centre.
    check(
        t.attr("button[aria-label='Toggle menu']", "aria-expanded")
            .await?
            .as_deref()
            == Some("true"),
        "the toggle should report itself expanded after being clicked",
    )?;
    check(
        t.is_hittable("nav a[href='/about']").await?,
        "the drawer's links should be reachable once it is open, not merely present in the DOM",
    )?;

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
    )?;

    // Escape closes the drawer, so the assertions leave the page as they found
    // it and a caller can keep testing the map underneath.
    t.press_key("#site-nav-drawer", "Escape").await?;
    check(
        t.attr("button[aria-label='Toggle menu']", "aria-expanded")
            .await?
            .as_deref()
            == Some("false"),
        "Escape should close the nav drawer",
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

        // The About card is expanded on a first visit.
        t.wait_for_selector("#about-card").await?;

        // Nav drawer links
        check_nav_links(t).await?;

        // Wordmark
        t.wait_for_body_text("Chronoscope").await?;

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

/// The FAQ anchor the About page deep-links to. A native test in
/// `web/src/pages/faq.rs` ties this fragment to both pages' markdown, so a
/// rename shows up there as a clear failure before it reaches the browser.
const DEEP_LINKED_FAQ_ANCHOR: &str = "how-does-chronoscope-work";

/// Whether the deep-linked FAQ item is expanded, waiting for it to render first.
async fn deep_linked_faq_item_is_expanded(
    t: &WebTest,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let expanded = t
        .attr(
            &format!("#{DEEP_LINKED_FAQ_ANCHOR} button[aria-expanded]"),
            "aria-expanded",
        )
        .await?;
    Ok(expanded.as_deref() == Some("true"))
}

/// Following the About page's deep link expands the FAQ item it names.
///
/// An in-app navigation publishes the router's URL before it moves
/// `window.location`, so an item that reads its initial state from the browser
/// hash sees the previous page's empty fragment and renders collapsed. A
/// client-side click is the only way to reproduce that ordering.
#[tokio::test]
async fn test_faq_deep_link_followed_from_the_about_page_arrives_expanded() -> TestResult {
    web_test(async |t| {
        t.goto("/about").await?;

        t.click(&format!(
            "#main-content a[href='/faq#{DEEP_LINKED_FAQ_ANCHOR}']"
        ))
        .await?;

        check(
            deep_linked_faq_item_is_expanded(t).await?,
            "the FAQ item named by the About page's deep link should be expanded on arrival",
        )
    })
    .await
}

/// The same deep link pasted into the address bar expands the item it names.
#[tokio::test]
async fn test_faq_deep_link_loaded_directly_arrives_expanded() -> TestResult {
    web_test(async |t| {
        t.goto(&format!("/faq#{DEEP_LINKED_FAQ_ANCHOR}")).await?;

        check(
            deep_linked_faq_item_is_expanded(t).await?,
            "the FAQ item named by the loaded fragment should be expanded on arrival",
        )
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
        // "Loading..." — "Known to exist" is asserted on below, so its
        // presence proves the fetch settled and rendered.
        t.wait_for_body_text("Known to exist").await?;

        let panel_text = t.text("[role='complementary']").await?;

        // Panel should have substantial content (names, timeline, links, etc.)
        check(
            panel_text.len() > 20,
            format!("Panel should have content after clicking entity, got: {panel_text}"),
        )?;

        // Verify timeline section renders.
        check(
            panel_text.contains("Timeline") || panel_text.contains("constructed"),
            format!("Panel should show timeline section, got: {panel_text}"),
        )?;

        // Hagia Sophia's only lifecycle date is its P571 inception (year 0537),
        // read as an existence witness. The year reaches the panel on its
        // "Known to exist" row.
        check(
            panel_text.contains("537"),
            format!("Panel should show Hagia Sophia's 537 existence witness, got: {panel_text}"),
        )?;
        check(
            panel_text.contains("Known to exist"),
            format!(
                "Panel should label Hagia Sophia's P571 inception as 'Known to exist', \
                 got: {panel_text}"
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

/// Hagia Sophia carries a P571 inception (537) and no asserted construction, so
/// the read-time solver fills the empty construction slot with an inferred "built
/// by 537" bound. The panel renders that row with the sage inferred marker,
/// distinct from a citation bullet and a conflict marker; opening it names the
/// derivation, the witness reason, and the underlying Wikidata source.
#[tokio::test]
async fn test_inferred_construction_bound_renders_marker_and_names_witness() -> TestResult {
    web_test(async |t| {
        t.goto_map_at(HAGIA_SOPHIA.0, HAGIA_SOPHIA.1, 14.0).await?;
        t.click_map_at(HAGIA_SOPHIA.0, HAGIA_SOPHIA.1).await?;
        t.wait_for_selector("[role='complementary']").await?;
        t.wait_for_body_text("Known to exist").await?;

        // The inferred marker's aria-label names the derived bound, distinct from a
        // citation bullet's "N sources" and a conflict marker's "date conflict".
        let marker = "[role='complementary'] button[aria-label*='inferred']";
        t.wait_for_selector(marker).await?;
        let label = t.attr(marker, "aria-label").await?.unwrap_or_default();
        check(
            label.contains("537"),
            format!("the inferred marker names the 537 built-by bound, got: {label}"),
        )?;

        t.screenshot("test_inferred_construction_bound_marker")
            .await?;

        // Opening the marker reveals the derivation: the "built by W" heading, the
        // witness reason, and the underlying witness citation linked to its real
        // Wikidata source (not a generic "Derivation" label).
        t.click(marker).await?;
        t.wait_for_selector("[role='group']").await?;
        let popover = t.text("[role='group']").await?.to_lowercase();
        for token in [
            "inferred",
            "built by",
            "537",
            "recorded existing",
            "wikidata",
        ] {
            check(
                popover.contains(token),
                format!("the inferred popover must name '{token}', got: {popover}"),
            )?;
        }

        Ok(())
    })
    .await
}

/// Chioggia's 1633 refounding witnesses existence *after* its 1623 demolition —
/// the demolition-ceiling side of the temporal solver, complementing Notre-Dame's
/// construction-floor conflict. The item projects as one entity (the demolition
/// and refounding no longer split it), so the click opens the detail directly,
/// and the conflict rides its participating rows as an amber "!" marker.
#[tokio::test]
async fn test_chioggia_existence_after_demolition_conflict() -> TestResult {
    web_test(async |t| {
        t.goto_map_at(CHIOGGIA_CATHEDRAL.0, CHIOGGIA_CATHEDRAL.1, 14.0)
            .await?;
        // Rewind into the disputed era — after the demolition, at the 1633
        // witness — where the cathedral is exactly the contested pin this test is
        // about. (The 1633 refounding refutes the 1623 demolition, so the pin
        // draws contested at every later instant, the present day included.)
        t.set_time_slider_year(1633.0).await?;
        t.click_map_at(CHIOGGIA_CATHEDRAL.0, CHIOGGIA_CATHEDRAL.1)
            .await?;
        t.wait_for_selector("[role='complementary']").await?;

        // A single entity here, so the click lands on the detail, not a picker.
        t.wait_for_body_text("Known to exist").await?;
        let panel_text = t.text("[role='complementary']").await?;
        check(
            !panel_text.contains("Multiple entities"),
            format!("Chioggia projects one entity, not a disambiguation picker, got: {panel_text}"),
        )?;
        for token in [
            "Chioggia Cathedral",
            "Demolition completed",
            "1623",
            "Known to exist",
            "1633",
        ] {
            check(
                panel_text.contains(token),
                format!("Chioggia's detail should render '{token}', got: {panel_text}"),
            )?;
        }

        t.screenshot("test_chioggia_existence_after_demolition_conflict")
            .await?;

        // The conflict marker's aria-label names the date conflict; opening it
        // reveals the clash and a time-axis plotting the rival instants.
        let marker = "[role='complementary'] button[aria-label*='date conflict']";
        t.wait_for_selector(marker).await?;
        t.click(marker).await?;
        t.wait_for_selector("[role='group']").await?;
        let popover = t.text("[role='group']").await?.to_lowercase();
        for token in ["existed", "1633", "1623", "demolished"] {
            check(
                popover.contains(token),
                format!("the conflict popover must name '{token}', got: {popover}"),
            )?;
        }
        check(
            t.exists("[role='group'] svg").await?,
            "the conflict popover must plot its participants on a time-axis",
        )?;

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

// ==================== Client-side clustering tests ====================
//
// These seed entities at open-ocean coordinates so the map geometry is fully
// controlled. Pairs straddling the prime meridian (lon 0) always land in
// distinct server cells — the near-overlap case the client folds — while a
// lone entity always stays an individual `Select` pin.

/// The headline near-split test: two entities ~44 m apart straddling lon 0
/// render as ONE badge, and no two individual markers sit within the
/// proximity-cluster radius. Without client clustering these two distinct
/// server cells would render as two near-overlapping pins (the Hagia
/// Sophia/Bostancı artifact).
#[tokio::test]
async fn test_near_adjacent_entities_render_one_badge() -> TestResult {
    let seeds = vec![
        seed_entity_at("Meridian West", 0.02, -0.0002)?,
        seed_entity_at("Meridian East", 0.02, 0.0002)?,
    ];
    web_test_seeded(seeds, async |t| {
        // Mid zoom where 44 m projects to ~1 px — well inside the 50 px radius.
        t.goto_map_at(0.0, 0.02, 12.0).await?;

        // The two entities are distinct server cells; the client folds them into
        // exactly one badge and leaves no individual pin behind. Had clustering
        // failed they would render as two near-overlapping pins instead (badge
        // count 0, marker count 2) — the Hagia Sophia/Bostancı artifact.
        let badges = t.badge_properties().await?;
        check(
            badges.len() == 1,
            format!("two near-adjacent entities must fold to exactly one badge, got {badges:?}"),
        )?;
        let markers = t.marker_properties().await?;
        check(
            markers.is_empty(),
            format!(
                "both entities fold into the badge, leaving no individual pin, got {markers:?}"
            ),
        )?;

        // The badge lands in-view: the client clips out-of-view cells before
        // clustering, so supercluster only ever sees in-view points and the fold
        // centroid projects onto the canvas the user is looking at.
        let (w, h) = canvas_size(t).await?;
        let badge = badges.first().ok_or("expected exactly one fold badge")?;
        let badge_xy = screen_xy(badge).ok_or("badge missing screen coords")?;
        check(
            in_canvas(badge_xy, w, h),
            format!("the fold badge must project in-view within {w}x{h}, got {badge_xy:?}"),
        )?;
        Ok(())
    })
    .await
}

/// A lone entity renders as an individual `Select` pin (with its name, no
/// `point_count`, `kind == "entity"`) and never as a badge.
#[tokio::test]
async fn test_lone_entity_is_a_pin_not_a_badge() -> TestResult {
    let seeds = vec![seed_entity_at("Lone Beacon", 25.0, -40.0)?];
    web_test_seeded(seeds, async |t| {
        t.goto_map_at(-40.0, 25.0, 12.0).await?;

        let badges = t.badge_properties().await?;
        check(
            badges.is_empty(),
            format!("a lone entity must not render a badge, got {badges:?}"),
        )?;

        let markers = t.marker_properties().await?;
        check(
            markers.len() == 1,
            format!("expected exactly one individual marker, got {markers:?}"),
        )?;
        let marker = &markers[0];
        check(
            marker.get("kind").and_then(|k| k.as_str()) == Some("entity"),
            format!("a lone marker's kind must be 'entity', got {marker:?}"),
        )?;
        check(
            marker.get("name").and_then(|n| n.as_str()) == Some("Lone Beacon"),
            format!("the lone marker must carry its negotiated name, got {marker:?}"),
        )?;
        Ok(())
    })
    .await
}

/// Zooming into Rome splits its clustered landmarks into individual pins.
/// Zoomed out, the central-Rome landmarks fall within the proximity radius and
/// fold into a cluster badge; clicking the badge eases the map in past the fold
/// distance, and the cluster breaks apart into individual pins (or, for a deeper
/// nesting, at least sheds a badge on the way down).
///
/// Runs against the curated Rome data, so the exact fold at zoom 10 is a
/// property of that dataset rather than a seeded geometry.
#[tokio::test]
async fn test_zoom_into_rome_splits_cluster_into_individual_pins() -> TestResult {
    web_test(async |t| {
        // Zoomed out over Rome, the landmarks fold into a cluster badge.
        t.goto_map_at(ROME_LNG, ROME_LAT, 10.0).await?;
        let badges = t.badge_properties().await?;
        check(
            !badges.is_empty(),
            format!(
                "zoomed out over Rome, the landmarks must fold into a cluster badge, got {badges:?}"
            ),
        )?;
        let badge = badges.first().ok_or("expected a Rome cluster badge")?;
        let badge_lng = badge
            .get("_lng")
            .and_then(serde_json::Value::as_f64)
            .ok_or("badge descriptor missing _lng")?;
        let badge_lat = badge
            .get("_lat")
            .and_then(serde_json::Value::as_f64)
            .ok_or("badge descriptor missing _lat")?;

        let zoom_before = t.zoom().await?;

        // Sample the fetch counter, click the badge, then wait for the
        // expansion's ease-to-triggered re-fetch and the map to settle.
        let prev_fetch = t.current_fetch_settled().await?;
        t.click_map_at(badge_lng, badge_lat).await?;
        t.wait_for_fetch_settled_after(prev_fetch).await?;
        t.wait_for_map_idle().await?;

        let zoom_after = t.zoom().await?;
        check(
            zoom_after > zoom_before,
            format!(
                "clicking a Rome cluster badge must zoom the map in: {zoom_before} -> {zoom_after}"
            ),
        )?;

        let markers_after = t.marker_properties().await?;
        let badges_after = t.badge_properties().await?;
        check(
            !markers_after.is_empty() || badges_after.len() < badges.len(),
            format!(
                "expansion must surface individual pins or drop the badge count: \
                 badges {} -> {}, individuals {}",
                badges.len(),
                badges_after.len(),
                markers_after.len()
            ),
        )?;
        Ok(())
    })
    .await
}

/// Village + wild: a dense group folds to one badge while a distant lone entity
/// in the same viewport stays an individual pin.
#[tokio::test]
async fn test_dense_group_and_lone_entity_render_badge_plus_pin() -> TestResult {
    // Geometry is chosen so both classifications are decided server-side, off any
    // coarse Morton boundary. At zoom 11 cells fold at level 11 + CELL_DEPTH(3) =
    // 14. The three village entities (~55 m apart, open ocean, off lon 0) all fall
    // in one level-14 sub-tile → one server cluster cell → a badge, with no
    // reliance on the client merging across a tile boundary. The wild sits 0.0412°
    // east — a distinct sub-tile (its own Select cell) and ~120 px away at zoom 11,
    // well past the 50 px fold radius. The map is centered between them so each
    // lands ~60 px from center, comfortably inside the canvas.
    let seeds = vec![
        seed_entity_at("Village A", 20.0000, -30.0000)?,
        seed_entity_at("Village B", 20.0003, -30.0002)?,
        seed_entity_at("Village C", 19.9998, -30.0003)?,
        seed_entity_at("Lonely Wild", 20.0000, -29.9588)?,
    ];
    web_test_seeded(seeds, async |t| {
        t.goto_map_at(-29.9794, 20.0, 11.0).await?;

        let (w, h) = canvas_size(t).await?;

        // The dense village folds to a cluster badge that lands in-view.
        let badges = t.badge_properties().await?;
        check(
            !badges.is_empty(),
            format!("the dense village must fold to a cluster badge, got {badges:?}"),
        )?;
        let badge = badges.first().ok_or("expected a village badge")?;
        let badge_xy = screen_xy(badge).ok_or("badge missing screen coords")?;
        check(
            in_canvas(badge_xy, w, h),
            format!("the village badge must render in-view within {w}x{h}, got {badge_xy:?}"),
        )?;

        // Only the wild entity renders as an individual pin, also in-view.
        let markers = t.marker_properties().await?;
        check(
            markers.len() == 1,
            format!("only the wild entity should render as an individual pin, got {markers:?}"),
        )?;
        let wild = markers.first().ok_or("expected the wild pin")?;
        check(
            wild.get("name").and_then(|n| n.as_str()) == Some("Lonely Wild"),
            format!("the lone pin must be the wild entity, got {wild:?}"),
        )?;
        let wild_xy = screen_xy(wild).ok_or("wild pin missing screen coords")?;
        check(
            in_canvas(wild_xy, w, h),
            format!("the wild pin must render in-view within {w}x{h}, got {wild_xy:?}"),
        )?;
        Ok(())
    })
    .await
}

/// Direction-B regression (no hidden entity): panning to a populated sub-region
/// at a medium zoom always renders at least one marker whose projected position
/// is genuinely on-screen. This is the empty-viewport bug the tiled redesign
/// exists to fix — a stale coarse feed, or a rollup whose representative fell
/// off-edge, would leave a populated view blank.
#[tokio::test]
async fn test_populated_viewport_always_renders_an_in_view_marker() -> TestResult {
    // A compact cluster in open ocean, spread ~1 km so the geometry is ours
    // alone. At this zoom the members sit within the proximity radius and fold to
    // one badge; folded or not, the region is non-empty and must show it.
    let seeds = vec![
        seed_entity_at("Reef North", 25.010, -40.000)?,
        seed_entity_at("Reef East", 25.000, -40.010)?,
        seed_entity_at("Reef South", 24.990, -40.000)?,
    ];
    web_test_seeded(seeds, async |t| {
        t.goto_map_at(-40.0, 25.0, 11.0).await?;

        let features = rendered_features(t).await?;
        check(
            !features.is_empty(),
            "a populated viewport must render at least one marker or badge",
        )?;

        let (w, h) = canvas_size(t).await?;
        let any_in_view = features
            .iter()
            .filter_map(screen_xy)
            .any(|xy| in_canvas(xy, w, h));
        check(
            any_in_view,
            format!(
                "at least one rendered feature must project on-screen within {w}x{h}, got {features:?}"
            ),
        )?;
        Ok(())
    })
    .await
}

/// The direction-B guarantee holds even for a thin, edge-shaped viewport — the
/// case a coarse-rollup representative is most likely to fall outside. A short
/// horizontal strip centered on a populated region still renders an in-view
/// marker.
#[tokio::test]
async fn test_thin_viewport_still_renders_a_populated_region() -> TestResult {
    let seeds = vec![
        seed_entity_at("Strip West", 25.000, -40.010)?,
        seed_entity_at("Strip East", 25.000, -39.990)?,
    ];
    web_test_seeded(seeds, async |t| {
        // Set the thin viewport before the map mounts so it initializes at this
        // shape (mirrors the mobile-layout test's set-then-goto order).
        t.set_viewport(1280, 150).await?;
        t.goto_map_at(-40.0, 25.0, 11.0).await?;

        let features = rendered_features(t).await?;
        check(
            !features.is_empty(),
            "a populated thin viewport must still render a marker or badge",
        )?;

        let (w, h) = canvas_size(t).await?;
        let any_in_view = features
            .iter()
            .filter_map(screen_xy)
            .any(|xy| in_canvas(xy, w, h));
        check(
            any_in_view,
            format!(
                "a thin populated viewport must show an in-view feature within {w}x{h}, got {features:?}"
            ),
        )?;
        Ok(())
    })
    .await
}

/// Panning across the antimeridian (±180°) must not blank or error: entities on
/// the Pacific side of the seam still render. The seam-aware viewport bounds and
/// tile enumeration carry the wrap; a naive box would fold to an antimeridian
/// sliver and drop them.
#[tokio::test]
async fn test_antimeridian_pan_still_renders_markers() -> TestResult {
    // Two entities straddling the seam — one just west, one just east of ±180°.
    let seeds = vec![
        seed_entity_at("Dateline West", 0.0, 179.9)?,
        seed_entity_at("Dateline East", 0.0, -179.9)?,
    ];
    web_test_seeded(seeds, async |t| {
        // Center on the seam itself, so the viewport spans both sides of ±180°.
        t.goto_map_at(180.0, 0.0, 5.0).await?;

        let features = rendered_features(t).await?;
        check(
            !features.is_empty(),
            "an antimeridian-straddling viewport must still render markers, not blank",
        )?;

        let (w, h) = canvas_size(t).await?;
        let any_in_view = features
            .iter()
            .filter_map(screen_xy)
            .any(|xy| in_canvas(xy, w, h));
        check(
            any_in_view,
            format!(
                "a seam-straddling viewport must show an in-view feature within {w}x{h}, got {features:?}"
            ),
        )?;

        // A blank-or-error regression would surface the fetch-error banner.
        check(
            !t.has_text("Retry").await?,
            "crossing the antimeridian must not raise a fetch error",
        )?;
        Ok(())
    })
    .await
}

/// A failed cache-miss fetch must keep the prior area's last-good render and
/// cache, not blank the map. Load area A, break the API, pan to a different
/// populated area B whose fetch now fails, then return to A: with the last-good
/// guard the cache A latched survives B's failed pass, so A re-renders from cache
/// with no network fetch even though the API is still broken. Without the guard,
/// B's failed pass blanks the map and its retain sweep evicts A's cache, so
/// returning to A is itself a failing cache-miss and the map stays blank.
///
/// The final assertion returns to A because `marker_properties` is
/// `queryRenderedFeatures`-backed — it only reports in-viewport features, so A's
/// pin can't be observed while the camera sits on B.
#[tokio::test]
async fn test_failed_pan_keeps_last_good_render() -> TestResult {
    // Two lone entities in open ocean → each always an individual pin (never a
    // badge), 10° apart so B's tiles are a genuine cache-miss from A's viewport.
    let seeds = vec![
        seed_entity_at("Area A Beacon", 25.0, -40.0)?,
        seed_entity_at("Area B Beacon", 25.0, -30.0)?,
    ];
    web_test_seeded(seeds, async |t| {
        // Load area A; its pin renders and its tiles land in the cache.
        t.goto_map_at(-40.0, 25.0, 12.0).await?;
        t.wait_for_markers().await?;
        let before = t.marker_properties().await?;
        check(
            !before.is_empty(),
            format!("area A must render a pin before the failed fetch, got {before:?}"),
        )?;

        // Break the API, then pan to area B — a cache-miss whose fetch now fails.
        t.set_api_url("http://127.0.0.1:1").await?;
        t.pan_map_to(-30.0, 25.0, 12.0).await?;

        // The failure surfaces the error banner.
        t.wait_for_body_text("Retry").await?;

        // Return to area A. The guard kept A's cache through B's failed pass, so
        // this is a cache hit (no fetch) and A renders again despite the broken
        // API — proof the last-good render and cache survived.
        t.pan_map_to(-40.0, 25.0, 12.0).await?;
        t.wait_for_markers().await?;
        let after = t.marker_properties().await?;
        check(
            !after.is_empty(),
            format!("area A must re-render from the kept last-good cache, got {after:?}"),
        )?;
        Ok(())
    })
    .await
}

// ==================== UI Chrome & Error Recovery Tests ====================

/// Every disclosure widget holds its toggle still and keeps it reachable.
///
/// Enumerated from the markup rather than named one at a time, so a new overlay
/// is covered the moment it declares itself — `aria-expanded` + `aria-controls`
/// is what a disclosure widget already *is*, so there is nothing extra to
/// remember.
///
/// **Position.** Each of these used to render its collapsed and expanded states
/// as separate buttons, so the control jumped out from under the pointer on
/// every use: 26 px for the About card, 293 px for the time slider, and a
/// resize for the nav's wordmark. Sub-pixel exact, because the drift that
/// breaks it is small — a content-sized chip that drops a 1 px border between
/// states moves its contents by one pixel.
///
/// **Reachability.** A control can sit at a perfect rect and still be buried
/// under something painted after it, which geometry alone cannot see. That is
/// how the map's "?" chip ended up under the mobile bar (tapping it toggled the
/// menu, so a dismissed About card could never be restored on a phone) and how
/// the nav trigger ended up under the drawer it opens.
#[tokio::test]
async fn test_disclosure_toggles_hold_position_and_stay_reachable() -> TestResult {
    web_test(async |t| {
        t.goto("/").await?;
        t.wait_for_selector(DISCLOSURE).await?;
        // Geometry is only meaningful once the webfonts have stopped
        // reflowing the page around them.
        t.wait_for_fonts().await?;

        let ids: Vec<String> = t
            .attributes(DISCLOSURE, "aria-controls")
            .await?
            .into_iter()
            .flatten()
            .collect();
        check(
            !ids.is_empty(),
            "no disclosure widgets found to check — the enumeration, not the page, is probably wrong",
        )?;

        for id in ids {
            let toggle = format!("[aria-controls='{id}']");

            let before = t.element_rect(&toggle).await?;
            check(
                !before.is_empty(),
                format!("{id}: toggle should exist before toggling"),
            )?;
            check(
                t.is_hittable(&toggle).await?,
                format!("{id}: toggle is covered by something else in its initial state"),
            )?;

            // Each widget is opened and closed again within its own iteration,
            // so the page is back at its baseline before the next one — an open
            // nav drawer, for instance, scrims every other overlay on the page.
            t.click(&toggle).await?;
            let toggled = t.element_rect(&toggle).await?;
            check(
                toggled == before,
                format!("{id}: toggle moved when toggled: {before:?} -> {toggled:?}"),
            )?;
            check(
                t.is_hittable(&toggle).await?,
                format!("{id}: toggle is covered by something else once toggled"),
            )?;

            t.click(&toggle).await?;
            let restored = t.element_rect(&toggle).await?;
            check(
                restored == before,
                format!("{id}: toggle moved on the way back: {before:?} -> {restored:?}"),
            )?;
        }

        Ok(())
    })
    .await
}

/// The time slider is one bar, not a chip sitting on a taller card.
///
/// Its chip *is* the bar's left end, so the two share a height — and a border
/// gets counted two different ways across that seam: an element with an
/// explicit height keeps its border inside that height under
/// `box-sizing: border-box`, while one sized by its content has the border
/// added on top. Same 1 px, and the bar grew 2 px taller than the chip it
/// expanded from.
///
/// Not part of the battery above because it doesn't generalise: the About
/// card's chip is a small circle on a tall panel and shares no dimension with
/// it.
#[tokio::test]
async fn test_time_slider_bar_matches_its_chip_height() -> TestResult {
    web_test(async |t| {
        t.goto("/").await?;
        let toggle = "[aria-controls='time-slider-panel']";
        t.wait_for_selector(toggle).await?;
        // Geometry is only meaningful once the webfonts have stopped
        // reflowing the page around them.
        t.wait_for_fonts().await?;

        let expanded_bar = t.element_rect("#time-slider-panel").await?;
        let chip = t.element_rect(toggle).await?;
        check(
            expanded_bar.len() == 4 && chip.len() == 4,
            "expected rects for both the slider bar and its chip",
        )?;
        check(
            expanded_bar[3] == chip[3],
            format!(
                "the expanded bar and its chip must be the same height, got bar {} vs chip {}",
                expanded_bar[3], chip[3]
            ),
        )?;

        Ok(())
    })
    .await
}

/// An overlay card's surface is opaque and its geometry constant *throughout*
/// opening, not merely once it has settled.
///
/// Both ways this broke had correct endpoints and a wrong frame in between, so
/// a before/after assertion saw nothing. The chip hands its background to the
/// card the instant it expands: a card fading up from transparent leaves the
/// chip's own area unpainted, and the pill visibly blinks out before fading
/// back. A card that scales instead flattens — these are stadiums, so a scale
/// shrinks the height and the cap radius with it.
///
/// Sampled by seeking the animation rather than sleeping, so each reading is
/// exact rather than whatever had rendered by the time the test looked.
#[tokio::test]
async fn test_overlay_card_surface_holds_through_the_opening_animation() -> TestResult {
    web_test(async |t| {
        t.goto("/").await?;
        let toggle = "[aria-controls='time-slider-panel']";
        t.wait_for_selector(toggle).await?;
        // Geometry is only meaningful once the webfonts have stopped
        // reflowing the page around them.
        t.wait_for_fonts().await?;

        let settled = t.element_rect("#time-slider-panel").await?;
        check(settled.len() == 4, "expected a rect for the expanded bar")?;

        // Stretch the animation so it is still running when the samples land;
        // the seek below is what makes each sample exact.
        t.slow_animations(4000.0).await?;
        t.click(toggle).await?; // collapse
        t.click(toggle).await?; // and open again, now in slow motion

        for progress in [0.0, 0.25, 0.5, 0.75] {
            let sample = t.sample_animation_at("#time-slider-panel", progress).await?;
            check(
                sample.len() == 6,
                format!("expected a sample at progress {progress}"),
            )?;
            let (animations, opacity, height) = (sample[0], sample[1], sample[5]);

            // Without this the test would read a settled card and pass no
            // matter what the animation did.
            check(
                animations > 0.0,
                format!("no live animation to sample at progress {progress}"),
            )?;
            check(
                opacity == 1.0,
                format!("the card's surface was {opacity} opaque at progress {progress}; it must never fade, or the chip's area goes unpainted"),
            )?;
            check(
                height == settled[3],
                format!(
                    "the card was {height} tall at progress {progress} but {} when settled; a transform on a stadium distorts its ends",
                    settled[3]
                ),
            )?;
        }

        Ok(())
    })
    .await
}

/// The About card's dismissal sticks across a reload, and the chip brings it
/// back.
///
/// Disclosure state is read from `#about-card`, the body the toggle's
/// `aria-controls` names, rather than from a phrase in the copy. The card's
/// wording is expected to keep changing; whether it is open is the behaviour
/// under test.
#[tokio::test]
async fn test_info_card_dismiss_restore() -> TestResult {
    web_test(async |t| {
        t.goto("/").await?;
        t.wait_for_selector("#about-card").await?;

        // The card's toggle is one button that stays put; only its label and
        // glyph change with state.
        t.click("button[aria-label='Hide the About panel']").await?;
        t.wait_for_selector_removal("#about-card").await?;

        // The "?" restore button should appear (aria-label="About Chronoscope")
        check(
            t.exists("button[aria-label='About Chronoscope']").await?,
            "Restore '?' button should appear after dismissing info card",
        )?;

        t.screenshot("test_info_card_dismissed").await?;

        // Reload and check persistence
        t.goto("/").await?;
        check(
            !t.exists("#about-card").await?,
            "Info card should remain dismissed after reload",
        )?;

        // Click restore button via WASM test hook (Leptos event handlers may
        // not fire via CDP's native click)
        t.click("button[aria-label='About Chronoscope']").await?;
        t.wait_for_selector("#about-card").await?;

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
    // Error→retry→recovery driven by the client's own tile fetches:
    // 1. Load page, verify entities appear (API works)
    // 2. Swap API URL to a bogus value, then force a cache-MISS fetch → it fails
    // 3. Error state with retry button
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

        // Step 2: Break the API by pointing at a bogus URL, then force a genuine
        // cache-MISS fetch so the failure actually fires. The tiled client only
        // fetches tiles it hasn't already cached, so nudging within the current
        // view would hit cache and never touch the network. Pan to a different
        // populated region at a moderate zoom — central Rome, zoom 13 — a fresh
        // location and level whose tiles must be fetched, and that fetch fails.
        // Zoom 13 keeps the landmarks past the fold radius, so recovery lands
        // individual pins the count can see (a deeper zoom risks clipping them
        // all out of the narrow canvas).
        t.set_api_url("http://127.0.0.1:1").await?;

        // `pan_map_to` works for failing fetches too — the fetch-settled counter
        // advances on both success and failure paths.
        t.pan_map_to(ROME_LNG, ROME_LAT, 13.0).await?;
        t.wait_for_body_text("Retry").await?;

        t.screenshot("test_retry_step2_error").await?;

        // Step 3: Restore the API URL — to the same-origin `/api` front door the
        // page's client uses, not the API's direct address (a cross-origin
        // restore fails now that the API serves no CORS).
        let real_url = t.same_origin_api_url();
        t.set_api_url(&real_url).await?;

        // Step 4: Click retry and wait for the fetch to complete.
        t.click_and_wait_for_fetch("button[aria-label*=\"Retry\"]")
            .await?;

        // The retry re-render is a data-only setData with no camera move, so the
        // fetch-settled counter bumps before MapLibre paints; a bare marker_count
        // read would race the paint and see 0. Wait for the reloaded markers to
        // actually render, then count them.
        t.wait_for_markers().await?;
        let count = t.marker_properties().await?.len();
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

        // The floating nav trigger replaces the old fixed top bar.
        let has_toggle = t.exists("button[aria-label='Toggle menu']").await?;
        check(has_toggle, "Nav trigger should be visible")?;

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

        // The drawer's links, reached the same way at every viewport.
        check_nav_links(t).await?;

        // Wordmark
        t.wait_for_body_text("Chronoscope").await?;

        // The trigger is the nav at every width now — there is no permanent
        // sidebar for desktop to fall back to, so it must be visible here.
        check(
            t.is_visible("button[aria-label='Toggle menu']").await?,
            "Nav trigger should be visible on desktop",
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
// These tests rely on the dev server having resolved every fact-store image
// to a placeholder in the media store during WebTest setup.

/// Find an entity that has resolved media via the typed API client, returning its (lng, lat).
///
/// The tile endpoint is clustered and bare (no thumbnails), so this enumerates
/// the singleton markers over the container tiles covering central Rome and
/// probes each entity's images sub-resource, returning the point of the one with
/// the most resolved media.
async fn find_entity_with_media(
    t: &WebTest,
) -> Result<(f64, f64), Box<dyn std::error::Error + Send + Sync>> {
    use chronoscope_api_client::Client;
    use chronoscope_core::geo::{mercator_x, mercator_y};

    let client = Client::new(t.api_base_url());

    // A tight box over central Rome (the 4 Roman entities, several with seeded
    // media). At container level 14 each well-separated entity folds into its
    // own singleton sub-tile cell, and the box spans only a handful of tiles.
    const LEVEL: u8 = 14;
    let (min_lat, max_lat, min_lon, max_lon) = (41.87, 41.92, 12.44, 12.51);
    let n = f64::from(1u32 << LEVEL);
    let tile_x = |lon: f64| (mercator_x(lon) * n).floor() as u32;
    let tile_y = |lat: f64| (mercator_y(lat) * n).floor() as u32;
    // Mercator y grows southward, so max_lat is the smaller (northern) row.
    let (x_lo, x_hi) = (tile_x(min_lon), tile_x(max_lon));
    let (y_lo, y_hi) = (tile_y(max_lat), tile_y(min_lat));

    let images_limit = std::num::NonZeroU32::new(50).ok_or("nonzero image page size")?;

    // Probe each singleton entity's images and keep the one with the most
    // resolved media. Only a singleton resolves to a clickable detail — a
    // cluster's click zooms — so cluster/co-located markers are skipped.
    let mut best: Option<(f64, f64, usize, String)> = None;
    for x in x_lo..=x_hi {
        for y in y_lo..=y_hi {
            let response = client.fetch_tile(LEVEL, x, y, None, None).await?;
            for marker in &response.markers {
                let chronoscope_api_client::ClickAction::Select { entity_id } =
                    &marker.click_action
                else {
                    continue;
                };
                let page = client
                    .get_entity_images(entity_id, images_limit, None, None)
                    .await?;
                let count = page.images.len();
                if count > 0 && best.as_ref().is_none_or(|b| count > b.2) {
                    best = Some((
                        marker.point.lon(),
                        marker.point.lat(),
                        count,
                        entity_id.to_string(),
                    ));
                }
            }
        }
    }

    let (lng, lat, count, id) =
        best.ok_or("No entity with resolved media found — did placeholder image resolution run?")?;
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
        t.wait_for_selector(LIGHTBOX).await?;

        t.press_key(LIGHTBOX, "Escape").await?;
        // Wait for the dialog to disappear (reactive update after signal change).
        t.wait_for_selector_removal(LIGHTBOX).await?;

        check(
            !t.exists(LIGHTBOX).await?,
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
        t.wait_for_selector(LIGHTBOX).await?;

        t.click("[role='dialog'] button[aria-label='Close preview']")
            .await?;
        // Wait for the dialog to disappear (reactive update after signal change).
        t.wait_for_selector_removal(LIGHTBOX).await?;

        check(
            !t.exists(LIGHTBOX).await?,
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

// ==================== Entity Listing Tests ====================
//
// These tests rely on the curated wikidata bundle (see nix/wikidata.nix), which
// includes 13 Italian entities spread across 8 regions plus the existing
// Chioggia Cathedral. The expected counts and names match the data produced
// by the Wikidata ingester. Text-only slice: no region clustering — every
// marker is an individual (or co-located-disambiguation) entity marker.

/// Rome — for testing zoomed-in views with 4 distinct entities.
const ROME_LNG: f64 = 12.48;
const ROME_LAT: f64 = 41.9;

/// Get markers of a given kind. Clustering is gone, so every marker's
/// `kind` is `"entity"` — kept as a filter (rather than dropped) so this
/// test's assertions about absent clusters stay meaningful if clustering
/// ever returns.
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

/// Notre-Dame de Paris (lng, lat).
const NOTRE_DAME: (f64, f64) = (2.349902, 48.852968);

/// Notre-Dame de Paris is constructed over 1163–1345 with a mid-life usage
/// change dated 1186. In moment order that interior event interleaves *between*
/// the two construction endpoints. This drives the real detail panel and pins
/// that order end-to-end — the render-layer complement of `core::moment`'s unit
/// coverage.
#[tokio::test]
async fn test_notre_dame_interior_event_renders_between_construction_endpoints() -> TestResult {
    web_test(async |t| {
        t.goto_map_at(NOTRE_DAME.0, NOTRE_DAME.1, 14.0).await?;

        // A single marker sits at these coords, so clicking opens the detail
        // panel directly — no disambiguation picker.
        t.click_map_at(NOTRE_DAME.0, NOTRE_DAME.1).await?;
        t.wait_for_selector("[role='complementary']").await?;

        // The detail fetch is async; wait for a loaded-state token (asserted on
        // below) before sampling the rendered order.
        t.wait_for_body_text("Construction completed").await?;

        let panel_text = t.text("[role='complementary']").await?;

        let construction_started = panel_text
            .find("Construction started")
            .ok_or("Notre-Dame panel should render a 'Construction started' row")?;
        let usage_changed = panel_text
            .find("Usage changed")
            .ok_or("Notre-Dame panel should render its mid-life 'Usage changed' row")?;
        let construction_completed = panel_text
            .find("Construction completed")
            .ok_or("Notre-Dame panel should render a 'Construction completed' row")?;

        // The mid-life usage change must fall between the construction
        // endpoints — after the start, before the completion — in rendered order.
        check(
            construction_started < usage_changed && usage_changed < construction_completed,
            format!(
                "Notre-Dame's mid-life 'Usage changed' row must render between \
                 'Construction started' and 'Construction completed'; got offsets \
                 started={construction_started}, usage={usage_changed}, \
                 completed={construction_completed} in panel: {panel_text}"
            ),
        )?;

        Ok(())
    })
    .await
}

/// Notre-Dame is attested existing (P571, 1160) before its construction started
/// (P793, 1163) — a contradiction spanning two fields with no per-slot home, so it
/// surfaces as an entity-level temporal conflict. The panel rides it inline on the
/// participating rows as an amber "!" marker; opening it reveals the plain-language
/// clash and a time-axis plotting the rival instants.
#[tokio::test]
async fn test_notre_dame_existence_before_construction_conflict() -> TestResult {
    web_test(async |t| {
        open_notre_dame_panel(t).await?;

        // The conflict marker's aria-label names the date conflict, distinguishing
        // it from a citation bullet's "conflicting sources".
        let marker = "[role='complementary'] button[aria-label*='date conflict']";
        t.wait_for_selector(marker).await?;
        t.click(marker).await?;

        // The popover portals to document.body with role='group'; wait for it,
        // then read the clash and confirm the time-axis rendered.
        t.wait_for_selector("[role='group']").await?;
        let popover = t.text("[role='group']").await?.to_lowercase();
        for token in ["existed", "1160", "1163", "construction"] {
            check(
                popover.contains(token),
                format!("the conflict popover must name '{token}', got: {popover}"),
            )?;
        }
        check(
            t.exists("[role='group'] svg").await?,
            "the conflict popover must plot its participants on a time-axis",
        )?;

        Ok(())
    })
    .await
}

/// Navigate to Notre-Dame de Paris, open its detail panel, and wait for the
/// timeline to render. A single marker sits at these coords, so the click opens
/// the panel directly with no disambiguation picker. Shared setup for the
/// citation-popover tests.
async fn open_notre_dame_panel(t: &WebTest) -> TestResult {
    t.goto_map_at(NOTRE_DAME.0, NOTRE_DAME.1, 14.0).await?;
    t.click_map_at(NOTRE_DAME.0, NOTRE_DAME.1).await?;
    t.wait_for_selector("[role='complementary']").await?;
    t.wait_for_body_text("Construction started").await
}

/// A settled field's bullet opens the plain (non-disputed) popover: a
/// "Source(s)" heading and the field's Wikidata source label, with no disputed
/// treatment. Guards the common source-listing path and the conditional
/// `<Portal>` mount behind it.
#[tokio::test]
async fn test_settled_citation_popover_shows_its_source() -> TestResult {
    web_test(async |t| {
        open_notre_dame_panel(t).await?;

        // A settled bullet reads "1 source" / "N sources"; the disputed one reads
        // "N conflicting sources", so exclude it.
        t.click(
            "[role='complementary'] button[aria-label*='source']:not([aria-label*='conflicting'])",
        )
        .await?;

        let popover = t.text("[role='group']").await?.to_lowercase();
        check(
            popover.contains("source"),
            format!("A settled popover must carry a Source(s) heading, got: {popover}"),
        )?;
        check(
            popover.contains("wikidata"),
            format!("Notre-Dame's settled fields are Wikidata-sourced, got: {popover}"),
        )?;
        check(
            !popover.contains("disputed"),
            format!("A settled popover reads as a source, not a dispute, got: {popover}"),
        )?;

        Ok(())
    })
    .await
}

/// The popover dismisses on both Escape and an outside click, and its portaled
/// subtree leaves the DOM each time (the `<Show>` unmount). Guards the dismissal
/// wiring and the reactive-disposal safety behind the conditional render.
#[tokio::test]
async fn test_citation_popover_dismisses_on_escape_and_outside_click() -> TestResult {
    web_test(async |t| {
        open_notre_dame_panel(t).await?;

        let bullet =
            "[role='complementary'] button[aria-label*='source']:not([aria-label*='conflicting'])";

        // Escape closes it. The keydown listener lives on the window, and
        // press_key dispatches a bubbling event, so targeting the popover reaches
        // it.
        t.click(bullet).await?;
        let popover = t.text("[role='group']").await?;
        check(
            !popover.is_empty(),
            "opening a bullet must mount a non-empty popover",
        )?;
        t.press_key("[role='group']", "Escape").await?;
        t.wait_for_selector_removal("[role='group']").await?;

        // An outside click closes it too. The panel's title heading is a neutral,
        // non-interactive target that neither navigates nor opens another popover.
        t.click(bullet).await?;
        t.wait_for_selector("[role='group']").await?;
        t.click("[role='complementary'] h2").await?;
        t.wait_for_selector_removal("[role='group']").await?;

        Ok(())
    })
    .await
}

/// A settled Wikidata-statement citation deep-links to the exact pinned revision
/// and property that sourced its date, opening in a new tab. Guards
/// `citation_url`'s oldid + property-anchor construction and the `target=_blank`
/// behavior.
#[tokio::test]
async fn test_settled_citation_source_links_to_wikidata_revision() -> TestResult {
    web_test(async |t| {
        open_notre_dame_panel(t).await?;

        // A timeline-row date bullet cites a Wikidata *statement* (its P-property),
        // so its link carries the property anchor; scoping to the timeline list
        // skips the name bullet, whose label source has none.
        t.click(
            "[role='complementary'] li button[aria-label*='source']:not([aria-label*='conflicting'])",
        )
        .await?;

        let href = t
            .attr("[role='group'] a", "href")
            .await?
            .ok_or("the settled source must link to its Wikidata revision")?;
        check(
            href.contains("wikidata.org/wiki/Q2981")
                && href.contains("oldid=")
                && href.contains("#P"),
            format!(
                "the settled statement link must deep-link to the pinned Wikidata \
                 revision and property, got: {href}"
            ),
        )?;

        let target = t
            .attr("[role='group'] a", "target")
            .await?
            .ok_or("the settled source link must declare a target")?;
        check(
            target == "_blank",
            format!("the source link must open in a new tab, got: {target}"),
        )?;

        Ok(())
    })
    .await
}
