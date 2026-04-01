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
const ENTITY_CIRCLES_LAYER: &str = "entity-circles";

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
fn build_geojson(entities: &[api::EntitySummary], selected_id: Option<&str>) -> Option<JsValue> {
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

        let entries: Vec<EntityPickerEntry> = group
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

    // Circle layer for the marker dots
    let circle = json!({
        "id": ENTITY_CIRCLES_LAYER,
        "type": "circle",
        "source": ENTITY_SOURCE_ID,
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
            // Write entities to the signal — the GeoJSON rebuild effect
            // (effect_rebuild_geojson_on_selection) tracks this and will
            // rebuild the map layer automatically.
            signals.set_cached_entities.set(entities);
        }
        Err(e) => {
            signals.set_loading.set(false);
            signals.set_fetch_error.set(Some(format!("{e}")));
        }
    }
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
        let layers = js_sys::Array::of1(&JsValue::from_str(ENTITY_CIRCLES_LAYER));
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
fn register_layer_handlers(
    map: &maplibre::Map,
    set_selected: WriteSignal<Option<EntitySelection>>,
) -> Vec<Box<dyn std::any::Any>> {
    let mut closures: Vec<Box<dyn std::any::Any>> = Vec::new();

    // --- Layer click handler ---
    let click_cb = Closure::<dyn Fn(JsValue)>::new(move |event: JsValue| {
        handle_entity_click(event, set_selected);
    });
    map.on_layer("click", ENTITY_CIRCLES_LAYER, click_cb.as_ref());
    closures.push(Box::new(click_cb));

    // --- Cursor styling on hover ---
    let map_for_enter = map.clone();
    let enter_cb = Closure::<dyn Fn()>::new(move || {
        maplibre::set_cursor(&map_for_enter, "pointer");
    });
    map.on_layer("mouseenter", ENTITY_CIRCLES_LAYER, enter_cb.as_ref());
    closures.push(Box::new(enter_cb));

    let map_for_leave = map.clone();
    let leave_cb = Closure::<dyn Fn()>::new(move || {
        maplibre::set_cursor(&map_for_leave, "");
    });
    map.on_layer("mouseleave", ENTITY_CIRCLES_LAYER, leave_cb.as_ref());
    closures.push(Box::new(leave_cb));

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

        // Tracked read: re-runs this effect when entities change.
        let entities = signals.cached_entities.get();

        if source_initialized.get()
            && !entities.is_empty()
            && let Some(geojson) = build_geojson(&entities, selected_id.as_deref())
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
pub fn MapView(api_client: Rc<RefCell<Option<api::Client>>>) -> impl IntoView {
    let container = NodeRef::<leptos::html::Div>::new();
    let map_handle: Rc<RefCell<Option<maplibre::Map>>> = Rc::new(RefCell::new(None));

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

    let signals = ViewportSignals {
        set_loading,
        set_truncated,
        set_empty,
        set_fetch_error,
        selected,
        cached_entities,
        set_cached_entities,
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
