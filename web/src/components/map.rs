use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use leptos::prelude::*;
use send_wrapper::SendWrapper;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::api;
use crate::maplibre;

// ==================== Constants ====================

/// `OpenFreeMap` Liberty style — a free, open-source map style.
/// <https://openfreemap.org/>
const MAP_STYLE_URL: &str = "https://tiles.openfreemap.org/styles/liberty";

/// Initial map center: Rome, Italy (roughly central in the Mediterranean).
const INITIAL_CENTER_LNG: f64 = 12.4964;
const INITIAL_CENTER_LAT: f64 = 41.9028;
const INITIAL_ZOOM: f64 = 3.0;

/// Debounce delay (ms) for map pan/zoom → entity re-fetch.
const MOVEEND_DEBOUNCE_MS: i32 = 150;

/// Name of the GeoJSON source added to the map.
const ENTITY_SOURCE_ID: &str = "entities";

/// Name of the circle layer for entity markers.
pub(crate) const ENTITY_CIRCLES_LAYER: &str = "entity-circles";

/// DOM event name signaling map mount completion (used by test hooks).
#[cfg(feature = "test-hooks")]
pub(crate) const MAP_READY_EVENT: &str = "chronoscope-map-ready";
/// DOM event name signaling entity fetch completion (used by test hooks).
#[cfg(feature = "test-hooks")]
pub(crate) const FETCH_COMPLETE_EVENT: &str = "chronoscope-fetch-complete";

#[cfg(feature = "test-hooks")]
fn dispatch_window_event(name: &str) {
    if let Some(window) = web_sys::window()
        && let Ok(event) = web_sys::Event::new(name)
    {
        let _ = window.dispatch_event(&event);
    }
}

/// Name of the symbol layer for thumbnail markers.
pub(crate) const ENTITY_THUMBNAILS_LAYER: &str = "entity-thumbnails";

/// DOM event name signaling thumbnail image loading completion (used by test hooks).
#[cfg(feature = "test-hooks")]
pub(crate) const THUMBNAILS_LOADED_EVENT: &str = "chronoscope-thumbnails-loaded";

/// Maximum number of thumbnail images to request per viewport.
const MAX_THUMBNAILS: usize = 30;

/// Size (CSS px) of circular thumbnail images on the map.
// TODO: Revisit for mobile — 96px may be too large on small screens.
// Consider scaling down to ~64px based on viewport width.
const THUMBNAIL_SIZE: u32 = 96;

// ==================== Public types ====================

/// What the user has selected on the map.
#[derive(Clone, Debug, PartialEq)]
pub enum EntitySelection {
    /// A single entity — show detail panel directly.
    /// The optional second field carries the previous `Multiple` entries
    /// so the user can navigate back to the disambiguation list.
    Single(String, Option<Vec<EntityPickerEntry>>),
    /// Multiple co-located entities — show disambiguation list.
    Multiple(Vec<EntityPickerEntry>),
}

/// Entry in the co-located entity picker.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EntityPickerEntry {
    pub id: String,
    pub name: String,
    pub entity_type: String,
}

/// Signal carrying the current map selection.
#[derive(Clone)]
pub struct SelectedEntity(
    pub ReadSignal<Option<EntitySelection>>,
    pub WriteSignal<Option<EntitySelection>>,
);

/// Map loading/status signals provided via context.
#[derive(Clone)]
pub struct MapStatus {
    /// True while a viewport entity fetch is in flight.
    pub loading: ReadSignal<bool>,
    /// True if the last fetch returned 500 entities (the cap).
    pub truncated: ReadSignal<bool>,
    /// True briefly after a fetch returns zero results.
    pub empty: ReadSignal<bool>,
    /// Error message from the last failed fetch, if any.
    pub fetch_error: ReadSignal<Option<String>>,
    /// Set to true to trigger a retry of the last failed fetch.
    pub retry: WriteSignal<bool>,
}

// ==================== GeoJSON construction ====================

/// Group entities by exact coordinate.
///
/// Uses `f64::to_bits()` as the hash key. This is correct because these
/// coordinates come directly from the database (stored as IEEE 754 doubles)
/// with no arithmetic transformations — two entities at the "same place"
/// will have bit-identical float values.
fn coord_key(lat: f64, lon: f64) -> (u64, u64) {
    (lat.to_bits(), lon.to_bits())
}

