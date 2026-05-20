use std::cell::{Cell, RefCell};
use std::future::Future;
use std::rc::Rc;

use futures_util::future::{AbortHandle, AbortRegistration, Abortable};
use leptos::prelude::*;
use send_wrapper::SendWrapper;
use serde::Serialize;
use serde_json::json;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::api;
use crate::maplibre;

/// Serialize a value to a JS object using JSON-compatible mode.
///
/// `serde_wasm_bindgen`'s default serializer produces JS `Map` objects for
/// Rust maps/structs. MapLibre (and most JS APIs) expect plain `Object`s
/// with dot-accessible properties. This helper always uses `json_compatible()`
/// to avoid that footgun.
pub(crate) fn to_js<T: Serialize>(value: &T) -> Result<JsValue, serde_wasm_bindgen::Error> {
    value.serialize(&serde_wasm_bindgen::Serializer::json_compatible())
}

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

/// Monotonic count of fetches that have *settled* (returned a result, success
/// or failure). Aborted fetches do not bump it because the future is dropped
/// before the settle point. Tests sample this counter before triggering an
/// action, then wait for it to advance — that's how "wait for the fetch
/// that follows my action" is expressed without a generation race.
#[cfg(feature = "test-hooks")]
thread_local! {
    static FETCH_SETTLED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(feature = "test-hooks")]
pub(crate) fn current_fetch_settled() -> u64 {
    FETCH_SETTLED.with(std::cell::Cell::get)
}

#[cfg(feature = "test-hooks")]
fn dispatch_window_event(name: &str) {
    if let Some(window) = web_sys::window()
        && let Ok(event) = web_sys::Event::new(name)
    {
        let _ = window.dispatch_event(&event);
    }
}

/// Bump `FETCH_SETTLED` and fire the fetch-complete event with the post-bump
/// count as `event.detail`, so listeners can identify which completion they
/// are seeing.
#[cfg(feature = "test-hooks")]
fn record_fetch_settled() {
    let count = FETCH_SETTLED.with(|c| {
        let next = c.get() + 1;
        c.set(next);
        next
    });
    if let Some(window) = web_sys::window() {
        let init = web_sys::CustomEventInit::new();
        init.set_detail(&JsValue::from_f64(count as f64));
        if let Ok(event) =
            web_sys::CustomEvent::new_with_event_init_dict(FETCH_COMPLETE_EVENT, &init)
        {
            let _ = window.dispatch_event(&event);
        }
    }
}

/// Name of the symbol layer for thumbnail markers.
pub(crate) const ENTITY_THUMBNAILS_LAYER: &str = "entity-thumbnails";

/// DOM event name signaling thumbnail image loading completion (used by test hooks).
#[cfg(feature = "test-hooks")]
pub(crate) const THUMBNAILS_LOADED_EVENT: &str = "chronoscope-thumbnails-loaded";

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

/// Re-export the picker entry type from the API client.
pub use chronoscope_api_client::EntityPickerEntry;