/// Build a GeoJSON `FeatureCollection` from entity data, grouping co-located entities.
fn build_geojson(
    entities: &[api::EntitySummary],
    selected_id: Option<&str>,
    thumbnails: &ThumbnailMap,
) -> Option<JsValue> {
    use geojson::{Feature, FeatureCollection, Geometry, Value};

    let mut groups: HashMap<(u64, u64), Vec<&api::EntitySummary>> = HashMap::new();
    for e in entities {
        groups
            .entry(coord_key(e.latitude, e.longitude))
            .or_default()
            .push(e);
    }

    let mut features = Vec::new();
    for group in groups.values() {
        let first = group[0];
        let is_selected = group.iter().any(|e| selected_id == Some(e.id.as_str()));

        let geometry = Geometry::new(Value::Point(vec![first.longitude, first.latitude]));

        // Sort co-located entities by earliest_date (undated last) so the
        // disambiguation picker is in temporal order, not the API's
        // updated_at default.
        let mut sorted_group: Vec<&api::EntitySummary> = group.clone();
        sorted_group.sort_by_key(|e| (e.earliest_date.is_none(), e.earliest_date));

        let entries: Vec<EntityPickerEntry> = sorted_group
            .iter()
            .map(|e| EntityPickerEntry {
                id: e.id.to_string(),
                name: e.name.clone().unwrap_or_else(|| "Unknown".to_string()),
                entity_type: e.entity_type.to_string(),
            })
            .collect();

        let mut props = serde_json::Map::new();
        props.insert("count".into(), entries.len().into());
        props.insert("selected".into(), is_selected.into());
        props.insert(
            "name".into(),
            entries
                .first()
                .map(|e| e.name.as_str())
                .unwrap_or("Unknown")
                .into(),
        );

        // For single entities, set "id" so click handler can select directly.
        if entries.len() == 1 {
            props.insert("id".into(), entries[0].id.clone().into());
            props.insert("entity_type".into(), entries[0].entity_type.clone().into());
        }
        // Attach a thumbnail if any entity at this location has one loaded.
        if let Some(image_name) = entries.iter().find_map(|e| thumbnails.get(&e.id)) {
            props.insert("thumbnail".into(), image_name.clone().into());
        }
        // Always set "group" — the click handler uses it for multi-entity pickers,
        // and having it on single entities is harmless.
        match serde_json::to_string(&entries) {
            Ok(json) => {
                props.insert("group".into(), json.into());
            }
            Err(e) => {
                web_sys::console::warn_1(&format!("Failed to serialize entity group: {e}").into());
            }
        }

        features.push(Feature {
            geometry: Some(geometry),
            properties: Some(props),
            ..Feature::default()
        });
    }

    let collection = FeatureCollection {
        features,
        bbox: None,
        foreign_members: None,
    };

    let serializer = serde_wasm_bindgen::Serializer::json_compatible();
    collection.serialize(&serializer).ok()
}

// ==================== MapLibre source/layer setup ====================

use serde::Serialize;
use serde_json::json;

/// Typed GeoJSON source specification for `map.addSource`.
#[derive(Serialize)]
struct GeoJsonSourceSpec {
    r#type: &'static str,
    data: EmptyFeatureCollection,
}

/// An empty GeoJSON `FeatureCollection` (used as initial source data).
#[derive(Serialize)]
struct EmptyFeatureCollection {
    r#type: &'static str,
    features: [(); 0],
}

/// Initialize empty GeoJSON source and layers on the map.
///
/// Must be called synchronously in the map load callback (before any async
/// work). Use `update_source_data` to populate with real data.
fn init_source_and_layers(map: &maplibre::Map) {
    let source = GeoJsonSourceSpec {
        r#type: "geojson",
        data: EmptyFeatureCollection {
            r#type: "FeatureCollection",
            features: [],
        },
    };
    let serializer = serde_wasm_bindgen::Serializer::json_compatible();
    let Ok(source_js) = source.serialize(&serializer) else {
        web_sys::console::error_1(&"Failed to serialize GeoJSON source spec".into());
        return;
    };
    if let Err(e) = map.add_source(ENTITY_SOURCE_ID, &source_js) {
        web_sys::console::error_1(&format!("Failed to add GeoJSON source: {e:?}").into());
        return;
    }

    let get_count = json!(["get", "count"]);
    let get_selected = json!(["get", "selected"]);

    // Circle layer for the marker dots (hidden when a thumbnail is available)
    let circle = json!({
        "id": ENTITY_CIRCLES_LAYER,
        "type": "circle",
        "source": ENTITY_SOURCE_ID,
        "filter": ["!", ["has", "thumbnail"]],
        "paint": {
            "circle-radius": ["interpolate", ["linear"], get_count, 1, 7, 5, 12, 20, 18],
            "circle-color": ["case", [">", get_count, 1], "#6B4226", "#8B5E3C"],
            "circle-stroke-width": ["case", get_selected, 4, 2],
            "circle-stroke-color": ["case", get_selected, "#FFFFFF", "#F5F0E8"],
            "circle-opacity": 0.85
        }
    });
    let Ok(circle_js) = circle.serialize(&serializer) else {
        web_sys::console::error_1(&"Failed to serialize circle layer spec".into());
        return;
    };
    if let Err(e) = map.add_layer(&circle_js) {
        web_sys::console::error_1(&format!("Failed to add circle layer: {e:?}").into());
        return;
    }

    // Symbol layer for thumbnail images (below count labels, above circles)
    let thumbnails = json!({
        "id": ENTITY_THUMBNAILS_LAYER,
        "type": "symbol",
        "source": ENTITY_SOURCE_ID,
        "filter": ["has", "thumbnail"],
        "layout": {
            "icon-image": ["get", "thumbnail"],
            "icon-size": 1.0,
            "icon-allow-overlap": true,
            "icon-ignore-placement": true,
            "icon-anchor": "bottom"
        },
        "paint": {
            "icon-opacity": 0.95
        }
    });
    let Ok(thumbnails_js) = thumbnails.serialize(&serializer) else {
        web_sys::console::error_1(&"Failed to serialize thumbnails layer spec".into());
        return;
    };
    if let Err(e) = map.add_layer(&thumbnails_js) {
        web_sys::console::error_1(&format!("Failed to add thumbnails layer: {e:?}").into());
    }

    // Symbol layer for count labels on co-located entities (rendered on top)
    let labels = json!({
        "id": "entity-labels",
        "type": "symbol",
        "source": ENTITY_SOURCE_ID,
        "filter": [">", ["get", "count"], 1],
        "layout": {
            "text-field": ["to-string", get_count],
            "text-font": ["Noto Sans Bold"],
            "text-size": 11,
            "text-allow-overlap": true,
            "text-ignore-placement": true
        },
        "paint": {
            "text-color": "#F5F0E8",
            "text-halo-color": "rgba(0,0,0,0.3)",
            "text-halo-width": 1
        }
    });
    let Ok(labels_js) = labels.serialize(&serializer) else {
        web_sys::console::error_1(&"Failed to serialize labels layer spec".into());
        return;
    };
    if let Err(e) = map.add_layer(&labels_js) {
        web_sys::console::error_1(&format!("Failed to add labels layer: {e:?}").into());
    }
}

/// Update the existing GeoJSON source with new data.
fn update_source_data(map: &maplibre::Map, geojson: &JsValue) {
    match map.get_source(ENTITY_SOURCE_ID) {
        Some(source) => source.set_data(geojson),
        None => {
            web_sys::console::warn_1(
                &format!("GeoJSON source '{ENTITY_SOURCE_ID}' not found on map — was init_source_and_layers called?").into(),
            );
        }
    }
}

// ==================== Entity fetching ====================

/// Track whether the GeoJSON source has been added to the map.
type SourceInitialized = Rc<Cell<bool>>;

/// Signals needed by viewport entity fetching and GeoJSON rebuilds.
///
/// All fields are `Copy` (Leptos signals), so this struct is `Copy` too —
/// it can be captured by closures without cloning.
/// Map from entity ID → MapLibre image name for loaded thumbnails.
type ThumbnailMap = HashMap<String, String>;

#[derive(Clone, Copy)]
struct ViewportSignals {
    set_loading: WriteSignal<bool>,
    set_truncated: WriteSignal<bool>,
    set_empty: WriteSignal<bool>,
    set_fetch_error: WriteSignal<Option<String>>,
    selected: ReadSignal<Option<EntitySelection>>,
    /// Fetched entities for the current viewport. Written by
    /// `load_entities_for_viewport`, read by the GeoJSON rebuild effect.
    cached_entities: ReadSignal<Vec<api::EntitySummary>>,
    set_cached_entities: WriteSignal<Vec<api::EntitySummary>>,
    /// Map of entity ID → MapLibre image name for loaded thumbnails.
    thumbnails: ReadSignal<ThumbnailMap>,
    set_thumbnails: WriteSignal<ThumbnailMap>,
}

async fn load_entities_for_viewport(
    map: &maplibre::Map,
    client: &api::Client,
    generation: u64,
    generation_counter: &Rc<Cell<u64>>,
    signals: ViewportSignals,
) {
    let (min_lon, min_lat, max_lon, max_lat) = maplibre::get_viewport_bounds(map);

    signals.set_loading.set(true);
    signals.set_fetch_error.set(None);

    let bbox = match chronoscope_api_client::Bbox::new(min_lat, max_lat, min_lon, max_lon) {
        Ok(b) => b,
        Err(e) => {
            signals.set_loading.set(false);
            signals
                .set_fetch_error
                .set(Some(format!("Invalid viewport bounds: {e}")));
            return;
        }
    };

    // Fetch one extra to detect whether there are more results than we display.
    use futures_util::{StreamExt, TryStreamExt};
    let result: Result<Vec<_>, _> = client
        .list_entities_pages(&bbox, api::PAGE_SIZE)
        .take(api::MAX_ENTITIES + 1)
        .try_collect()
        .await;

    // Discard stale response: when the user pans rapidly, multiple
    // fetches fire concurrently. Without this check, a slow early
    // response could overwrite a newer one.
    if generation_counter.get() != generation {
        return;
    }

    match result {
        Ok(mut entities) => {
            let truncated = entities.len() > api::MAX_ENTITIES;
            entities.truncate(api::MAX_ENTITIES);
            signals.set_loading.set(false);
            signals.set_truncated.set(truncated);
            signals.set_fetch_error.set(None);
            signals.set_empty.set(entities.is_empty());
            // Set entities immediately so circles appear on the map,
            // then load thumbnails progressively (the clone is needed because
            // we set the signal before borrowing for thumbnail loading).
            signals.set_cached_entities.set(entities.clone());

            load_thumbnails_for_viewport(
                map.clone(),
                client,
                &entities,
                generation,
                generation_counter,
                signals,
            )
            .await;
        }
        Err(e) => {
            signals.set_loading.set(false);
            signals.set_fetch_error.set(Some(format!("{e}")));
        }
    }

    // Signal fetch completion (used by browser test hooks to avoid sleep-based waits).
    #[cfg(feature = "test-hooks")]
    dispatch_window_event(FETCH_COMPLETE_EVENT);
}