/// A map marker — the map component's uniform view of anything rendered
/// on the map. The server decides whether to return individual entities or
/// region clusters; the map just renders markers with positions, labels,
/// counts, thumbnails, and click actions.
#[derive(Clone, Debug)]
struct MapMarker {
    id: String,
    /// Geographic position as `(latitude, longitude)`.
    position: (f64, f64),
    label: Option<String>,
    click_action: api::ClickAction,
    /// True if the server indicated a thumbnail exists (image may still be loading).
    has_thumbnail: bool,
    /// Dropping cancels any pending thumbnail load for this marker.
    _thumbnail_abort: Option<AbortHandle>,
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
/// Build a GeoJSON `FeatureCollection` from a list of map markers.
///
/// One marker = one feature. The server handles co-location grouping,
/// so there's no client-side coordinate dedup or entity/cluster branching.
fn build_markers_geojson(markers: &[MapMarker]) -> Option<JsValue> {
    use geojson::{Feature, FeatureCollection, Geometry, Value, feature};

    let features: Vec<Feature> = markers
        .iter()
        .map(|marker| {
            // GeoJSON Point is [lon, lat]
            let geometry = Geometry::new(Value::Point(vec![marker.position.1, marker.position.0]));
            let mut props = serde_json::Map::new();
            props.insert("feature_id".into(), marker.id.clone().into());
            if let Some(ref label) = marker.label {
                props.insert("name".into(), label.clone().into());
            }

            match &marker.click_action {
                api::ClickAction::Select { entity_id } => {
                    props.insert("kind".into(), "entity".into());
                    props.insert("id".into(), entity_id.to_string().into());
                }
                api::ClickAction::ZoomTo { bbox } => {
                    props.insert("kind".into(), "cluster".into());
                    props.insert("bbox_min_lat".into(), bbox.min_lat().into());
                    props.insert("bbox_max_lat".into(), bbox.max_lat().into());
                    props.insert("bbox_min_lon".into(), bbox.min_lon().into());
                    props.insert("bbox_max_lon".into(), bbox.max_lon().into());
                }
                api::ClickAction::Disambiguate { entries } => {
                    props.insert("kind".into(), "entity".into());
                    if let Ok(json) = serde_json::to_string(entries) {
                        props.insert("group".into(), json.into());
                    }
                }
            }

            // Mark features whose thumbnails are loading so the circle layer
            // can show a distinct "pending" style. The `thumbnail` property
            // (which triggers the symbol layer) is patched in via updateData
            // once the image actually loads.
            if marker.has_thumbnail {
                props.insert("has_thumbnail".into(), true.into());
            }

            Feature {
                // Set the GeoJSON-level id so MapLibre's updateData can
                // match features by id. promoteId alone isn't sufficient —
                // updateData resolves against the feature's own id field.
                id: Some(feature::Id::String(marker.id.clone())),
                geometry: Some(geometry),
                properties: Some(props),
                ..Feature::default()
            }
        })
        .collect();

    let collection = FeatureCollection {
        features,
        bbox: None,
        foreign_members: None,
    };

    to_js(&collection).ok()
}

// ==================== MapLibre source/layer setup ====================

/// Typed GeoJSON source specification for `map.addSource`.
///
/// `promote_id: "feature_id"` tells MapLibre to use the `feature_id`
/// property as each feature's stable ID. This is required for
/// `setFeatureState` (selection highlighting) and for deduplicating
/// `queryRenderedFeatures` across layers.
#[derive(Serialize)]
struct GeoJsonSourceSpec {
    r#type: &'static str,
    data: serde_json::Value,
    #[serde(rename = "promoteId")]
    promote_id: &'static str,
}

/// Initialize empty GeoJSON source and layers on the map.
///
/// Must be called synchronously in the map load callback (before any async
/// work). Use `update_source_data` to populate with real data.
fn init_source_and_layers(map: &maplibre::Map) {
    let source = GeoJsonSourceSpec {
        r#type: "geojson",
        data: json!({"type": "FeatureCollection", "features": []}),
        promote_id: "feature_id",
    };
    let Ok(source_js) = to_js(&source) else {
        web_sys::console::error_1(&"Failed to serialize GeoJSON source spec".into());
        return;
    };
    if let Err(e) = map.add_source(ENTITY_SOURCE_ID, &source_js) {
        web_sys::console::error_1(&format!("Failed to add GeoJSON source: {e:?}").into());
        return;
    }

    // Coalesce expressions: MapLibre evaluates paint expressions even
    // during source initialization when properties/feature-state may be
    // null. Wrap in ["coalesce", ..., default] to avoid type errors.
    let get_selected = json!(["coalesce", ["feature-state", "selected"], false]);
    let has_thumbnail = json!(["coalesce", ["get", "has_thumbnail"], false]);

    // Circle layer for the marker dots.
    // - Hidden when a loaded thumbnail is displayed (symbol layer takes over).
    // - Markers with a pending thumbnail show a larger, lighter placeholder
    //   circle so the user knows an image is loading.
    let circle = json!({
        "id": ENTITY_CIRCLES_LAYER,
        "type": "circle",
        "source": ENTITY_SOURCE_ID,
        "filter": ["!", ["has", "thumbnail"]],
        "paint": {
            "circle-radius": ["case", has_thumbnail, 20, 10],
            "circle-color": ["case", has_thumbnail, "#D5C4A1", "#8B5E3C"],
            "circle-stroke-width": ["case", get_selected, 4, 2],
            "circle-stroke-color": ["case", get_selected, "#FFFFFF", "#F5F0E8"],
            "circle-opacity": ["case", has_thumbnail, 0.5, 0.85]
        }
    });
    let Ok(circle_js) = to_js(&circle) else {
        web_sys::console::error_1(&"Failed to serialize circle layer spec".into());
        return;
    };
    if let Err(e) = map.add_layer(&circle_js) {
        web_sys::console::error_1(&format!("Failed to add circle layer: {e:?}").into());
        return;
    }

    // Symbol layer for marker labels — added BEFORE thumbnails so thumbnails
    // render on top and aren't occluded by neighboring labels.
    // - Co-located entity groups show the count.
    // - Clusters show the region name and count.
    // - Single entities show no label (the thumbnail or circle is enough).
    // Read label styling from the basemap's city label layer so our markers
    // match the basemap visually, regardless of which style is loaded.
    let basemap = "label_city";
    let labels = json!({
        "id": "entity-labels",
        "type": "symbol",
        "source": ENTITY_SOURCE_ID,
        "filter": ["has", "name"],
        "layout": {
            "text-field": ["get", "name"],
            "text-size": 12,
            "text-anchor": "top",
            "text-offset": [0, 0.8],
            "text-allow-overlap": false,
            "text-ignore-placement": false,
            "text-max-width": 8
        }
    });
    let Ok(labels_js) = to_js(&labels) else {
        web_sys::console::error_1(&"Failed to serialize labels layer spec".into());
        return;
    };
    // Copy font and paint properties from the basemap label layer.
    let Ok(layout) = js_sys::Reflect::get(&labels_js, &"layout".into()) else {
        web_sys::console::error_1(&"Failed to read layout from labels spec".into());
        return;
    };
    let font = map.get_layout_property(basemap, "text-font");
    if !font.is_undefined() {
        let _ = js_sys::Reflect::set(&layout, &"text-font".into(), &font);
    }
    let paint = js_sys::Object::new();
    for prop in &[
        "text-color",
        "text-halo-color",
        "text-halo-width",
        "text-halo-blur",
    ] {
        let val = map.get_paint_property(basemap, prop);
        if !val.is_undefined() {
            let _ = js_sys::Reflect::set(&paint, &(*prop).into(), &val);
        }
    }
    let _ = js_sys::Reflect::set(&labels_js, &"paint".into(), &paint);
    if let Err(e) = map.add_layer(&labels_js) {
        web_sys::console::error_1(&format!("Failed to add labels layer: {e:?}").into());
    }

    // Symbol layer for thumbnail images — added LAST so thumbnails render on
    // top of all other marker layers (circles and labels).
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
    let Ok(thumbnails_js) = to_js(&thumbnails) else {
        web_sys::console::error_1(&"Failed to serialize thumbnails layer spec".into());
        return;
    };
    if let Err(e) = map.add_layer(&thumbnails_js) {
        web_sys::console::error_1(&format!("Failed to add thumbnails layer: {e:?}").into());
    }
}

/// Update an existing GeoJSON source with new data.
fn update_source_data(map: &maplibre::Map, source_id: &str, geojson: &JsValue) {
    match map.get_source(source_id) {
        Some(source) => source.set_data(geojson),
        None => {
            web_sys::console::warn_1(
                &format!("GeoJSON source '{source_id}' not found on map — was init_source_and_layers called?").into(),
            );
        }
    }
}

/// Clear a GeoJSON source by setting it to an empty feature collection.
fn clear_source_data(map: &maplibre::Map, source_id: &str) {
    let empty = serde_json::json!({
        "type": "FeatureCollection",
        "features": []
    });
    if let Ok(js) = to_js(&empty) {
        update_source_data(map, source_id, &js);
    }
}

// ==================== Entity fetching ====================

/// Track whether the GeoJSON source has been added to the map.
///
/// This is a Leptos signal so that effects (`effect_rebuild_geojson`,
/// `effect_selection`) track it reactively rather than reading an untracked
/// `Rc<Cell<bool>>` that wouldn't trigger re-runs.
type SourceInitialized = RwSignal<bool>;

/// Signals needed by viewport entity fetching and GeoJSON rebuilds.
///
/// All fields are `Copy` (Leptos signals), so this struct is `Copy` too —
/// it can be captured by closures without cloning.
#[derive(Clone, Copy)]
struct ViewportSignals {
    set_loading: WriteSignal<bool>,
    set_truncated: WriteSignal<bool>,
    set_empty: WriteSignal<bool>,
    set_fetch_error: WriteSignal<Option<String>>,
    set_map_error: WriteSignal<Option<String>>,
    selected: ReadSignal<Option<EntitySelection>>,
    /// All markers currently rendered on the map.
    cached_markers: ReadSignal<Vec<MapMarker>>,
    set_cached_markers: WriteSignal<Vec<MapMarker>>,
}

async fn load_entities_for_viewport(
    map: &maplibre::Map,
    client: &api::Client,
    signals: ViewportSignals,
) {
    let (min_lon, min_lat, max_lon, max_lat) = maplibre::get_viewport_bounds(map);

    signals.set_loading.set(true);
    signals.set_fetch_error.set(None);
    signals.set_map_error.set(None);

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

    // Single call to /markers — the server decides granularity.
    let result = client.list_markers(&bbox).await;

    match result {
        Ok(response) => {
            signals.set_loading.set(false);
            signals.set_truncated.set(response.truncated);
            signals.set_fetch_error.set(None);
            signals.set_empty.set(response.markers.is_empty());

            // Collect thumbnail URLs before markers are moved into the signal.
            let marker_thumbnail_urls: std::collections::HashMap<String, String> = response
                .markers
                .iter()
                .filter_map(|m| {
                    m.thumbnail_url
                        .as_ref()
                        .map(|url| (m.id.to_string(), url.clone()))
                })
                .collect();

            // Convert API markers to MapMarkers. Thumbnail loads are spawned
            // below — each marker just stores an AbortHandle for cancellation.
            let mut new_markers: Vec<MapMarker> = response
                .markers
                .into_iter()
                .map(|m| {
                    let has_thumbnail = m.thumbnail_url.is_some();
                    MapMarker {
                        id: m.id.to_string(),
                        position: (m.latitude, m.longitude),
                        label: m.label,
                        click_action: m.click_action,
                        has_thumbnail,
                        _thumbnail_abort: None,
                    }
                })
                .collect();

            // For each marker with a thumbnail URL, spawn an image load in
            // the viewport scope. When the image loads: draw canvas → register
            // with MapLibre → patch the single feature via updateData.
            let dpr = device_pixel_ratio();
            for marker in &mut new_markers {
                if let Some(url) = marker_thumbnail_urls.get(&marker.id) {
                    let map = map.clone();
                    let marker_id = marker.id.clone();
                    let url = url.to_string();
                    let (abort_handle, abort_reg) = AbortHandle::new_pair();
                    wasm_bindgen_futures::spawn_local({
                        let fut = async move {
                            let img = match load_image(&url).await {
                                Ok(img) => img,
                                Err(e) => {
                                    web_sys::console::warn_1(
                                        &format!("thumbnail load failed for {marker_id}: {e}")
                                            .into(),
                                    );
                                    return;
                                }
                            };
                            let image_data = match draw_circular_thumbnail(&img, dpr) {
                                Ok(data) => data,
                                Err(e) => {
                                    web_sys::console::warn_1(
                                        &format!("thumbnail draw failed for {marker_id}: {e}")
                                            .into(),
                                    );
                                    return;
                                }
                            };
                            let image_name = format!("thumb-{marker_id}");
                            let opts = js_sys::Object::new();
                            let _ = js_sys::Reflect::set(&opts, &"pixelRatio".into(), &dpr.into());
                            // A prior fetch may have already registered this image
                            // (duplicate fetches from moveend race). Skip add_image
                            // but still updateData below to re-apply the property
                            // that set_data wiped.
                            if !map.has_image(&image_name)
                                && let Err(e) = map.add_image_with_options(
                                    &image_name,
                                    image_data.as_ref(),
                                    &opts,
                                )
                            {
                                web_sys::console::warn_1(
                                    &format!("add_image failed for {marker_id}: {e:?}").into(),
                                );
                                return;
                            }
                            // Patch this feature's thumbnail property via updateData
                            if let Some(source) = map.get_source(ENTITY_SOURCE_ID) {
                                let diff = serde_json::json!({
                                    "update": [{
                                        "id": marker_id,
                                        "addOrUpdateProperties": [
                                            {"key": "thumbnail", "value": image_name}
                                        ]
                                    }]
                                });
                                if let Ok(diff_js) = to_js(&diff) {
                                    source.update_data(&diff_js);
                                }
                            }

                            // Signal thumbnail readiness for test hooks.
                            #[cfg(feature = "test-hooks")]
                            dispatch_window_event(THUMBNAILS_LOADED_EVENT);
                        };
                        async move {
                            let _ = Abortable::new(fut, abort_reg).await;
                        }
                    });
                    marker._thumbnail_abort = Some(abort_handle);
                }
            }

            // If no thumbnails to load, signal readiness immediately so
            // test hooks don't hang waiting for an event that will never fire.
            #[cfg(feature = "test-hooks")]
            if marker_thumbnail_urls.is_empty() {
                dispatch_window_event(THUMBNAILS_LOADED_EVENT);
            }

            signals.set_cached_markers.set(new_markers);
        }
        Err(e) => {
            signals.set_loading.set(false);
            signals.set_fetch_error.set(Some(format!("{e}")));
        }
    }

    #[cfg(feature = "test-hooks")]
    record_fetch_settled();
}

// ==================== Thumbnail registration ====================
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
) -> Result<web_sys::ImageData, String> {
    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or("no document")?;
    let canvas = document
        .create_element("canvas")
        .map_err(|e| format!("create_element failed: {e:?}"))?
        .dyn_into::<web_sys::HtmlCanvasElement>()
        .map_err(|_| "cast to HtmlCanvasElement failed")?;
    let thumb_r = f64::from(THUMBNAIL_SIZE) / 2.0 * dpr;
    let border = 2.0 * dpr;
    let stem_len = STEM_LENGTH * dpr;
    let dot_r = DOT_RADIUS * dpr;
    // Use ceil() so non-integer device pixel ratios (1.5/1.75/2.625 on
    // some Windows and Android devices) don't clip the bottom row of the
    // drop shadow.
    let canvas_w = (f64::from(THUMBNAIL_SIZE) * dpr).ceil() as u32;
    // Extra space below dot for the drop shadow (blur + offset can extend
    // further than `dot_r` alone, especially on Retina displays).
    let shadow_blur = 4.0 * dpr;
    let shadow_offset_y = 2.0 * dpr;
    let bottom_pad = dot_r.max(shadow_blur + shadow_offset_y);
    let canvas_h =
        (f64::from(THUMBNAIL_SIZE) * dpr + stem_len + dot_r * 2.0 + bottom_pad).ceil() as u32;
    canvas.set_width(canvas_w);
    canvas.set_height(canvas_h);

    let ctx = canvas
        .get_context("2d")
        .map_err(|e| format!("getContext failed: {e:?}"))?
        .ok_or("getContext returned None")?
        .dyn_into::<web_sys::CanvasRenderingContext2d>()
        .map_err(|_| "cast to CanvasRenderingContext2d failed")?;

    let cx = f64::from(canvas_w) / 2.0; // horizontal center
    let thumb_cy = thumb_r; // thumbnail circle center Y

    // --- Thumbnail circle (center-cropped for non-square sources) ---
    ctx.save();
    ctx.begin_path();
    ctx.arc(cx, thumb_cy, thumb_r - border, 0.0, std::f64::consts::TAU)
        .map_err(|e| format!("arc failed: {e:?}"))?;
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
    .map_err(|e| format!("drawImage failed: {e:?}"))?;
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
    .map_err(|e| format!("arc failed: {e:?}"))?;
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
        .map_err(|e| format!("arc failed: {e:?}"))?;
    ctx.fill();
    // Restore before stroke so the shadow only applies to the fill, not the border
    ctx.restore();
    ctx.set_stroke_style_str("#F5F0E8"); // parchment stroke
    ctx.set_line_width(2.0 * dpr);
    ctx.stroke();

    ctx.get_image_data(0.0, 0.0, f64::from(canvas_w), f64::from(canvas_h))
        .map_err(|e| format!("getImageData failed: {e:?}"))
}