// ==================== Thumbnail loading ====================
//
// Thumbnails use MapLibre's `addImage` + symbol layer, which is the intended
// way to do custom icons — once registered, they're rendered by MapLibre's
// WebGL pipeline alongside the circle and label layers, all driven by the
// same GeoJSON source and filtered by properties.
//
// The canvas drawing below prepares the image data (circular photo + stem +
// location dot) as a single raster icon. This is analogous to a sprite sheet
// entry. An alternative would be to register just the circular photo, use
// `icon-offset` to shift it up, and let the existing circle layer render the
// location dot underneath. That would reduce canvas drawing and lean more on
// MapLibre's compositing, but requires coordinating multiple layers and
// doesn't give us the connecting stem.
//
// TODO: Consider minimizing canvas drawing by splitting the pin into
// composited MapLibre layers (offset icon + circle + line) instead of
// baking the full pin shape into one raster image. This would improve
// zoom scaling behavior and reduce per-image canvas work.

/// Radius of the small location dot at the bottom of a thumbnail pin.
/// Matches the circle layer's base radius (7px at count=1) for visual continuity.
const DOT_RADIUS: f64 = 7.0;
/// Vertical gap between the thumbnail circle and the location dot.
const STEM_LENGTH: f64 = 8.0;

/// Draw a thumbnail "pin": a circular photo on top, a short stem, and a small
/// dot at the bottom marking the actual geographic location.
///
/// The canvas is sized so the dot sits at the bottom center — use
/// `icon-anchor: "bottom"` in MapLibre so the dot aligns with the coordinate.
/// Get the device pixel ratio, defaulting to 1.0 if unavailable.
fn device_pixel_ratio() -> f64 {
    web_sys::window()
        .map(|w| w.device_pixel_ratio())
        .unwrap_or(1.0)
}

fn draw_circular_thumbnail(
    img: &web_sys::HtmlImageElement,
    dpr: f64,
) -> Option<web_sys::ImageData> {
    let document = web_sys::window()?.document()?;
    let canvas = document
        .create_element("canvas")
        .ok()?
        .dyn_into::<web_sys::HtmlCanvasElement>()
        .ok()?;
    let thumb_r = f64::from(THUMBNAIL_SIZE) / 2.0 * dpr;
    let border = 2.0 * dpr;
    let stem_len = STEM_LENGTH * dpr;
    let dot_r = DOT_RADIUS * dpr;
    let canvas_w = (f64::from(THUMBNAIL_SIZE) * dpr) as u32;
    // Extra space below dot for the drop shadow ellipse
    let canvas_h = (f64::from(THUMBNAIL_SIZE) * dpr + stem_len + dot_r * 2.0 + dot_r) as u32;
    canvas.set_width(canvas_w);
    canvas.set_height(canvas_h);

    let ctx = canvas
        .get_context("2d")
        .ok()??
        .dyn_into::<web_sys::CanvasRenderingContext2d>()
        .ok()?;

    let cx = f64::from(canvas_w) / 2.0; // horizontal center
    let thumb_cy = thumb_r; // thumbnail circle center Y

    // --- Thumbnail circle (center-cropped for non-square sources) ---
    ctx.save();
    ctx.begin_path();
    ctx.arc(cx, thumb_cy, thumb_r - border, 0.0, std::f64::consts::TAU)
        .ok()?;
    ctx.clip();

    let nw = f64::from(img.natural_width());
    let nh = f64::from(img.natural_height());
    let side = nw.min(nh);
    let sx = (nw - side) / 2.0;
    let sy = (nh - side) / 2.0;
    ctx.draw_image_with_html_image_element_and_sw_and_sh_and_dx_and_dy_and_dw_and_dh(
        img,
        sx,
        sy,
        side,
        side, // source: center square crop
        0.0,
        0.0,
        thumb_r * 2.0,
        thumb_r * 2.0, // dest: fill circle
    )
    .ok()?;
    ctx.restore();

    // Thumbnail border
    ctx.set_stroke_style_str("#F5F0E8");
    ctx.set_line_width(border);
    ctx.begin_path();
    ctx.arc(
        cx,
        thumb_cy,
        thumb_r - border / 2.0,
        0.0,
        std::f64::consts::TAU,
    )
    .ok()?;
    ctx.stroke();

    // --- Stem line ---
    let stem_top = thumb_cy + thumb_r;
    let stem_bottom = stem_top + stem_len;
    ctx.set_stroke_style_str("#8B5E3C"); // copper
    ctx.set_line_width(2.0 * dpr);
    ctx.begin_path();
    ctx.move_to(cx, stem_top);
    ctx.line_to(cx, stem_bottom);
    ctx.stroke();

    // --- Drop shadow (soft circle behind the dot, offset down) ---
    let dot_cy = stem_bottom + dot_r;
    ctx.save();
    ctx.set_shadow_color("rgba(0, 0, 0, 0.4)");
    ctx.set_shadow_blur(4.0 * dpr);
    ctx.set_shadow_offset_y(2.0 * dpr);

    // --- Location dot (matches circle marker style) ---
    ctx.set_fill_style_str("#8B5E3C"); // copper fill
    ctx.begin_path();
    ctx.arc(cx, dot_cy, dot_r, 0.0, std::f64::consts::TAU)
        .ok()?;
    ctx.fill();
    // Restore before stroke so the shadow only applies to the fill, not the border
    ctx.restore();
    ctx.set_stroke_style_str("#F5F0E8"); // parchment stroke
    ctx.set_line_width(2.0 * dpr);
    ctx.stroke();

    ctx.get_image_data(0.0, 0.0, f64::from(canvas_w), f64::from(canvas_h))
        .ok()
}

/// Load thumbnails for visible entities and register them as MapLibre images.
///
/// Fetches thumbnail URLs for a subset of entity IDs, loads images
/// incrementally (each appears on the map as soon as it loads rather than
/// waiting for the entire batch), and cleans up stale images that are no
/// longer in the viewport.
///
/// Uses `generation` to detect stale loads — if the viewport has changed since
/// this call started, the results are discarded.
async fn load_thumbnails_for_viewport(
    map: maplibre::Map,
    client: &api::Client,
    entities: &[api::EntitySummary],
    generation: u64,
    generation_counter: &Rc<Cell<u64>>,
    signals: ViewportSignals,
) {
    use chronoscope_api_client::EntityId;
    use futures_util::StreamExt;
    use futures_util::stream::FuturesUnordered;

    let ids: Vec<EntityId> = entities
        .iter()
        .take(MAX_THUMBNAILS)
        .map(|e| e.id.clone())
        .collect();

    let new_entity_ids: std::collections::HashSet<String> =
        ids.iter().map(|id| id.to_string()).collect();

    // Remove only thumbnails that are no longer in the viewport (diff, not clear-all).
    let old_thumbnails = signals.thumbnails.get_untracked();
    let mut kept: ThumbnailMap = HashMap::new();
    for (entity_id, image_name) in &old_thumbnails {
        if new_entity_ids.contains(entity_id) {
            kept.insert(entity_id.clone(), image_name.clone());
        } else if map.has_image(image_name) {
            let _ = map.remove_image(image_name);
        }
    }
    signals.set_thumbnails.set(kept);

    if ids.is_empty() {
        return;
    }

    let response = match client.get_entity_thumbnails(&ids).await {
        Ok(r) => r,
        Err(e) => {
            web_sys::console::warn_1(&format!("Failed to fetch entity thumbnails: {e}").into());
            return;
        }
    };

    if generation_counter.get() != generation {
        return;
    }

    let dpr = device_pixel_ratio();
    let opts = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&opts, &"pixelRatio".into(), &JsValue::from_f64(dpr));

    // Capture current thumbnails once (not per-filter-iteration).
    let current_thumbs = signals.thumbnails.get_untracked();

    // Load images in parallel, skipping entities that already have thumbnails.
    let mut futures: FuturesUnordered<_> = response
        .thumbnails
        .iter()
        .filter(|(eid, _)| !current_thumbs.contains_key(&eid.to_string()))
        .map(|(entity_id, thumb_info)| {
            let entity_id = entity_id.to_string();
            let url = thumb_info.url.clone();
            async move { (entity_id, load_image(&url).await) }
        })
        .collect();

    // Collect loaded thumbnails, then update the signal once (avoids
    // triggering a GeoJSON rebuild per image).
    let mut batch: ThumbnailMap = HashMap::new();

    while let Some((entity_id, img_opt)) = futures.next().await {
        if generation_counter.get() != generation {
            return;
        }

        let Some(img) = img_opt else {
            continue;
        };

        let image_name = format!("thumb-{entity_id}");

        let Some(image_data) = draw_circular_thumbnail(&img, dpr) else {
            continue;
        };

        if map
            .add_image_with_options(&image_name, image_data.as_ref(), &opts)
            .is_ok()
        {
            batch.insert(entity_id, image_name);
        }
    }

    if !batch.is_empty() {
        signals.set_thumbnails.update(|m| m.extend(batch));
    }

    #[cfg(feature = "test-hooks")]
    dispatch_window_event(THUMBNAILS_LOADED_EVENT);
}

/// Load an image from a URL, returning the `HtmlImageElement` when loaded.
async fn load_image(url: &str) -> Option<web_sys::HtmlImageElement> {
    let img = web_sys::HtmlImageElement::new().ok()?;
    img.set_cross_origin(Some("anonymous"));

    let img_for_handlers = img.clone();
    let promise = js_sys::Promise::new(&mut move |resolve, reject| {
        let resolve_cb = Closure::once_into_js(move || {
            let _ = resolve.call0(&JsValue::NULL);
        });
        let reject_cb = Closure::once_into_js(move || {
            let _ = reject.call0(&JsValue::NULL);
        });
        img_for_handlers.set_onload(Some(resolve_cb.unchecked_ref()));
        img_for_handlers.set_onerror(Some(reject_cb.unchecked_ref()));
    });

    img.set_src(url);

    wasm_bindgen_futures::JsFuture::from(promise).await.ok()?;
    Some(img)
}

// ==================== Click handler helpers ====================