/// Load an image from a URL, returning the `HtmlImageElement` when loaded.
///
/// `crossOrigin = "anonymous"` is required so the resulting image can be
/// drawn to a canvas and read back via `getImageData` without tainting it.
/// The thumbnail server must respond with `Access-Control-Allow-Origin`
/// — without it, every image silently fails to load.
async fn load_image(url: &str) -> Result<web_sys::HtmlImageElement, String> {
    let img = web_sys::HtmlImageElement::new()
        .map_err(|e| format!("HtmlImageElement::new failed: {e:?}"))?;
    img.set_cross_origin(Some("anonymous"));

    let img_for_handlers = img.clone();
    let promise = js_sys::Promise::new(&mut move |resolve, reject| {
        // TODO: on abort, the <img> may outlive the future, leaking
        // these closures until the orphaned element is GC'd. Bounded leak.
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

    wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map_err(|_| format!("image load failed for {url}"))?;
    Ok(img)
}

// ==================== Click handler helpers ====================

/// What a click on a marker resolves to.
///
/// Parsed from the flat GeoJSON feature properties (set in
/// `build_markers_geojson`) into a proper discriminated type.
enum ClickTarget {
    /// Zoom to a region bounding box (cluster click).
    ZoomTo {
        bbox_min_lat: f64,
        bbox_max_lat: f64,
        bbox_min_lon: f64,
        bbox_max_lon: f64,
    },
    /// Select a single entity.
    Select(String),
    /// Disambiguate co-located entities.
    Disambiguate(Vec<EntityPickerEntry>),
}

/// Raw deserialization target for GeoJSON feature properties.
/// Immediately converted to [`ClickTarget`] — never used directly.
#[derive(serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum RawMarkerProps {
    Entity {
        id: Option<String>,
        group: Option<String>,
    },
    Cluster {
        bbox_min_lat: f64,
        bbox_max_lat: f64,
        bbox_min_lon: f64,
        bbox_max_lon: f64,
    },
}

impl RawMarkerProps {
    fn into_click_target(self) -> Option<ClickTarget> {
        match self {
            Self::Cluster {
                bbox_min_lat,
                bbox_max_lat,
                bbox_min_lon,
                bbox_max_lon,
            } => Some(ClickTarget::ZoomTo {
                bbox_min_lat,
                bbox_max_lat,
                bbox_min_lon,
                bbox_max_lon,
            }),
            Self::Entity {
                group: Some(json), ..
            } => serde_json::from_str::<Vec<EntityPickerEntry>>(&json)
                .ok()
                .map(ClickTarget::Disambiguate),
            Self::Entity { id: Some(id), .. } => Some(ClickTarget::Select(id)),
            Self::Entity {
                id: None,
                group: None,
            } => None,
        }
    }
}

/// Handle a click on a marker (entity or cluster), dispatching on `kind`.
fn handle_marker_click(
    event: JsValue,
    map: &maplibre::Map,
    set_selected: WriteSignal<Option<EntitySelection>>,
) {
    let features = js_sys::Reflect::get(&event, &"features".into()).ok();
    let features = features.and_then(|f| f.dyn_into::<js_sys::Array>().ok());
    let Some(features) = features else { return };
    if features.length() == 0 {
        return;
    }
    let feature = features.get(0);
    let Some(props_js) = js_sys::Reflect::get(&feature, &"properties".into()).ok() else {
        return;
    };

    let raw: RawMarkerProps = match serde_wasm_bindgen::from_value(props_js) {
        Ok(p) => p,
        Err(e) => {
            web_sys::console::warn_1(&format!("Failed to deserialize marker props: {e}").into());
            return;
        }
    };

    let Some(target) = raw.into_click_target() else {
        return;
    };

    match target {
        ClickTarget::ZoomTo {
            bbox_min_lat,
            bbox_max_lat,
            bbox_min_lon,
            bbox_max_lon,
        } => {
            let sw = js_sys::Array::new();
            sw.push(&bbox_min_lon.into());
            sw.push(&bbox_min_lat.into());
            let ne = js_sys::Array::new();
            ne.push(&bbox_max_lon.into());
            ne.push(&bbox_max_lat.into());
            let bounds = js_sys::Array::new();
            bounds.push(&sw);
            bounds.push(&ne);

            let opts = js_sys::Object::new();
            let _ = js_sys::Reflect::set(&opts, &"padding".into(), &50.into());
            map.fit_bounds(&bounds, &opts);
        }
        ClickTarget::Select(id) => {
            set_selected.set(Some(EntitySelection::Single(id, None)));
        }
        ClickTarget::Disambiguate(entries) => {
            set_selected.set(Some(EntitySelection::Multiple(entries)));
        }
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
/// Register click + hover handlers for a single marker layer.
///
/// Each layer needs its own `Closure` instances (MapLibre takes ownership),
/// so this is called once per interactive layer.
fn register_marker_layer(
    map: &maplibre::Map,
    layer: &str,
    set_selected: WriteSignal<Option<EntitySelection>>,
    closures: &mut Vec<Box<dyn std::any::Any>>,
) {
    let map_for_click = map.clone();
    let click_cb = Closure::<dyn Fn(JsValue)>::new(move |event: JsValue| {
        handle_marker_click(event, &map_for_click, set_selected);
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

    register_marker_layer(map, ENTITY_CIRCLES_LAYER, set_selected, &mut closures);
    register_marker_layer(map, ENTITY_THUMBNAILS_LAYER, set_selected, &mut closures);

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
    state: &MapState,
    signals: ViewportSignals,
) -> Box<dyn std::any::Any> {
    let map_for_move = map.clone();
    let state_for_move = state.clone();

    let move_cb = Closure::<dyn Fn()>::new(move || {
        // Clear any pending debounce timer
        if let Some(timer_id) = state_for_move.debounce_timer.get()
            && let Some(w) = web_sys::window()
        {
            w.clear_timeout_with_handle(timer_id);
        }

        let map_ref = map_for_move.clone();
        let st = state_for_move.clone();

        // `once_into_js` transfers ownership to JS: the closure is freed by
        // the JS garbage collector after the timer fires.
        let timeout_cb = Closure::once_into_js(move || {
            st.debounce_timer.set(None);
            let st2 = st.clone();
            st.scope.spawn(async move {
                let Some(client) = api::get_or_init_client(&st2.api_client).await else {
                    signals
                        .set_fetch_error
                        .set(Some("Failed to load API configuration".to_string()));
                    return;
                };
                load_entities_for_viewport(&map_ref, &client, signals).await;
            });
        });

        if let Some(w) = web_sys::window()
            && let Ok(id) = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                timeout_cb.unchecked_ref(),
                MOVEEND_DEBOUNCE_MS,
            )
        {
            state_for_move.debounce_timer.set(Some(id));
        }
    });
    map.on("moveend", move_cb.as_ref());
    Box::new(move_cb)
}

// ==================== Viewport-scoped task cancellation ====================

/// Structured cancellation for viewport-scoped async work.
///
/// Each viewport change (pan/zoom debounce, retry, initial load) starts a
/// new "epoch" that aborts the prior one. Tasks spawned via [`spawn`] are
/// wrapped in [`Abortable`], so on the next [`reset`] (or next [`spawn`])
/// they're dropped at their next `.await` — including any in-flight network
/// requests, since reqwest/gloo-net both honor future-drop and propagate
/// cancellation to the underlying browser `fetch()`.
///
/// This replaces a manual generation-counter pattern that required every
/// `.await` site to check `if generation_counter.get() != my_generation
/// { return }`. The counter approach was correct in principle but easy to
/// forget; structured cancellation makes the check unforgeable because it
/// happens automatically at the executor level.
///
/// [`spawn`]: ViewportScope::spawn
/// [`reset`]: ViewportScope::reset
#[derive(Clone, Default)]
struct ViewportScope {
    handle: Rc<Cell<Option<AbortHandle>>>,
}

impl ViewportScope {
    /// Abort the current epoch and start a new one. Returns the registration
    /// that should be paired with an [`Abortable`] for any future spawned in
    /// this epoch.
    fn reset(&self) -> AbortRegistration {
        if let Some(h) = self.handle.take() {
            h.abort();
        }
        let (handle, reg) = AbortHandle::new_pair();
        self.handle.set(Some(handle));
        reg
    }

    /// Spawn a future tied to a fresh epoch. Any previously spawned task is
    /// aborted; the new task will itself be aborted by the next call to
    /// `spawn` or `reset`.
    fn spawn<F>(&self, fut: F)
    where
        F: Future<Output = ()> + 'static,
    {
        let reg = self.reset();
        wasm_bindgen_futures::spawn_local(async move {
            let _ = Abortable::new(fut, reg).await;
        });
    }
}

// ==================== Shared map state ====================

/// State shared between the `MapView` component, its Effects, and JS closures.
///
/// All fields are `Rc` (cheap to clone) so we can hand out copies to closures
/// without threading dozens of individual `Rc::clone` calls.
#[derive(Clone)]
struct MapState {
    source_initialized: SourceInitialized,
    scope: ViewportScope,
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
    set_map_error: WriteSignal<Option<String>>,
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
    //
    // We register via `map.once("load", ...)` unconditionally rather than
    // branching on `is_style_loaded()`. MapLibre's `once` fires immediately
    // (on the next tick) if the style is already loaded, and `is_style_loaded`
    // is a known footgun: it can return true while glyph fetches are still
    // outstanding, causing the initial fetch to race with style finalization.
    let st = state.clone();
    let map_for_load = map.clone();
    let load_cb = Closure::<dyn Fn()>::new(move || {
        let map_ref = map_for_load.clone();
        let st2 = st.clone();

        init_source_and_layers(&map_ref);
        st.source_initialized.set(true);

        let handler_closures = register_layer_handlers(&map_ref, set_selected);
        st.closures.borrow_mut().extend(handler_closures);

        st.scope.spawn(async move {
            let Some(client) = api::get_or_init_client(&st2.api_client).await else {
                signals
                    .set_fetch_error
                    .set(Some("Failed to load API configuration".to_string()));
                return;
            };
            load_entities_for_viewport(&map_ref, &client, signals).await;
        });
    });

    map.once("load", load_cb.as_ref());
    state.closures.borrow_mut().push(Box::new(load_cb));

    let moveend_closure = register_moveend_handler(&map, state, signals);
    state.closures.borrow_mut().push(moveend_closure);

    // Surface MapLibre errors from our entity source in the UI. Errors
    // from the basemap (tile loading, style evaluation) are logged but
    // not surfaced since they're outside our control.
    let error_cb = Closure::<dyn Fn(JsValue)>::new(move |event: JsValue| {
        let msg = js_sys::Reflect::get(&event, &"message".into())
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_else(|| "Map style error".to_string());

        let source_id = js_sys::Reflect::get(&event, &"sourceId".into())
            .ok()
            .and_then(|v| v.as_string());

        if source_id.as_deref() == Some(ENTITY_SOURCE_ID) {
            set_map_error.set(Some(msg));
        }
    });
    map.on("error", error_cb.as_ref());
    state.closures.borrow_mut().push(Box::new(error_cb));

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
    set_map_error: WriteSignal<Option<String>>,
) {
    Effect::new(move || {
        let Some(el) = container.get() else { return };

        if let Some(old_map) = map_handle.borrow().as_ref() {
            old_map.remove();
        }
        state.closures.borrow_mut().clear();
        state.source_initialized.set(false);

        if let Some(map) = initialize_map(&el, &state, signals, set_selected, set_map_error) {
            *map_handle.borrow_mut() = Some(map);

            // Signal that the map has mounted (used by test hooks).
            #[cfg(feature = "test-hooks")]
            dispatch_window_event(MAP_READY_EVENT);
        }
    });
}

/// Rebuilds the GeoJSON source when the marker list changes.
///
/// Thumbnails are patched incrementally via `updateData` in per-marker
/// spawn closures (see `load_entities_for_viewport`), so this effect
/// only needs to react to the marker list signal — not to individual
/// thumbnail load completions.
///
/// Selection highlighting is handled separately via `setFeatureState` in
/// `effect_selection` — this effect does not read `signals.selected`, so
/// clicking an entity doesn't trigger a full GeoJSON rebuild+reserialize.
fn effect_rebuild_geojson(
    signals: ViewportSignals,
    map_handle: Rc<RefCell<Option<maplibre::Map>>>,
    source_initialized: SourceInitialized,
) {
    Effect::new(move || {
        // Tracked read: re-runs when marker list changes.
        let markers = signals.cached_markers.get();

        if !source_initialized.get() {
            return;
        }
        let Some(map) = map_handle.borrow().as_ref().cloned() else {
            return;
        };

        if markers.is_empty() {
            clear_source_data(&map, ENTITY_SOURCE_ID);
        } else if let Some(geojson) = build_markers_geojson(&markers) {
            update_source_data(&map, ENTITY_SOURCE_ID, &geojson);
        }
    });
}

/// Apply selection highlighting via `setFeatureState` instead of rebuilding
/// the entire GeoJSON. Tracks `signals.selected` — when the selection changes,
/// clears the old feature state and sets the new one. Only entities (not
/// clusters) can be selected.
fn effect_selection(
    signals: ViewportSignals,
    map_handle: Rc<RefCell<Option<maplibre::Map>>>,
    source_initialized: SourceInitialized,
) {
    // Track the previously-selected feature ID so we can clear its state.
    let prev_id: Rc<Cell<Option<String>>> = Rc::new(Cell::new(None));

    Effect::new(move || {
        let new_id = match signals.selected.get() {
            Some(EntitySelection::Single(id, _)) => Some(id),
            _ => None,
        };

        if !source_initialized.get() {
            return;
        }
        let Some(map) = map_handle.borrow().as_ref().cloned() else {
            return;
        };

        // Clear previous selection
        if let Some(old_id) = prev_id.take() {
            let target = serde_json::json!({
                "source": ENTITY_SOURCE_ID,
                "id": old_id,
            });
            if let Ok(target_js) = to_js(&target) {
                let _ = map.remove_feature_state(&target_js);
            }
        }

        // Set new selection
        if let Some(ref id) = new_id {
            let target = serde_json::json!({
                "source": ENTITY_SOURCE_ID,
                "id": id,
            });
            let state = serde_json::json!({"selected": true});
            if let Ok(target_js) = to_js(&target)
                && let Ok(state_js) = to_js(&state)
            {
                let _ = map.set_feature_state(&target_js, &state_js);
            }
        }

        prev_id.set(new_id);
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
                st.scope.clone().spawn(async move {
                    let Some(client) = api::get_or_init_client(&st.api_client).await else {
                        signals
                            .set_fetch_error
                            .set(Some("Failed to load API configuration".to_string()));
                        return;
                    };
                    load_entities_for_viewport(&map, &client, signals).await;
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

    let (map_error, set_map_error) = signal(None::<String>);

    let (cached_markers, set_cached_markers) = signal(Vec::<MapMarker>::new());

    let signals = ViewportSignals {
        set_loading,
        set_truncated,
        set_empty,
        set_fetch_error,
        set_map_error,
        selected,
        cached_markers,
        set_cached_markers,
    };

    // Shared state for JS closures. Grouped into a struct so we clone once
    // instead of threading a dozen individual Rc::clone calls.
    let state = MapState {
        source_initialized: RwSignal::new(false),
        scope: ViewportScope::default(),
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
        set_map_error,
    );
    effect_rebuild_geojson(signals, Rc::clone(&map_handle), state.source_initialized);
    effect_selection(signals, Rc::clone(&map_handle), state.source_initialized);
    effect_retry_on_signal(
        retry_signal,
        set_retry,
        Rc::clone(&map_handle),
        state.clone(),
        signals,
    );

    // Clean up on unmount: destroy the map and release JS closure references.
    //
    // `SendWrapper` is required because Leptos's `on_cleanup` wants `Send`
    // closures even on single-threaded WASM targets. There are no other
    // threads in browser WASM, so the wrapper's runtime panic-on-cross-
    // thread-access can never fire here — it's purely a type-bound shim.
    let cleanup_handle = SendWrapper::new(Rc::clone(&map_handle));
    let cleanup_closures = SendWrapper::new(Rc::clone(&state.closures));
    let cleanup_debounce = SendWrapper::new(Rc::clone(&state.debounce_timer));
    let cleanup_scope = SendWrapper::new(state.scope.clone());
    on_cleanup(move || {
        // Cancel any pending debounce timer so it doesn't fire after unmount.
        if let Some(timer_id) = cleanup_debounce.get()
            && let Some(w) = web_sys::window()
        {
            w.clear_timeout_with_handle(timer_id);
        }
        // Abort any in-flight viewport fetches so they don't mutate
        // disposed signals or keep the map alive.
        let _ = cleanup_scope.reset();
        if let Some(map) = cleanup_handle.borrow().as_ref() {
            map.remove();
        }
        cleanup_closures.borrow_mut().clear();
    });

    view! {
        <div class="relative w-full h-full">
            <div
                node_ref=container
                class="w-full h-full"
                role="region"
                aria-label="Interactive map"
            />
            {move || {
                map_error.get().map(|msg| view! {
                    <div
                        class="absolute bottom-2 left-2 right-2 bg-red-900/90 text-white text-xs px-3 py-2 rounded shadow"
                        role="alert"
                    >
                        {msg}
                    </div>
                })
            }}
        </div>
    }
}