/// Handle a click on the entity circles layer.
fn handle_entity_click(event: JsValue, set_selected: WriteSignal<Option<EntitySelection>>) {
    let features = js_sys::Reflect::get(&event, &"features".into()).ok();
    let features = features.and_then(|f| f.dyn_into::<js_sys::Array>().ok());
    let Some(features) = features else { return };
    if features.length() == 0 {
        return;
    }
    let feature = features.get(0);
    let Some(props) = js_sys::Reflect::get(&feature, &"properties".into()).ok() else {
        return;
    };

    let count = js_sys::Reflect::get(&props, &"count".into())
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0) as u32;

    if count > 1 {
        // Co-located group — deserialize picker entries from JSON property
        if let Ok(group_val) = js_sys::Reflect::get(&props, &"group".into())
            && let Some(json) = group_val.as_string()
            && let Ok(entries) = serde_json::from_str::<Vec<EntityPickerEntry>>(&json)
        {
            set_selected.set(Some(EntitySelection::Multiple(entries)));
        }
    } else if let Ok(id_val) = js_sys::Reflect::get(&props, &"id".into())
        && let Some(id) = id_val.as_string()
    {
        set_selected.set(Some(EntitySelection::Single(id, None)));
    }
}

/// Handle a click on the map background (not on an entity marker).
fn handle_background_click(
    event: JsValue,
    map: &maplibre::Map,
    set_selected: WriteSignal<Option<EntitySelection>>,
) {
    let point = js_sys::Reflect::get(&event, &"point".into()).ok();
    if let Some(point) = point {
        let opts = js_sys::Object::new();
        let layers = js_sys::Array::of2(
            &JsValue::from_str(ENTITY_CIRCLES_LAYER),
            &JsValue::from_str(ENTITY_THUMBNAILS_LAYER),
        );
        let _ = js_sys::Reflect::set(&opts, &"layers".into(), &layers);
        let features = map.query_rendered_features(&point, &opts);
        if features.length() > 0 {
            // Click hit an entity — the layer handler already fired
            return;
        }
    }
    // Nothing hit — clear selection
    set_selected.set(None);
}

// ==================== Map lifecycle helpers ====================

/// Register click, hover, and background-click handlers on the entity layer.
///
/// Returns the closures that must be kept alive for the handlers to work.
/// (`wasm_bindgen::Closure` is invalidated when dropped — the Vec keeps
/// them alive for the map's lifetime, and they're cleared on cleanup/remount.)
/// Register click + hover handlers for a single entity layer.
///
/// Each layer needs its own `Closure` instances (MapLibre takes ownership),
/// so this is called once per interactive layer.
fn register_entity_layer(
    map: &maplibre::Map,
    layer: &str,
    set_selected: WriteSignal<Option<EntitySelection>>,
    closures: &mut Vec<Box<dyn std::any::Any>>,
) {
    let click_cb = Closure::<dyn Fn(JsValue)>::new(move |event: JsValue| {
        handle_entity_click(event, set_selected);
    });
    map.on_layer("click", layer, click_cb.as_ref());
    closures.push(Box::new(click_cb));

    let map_for_enter = map.clone();
    let enter_cb = Closure::<dyn Fn()>::new(move || {
        maplibre::set_cursor(&map_for_enter, "pointer");
    });
    map.on_layer("mouseenter", layer, enter_cb.as_ref());
    closures.push(Box::new(enter_cb));

    let map_for_leave = map.clone();
    let leave_cb = Closure::<dyn Fn()>::new(move || {
        maplibre::set_cursor(&map_for_leave, "");
    });
    map.on_layer("mouseleave", layer, leave_cb.as_ref());
    closures.push(Box::new(leave_cb));
}

fn register_layer_handlers(
    map: &maplibre::Map,
    set_selected: WriteSignal<Option<EntitySelection>>,
) -> Vec<Box<dyn std::any::Any>> {
    let mut closures: Vec<Box<dyn std::any::Any>> = Vec::new();

    register_entity_layer(map, ENTITY_CIRCLES_LAYER, set_selected, &mut closures);
    register_entity_layer(map, ENTITY_THUMBNAILS_LAYER, set_selected, &mut closures);

    // --- Map background click: dismiss panel when clicking empty area ---
    let map_for_bg = map.clone();
    let bg_click_cb = Closure::<dyn Fn(JsValue)>::new(move |event: JsValue| {
        handle_background_click(event, &map_for_bg, set_selected);
    });
    map.on("click", bg_click_cb.as_ref());
    closures.push(Box::new(bg_click_cb));

    closures
}

/// Set up the debounced `moveend` handler that re-fetches entities on pan/zoom.
///
/// Returns the closure that must be kept alive.
fn register_moveend_handler(
    map: &maplibre::Map,
    api_client: &Rc<RefCell<Option<api::Client>>>,
    generation: &Rc<Cell<u64>>,
    debounce_timer: &Rc<Cell<Option<i32>>>,
    signals: ViewportSignals,
) -> Box<dyn std::any::Any> {
    let map_for_move = map.clone();
    let api_for_move = Rc::clone(api_client);
    let gen_for_move = Rc::clone(generation);
    let debounce_for_move = Rc::clone(debounce_timer);

    let move_cb = Closure::<dyn Fn()>::new(move || {
        // Clear any pending debounce timer
        if let Some(timer_id) = debounce_for_move.get()
            && let Some(w) = web_sys::window()
        {
            w.clear_timeout_with_handle(timer_id);
        }

        let map_ref = map_for_move.clone();
        let gc = Rc::clone(&gen_for_move);
        let debounce_ref = Rc::clone(&debounce_for_move);

        // `once_into_js` transfers ownership to JS: the closure is freed by
        // the JS garbage collector after the timer fires.
        let api_ref = Rc::clone(&api_for_move);
        let timeout_cb = Closure::once_into_js(move || {
            debounce_ref.set(None);
            let g = gc.get() + 1;
            gc.set(g);
            wasm_bindgen_futures::spawn_local(async move {
                let Some(client) = api::get_or_init_client(&api_ref).await else {
                    signals
                        .set_fetch_error
                        .set(Some("Failed to load API configuration".to_string()));
                    return;
                };
                load_entities_for_viewport(&map_ref, &client, g, &gc, signals).await;
            });
        });

        if let Some(w) = web_sys::window()
            && let Ok(id) = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                timeout_cb.unchecked_ref(),
                MOVEEND_DEBOUNCE_MS,
            )
        {
            debounce_for_move.set(Some(id));
        }
    });
    map.on("moveend", move_cb.as_ref());
    Box::new(move_cb)
}

// ==================== Shared map state ====================

/// State shared between the `MapView` component, its Effects, and JS closures.
///
/// All fields are `Rc` (cheap to clone) so we can hand out copies to closures
/// without threading dozens of individual `Rc::clone` calls.
#[derive(Clone)]
struct MapState {
    source_initialized: SourceInitialized,
    generation: Rc<Cell<u64>>,
    debounce_timer: Rc<Cell<Option<i32>>>,
    closures: Rc<RefCell<Vec<Box<dyn std::any::Any>>>>,
    api_client: Rc<RefCell<Option<api::Client>>>,
}

/// Create the map, register the style-load callback (which adds source/layers,
/// click/hover handlers, and kicks off the initial entity fetch), and the
/// debounced moveend handler.
///
/// Returns the closures that must be kept alive and the map instance.
fn initialize_map(
    el: &web_sys::HtmlDivElement,
    state: &MapState,
    signals: ViewportSignals,
    set_selected: WriteSignal<Option<EntitySelection>>,
) -> Option<maplibre::Map> {
    let map = maplibre::create_map(
        el,
        &maplibre::MapOptions {
            style: MAP_STYLE_URL,
            center: [INITIAL_CENTER_LNG, INITIAL_CENTER_LAT],
            zoom: INITIAL_ZOOM,
        },
    )?;

    // --- On style load: add source/layers, register handlers, initial fetch ---
    let st = state.clone();
    let map_for_load = map.clone();
    let load_cb = Closure::<dyn Fn()>::new(move || {
        let map_ref = map_for_load.clone();
        let gc = Rc::clone(&st.generation);
        let api_ref = Rc::clone(&st.api_client);
        let g = gc.get() + 1;
        gc.set(g);

        init_source_and_layers(&map_ref);
        st.source_initialized.set(true);

        let handler_closures = register_layer_handlers(&map_ref, set_selected);
        st.closures.borrow_mut().extend(handler_closures);

        wasm_bindgen_futures::spawn_local(async move {
            let Some(client) = api::get_or_init_client(&api_ref).await else {
                signals
                    .set_fetch_error
                    .set(Some("Failed to load API configuration".to_string()));
                return;
            };
            load_entities_for_viewport(&map_ref, &client, g, &gc, signals).await;
        });
    });

    if map.is_style_loaded() {
        let func: &js_sys::Function = load_cb.as_ref().unchecked_ref();
        let _ = func.call0(&JsValue::NULL);
    } else {
        map.on("load", load_cb.as_ref());
    }
    state.closures.borrow_mut().push(Box::new(load_cb));

    let moveend_closure = register_moveend_handler(
        &map,
        &state.api_client,
        &state.generation,
        &state.debounce_timer,
        signals,
    );
    state.closures.borrow_mut().push(moveend_closure);

    Some(map)
}

// ==================== Effects ====================

/// Mount/remount the MapLibre map when the container DOM node becomes available.
///
/// On first run, `container.get()` is `None` (DOM not yet rendered); the
/// effect re-runs automatically when the `NodeRef` resolves. On subsequent
/// runs (e.g., hot-reload), the old map is destroyed before creating a new one.
fn effect_mount_map(
    container: NodeRef<leptos::html::Div>,
    map_handle: Rc<RefCell<Option<maplibre::Map>>>,
    state: MapState,
    signals: ViewportSignals,
    set_selected: WriteSignal<Option<EntitySelection>>,
) {
    Effect::new(move || {
        let Some(el) = container.get() else { return };

        if let Some(old_map) = map_handle.borrow().as_ref() {
            old_map.remove();
        }
        state.closures.borrow_mut().clear();
        state.source_initialized.set(false);

        if let Some(map) = initialize_map(&el, &state, signals, set_selected) {
            *map_handle.borrow_mut() = Some(map);

            // Signal that the map has mounted (used by test hooks).
            #[cfg(feature = "test-hooks")]
            dispatch_window_event(MAP_READY_EVENT);
        }
    });
}

/// Single choke point for all GeoJSON rebuilds. Tracks both the entity
/// selection and the cached entity list — rebuilds whenever either changes
/// (selection click, viewport fetch, or retry).
fn effect_rebuild_geojson(
    signals: ViewportSignals,
    map_handle: Rc<RefCell<Option<maplibre::Map>>>,
    source_initialized: SourceInitialized,
) {
    Effect::new(move || {
        let selected_id = match signals.selected.get() {
            Some(EntitySelection::Single(id, _)) => Some(id),
            _ => None,
        };

        // Tracked reads: re-runs this effect when entities or thumbnails change.
        let entities = signals.cached_entities.get();
        let thumbs = signals.thumbnails.get();

        if source_initialized.get()
            && !entities.is_empty()
            && let Some(geojson) = build_geojson(&entities, selected_id.as_deref(), &thumbs)
            && let Some(map) = map_handle.borrow().as_ref()
        {
            update_source_data(map, &geojson);
        }
    });
}

/// Re-fetch entities when the retry signal fires (after a failed fetch).
fn effect_retry_on_signal(
    retry_signal: ReadSignal<bool>,
    set_retry: WriteSignal<bool>,
    map_handle: Rc<RefCell<Option<maplibre::Map>>>,
    state: MapState,
    signals: ViewportSignals,
) {
    Effect::new(move || {
        if retry_signal.get() {
            set_retry.set(false);
            let map_ref = map_handle.borrow();
            if let Some(map) = map_ref.as_ref() {
                let map = map.clone();
                let st = state.clone();
                let g = st.generation.get() + 1;
                st.generation.set(g);
                wasm_bindgen_futures::spawn_local(async move {
                    let Some(client) = api::get_or_init_client(&st.api_client).await else {
                        signals
                            .set_fetch_error
                            .set(Some("Failed to load API configuration".to_string()));
                        return;
                    };
                    load_entities_for_viewport(&map, &client, g, &st.generation, signals).await;
                });
            }
        }
    });
}

// ==================== Component ====================

#[component]
pub fn MapView(
    api_client: Rc<RefCell<Option<api::Client>>>,
    /// Shared map handle — populated when the map mounts. Allows external
    /// code (e.g., test hooks) to access the live map instance.
    #[prop(optional)]
    map_handle: Option<Rc<RefCell<Option<maplibre::Map>>>>,
) -> impl IntoView {
    let container = NodeRef::<leptos::html::Div>::new();
    let map_handle = map_handle.unwrap_or_else(|| Rc::new(RefCell::new(None)));

    // Selected entity signal — provided to parent via context
    let (selected, set_selected) = signal(None::<EntitySelection>);
    provide_context(SelectedEntity(selected, set_selected));

    // Map status signals — provided to parent via context
    let (loading, set_loading) = signal(false);
    let (truncated, set_truncated) = signal(false);
    let (empty, set_empty) = signal(false);
    let (fetch_error, set_fetch_error) = signal(None::<String>);
    let (retry_signal, set_retry) = signal(false);
    provide_context(MapStatus {
        loading,
        truncated,
        empty,
        fetch_error,
        retry: set_retry,
    });

    let (cached_entities, set_cached_entities) = signal(Vec::<api::EntitySummary>::new());
    let (thumbnails, set_thumbnails) = signal(ThumbnailMap::new());

    let signals = ViewportSignals {
        set_loading,
        set_truncated,
        set_empty,
        set_fetch_error,
        selected,
        cached_entities,
        set_cached_entities,
        thumbnails,
        set_thumbnails,
    };

    // Shared state for JS closures. Grouped into a struct so we clone once
    // instead of threading a dozen individual Rc::clone calls.
    let state = MapState {
        source_initialized: Rc::new(Cell::new(false)),
        generation: Rc::new(Cell::new(0)),
        debounce_timer: Rc::new(Cell::new(None)),
        // wasm_bindgen::Closure must be kept alive as long as JS holds a reference
        // to the callback. Dropping a Closure invalidates the JS-side function
        // reference. This vec keeps them alive for the map's lifetime; it's
        // cleared on cleanup or remount.
        closures: Rc::new(RefCell::new(Vec::new())),
        api_client,
    };
    effect_mount_map(
        container,
        Rc::clone(&map_handle),
        state.clone(),
        signals,
        set_selected,
    );
    effect_rebuild_geojson(
        signals,
        Rc::clone(&map_handle),
        Rc::clone(&state.source_initialized),
    );
    effect_retry_on_signal(
        retry_signal,
        set_retry,
        Rc::clone(&map_handle),
        state.clone(),
        signals,
    );

    // Clean up on unmount: destroy the map and release JS closure references.
    let cleanup_handle = SendWrapper::new(Rc::clone(&map_handle));
    let cleanup_closures = SendWrapper::new(Rc::clone(&state.closures));
    on_cleanup(move || {
        if let Some(map) = cleanup_handle.borrow().as_ref() {
            map.remove();
        }
        cleanup_closures.borrow_mut().clear();
    });

    view! {
        <div node_ref=container class="w-full h-full"/>
    }
}
