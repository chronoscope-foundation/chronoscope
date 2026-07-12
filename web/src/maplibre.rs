//! Typed Rust bindings for the MapLibre GL JS API.
//!
//! Only declares the methods we actually use. MapLibre is loaded as a global
//! (`maplibregl`) via a `<script>` tag in `index.html`.

use js_sys::Array;
use serde::Serialize;
use wasm_bindgen::prelude::*;

// ==================== JS type declarations ====================

#[wasm_bindgen]
extern "C" {
    /// A MapLibre GL JS map instance (`maplibregl.Map`).
    #[wasm_bindgen(js_namespace = maplibregl)]
    pub type Map;

    #[wasm_bindgen(constructor, js_namespace = maplibregl, catch)]
    fn new(options: &JsValue) -> Result<Map, JsValue>;

    /// Destroy the map, releasing all resources.
    #[wasm_bindgen(method)]
    pub fn remove(this: &Map);

    /// Register an event handler: `map.on(event, callback)`.
    #[wasm_bindgen(method)]
    pub fn on(this: &Map, event: &str, callback: &JsValue);

    /// Register a one-shot event handler: `map.once(event, callback)`.
    /// Fires immediately (next tick) if the event has already happened —
    /// safer than `on("load", ...)` paired with an `is_style_loaded` check,
    /// since `is_style_loaded` can return true while glyph fetches are still
    /// outstanding.
    #[wasm_bindgen(method)]
    pub fn once(this: &Map, event: &str, callback: &JsValue);

    /// Register a layer-specific event handler: `map.on(event, layer, callback)`.
    #[wasm_bindgen(method, js_name = on)]
    pub fn on_layer(this: &Map, event: &str, layer: &str, callback: &JsValue);

    /// Add a data source to the map. Can throw if the source ID already exists
    /// (e.g., on a hot-reload race).
    #[wasm_bindgen(method, js_name = addSource, catch)]
    pub fn add_source(this: &Map, id: &str, source: &JsValue) -> Result<(), JsValue>;

    /// Add a style layer to the map. Can throw if the layer ID already exists.
    #[wasm_bindgen(method, js_name = addLayer, catch)]
    pub fn add_layer(this: &Map, layer: &JsValue) -> Result<(), JsValue>;

    /// Read a layout property from an existing style layer.
    #[wasm_bindgen(method, js_name = getLayoutProperty)]
    pub fn get_layout_property(this: &Map, layer: &str, name: &str) -> JsValue;

    /// Read a paint property from an existing style layer.
    #[wasm_bindgen(method, js_name = getPaintProperty)]
    pub fn get_paint_property(this: &Map, layer: &str, name: &str) -> JsValue;

    /// Get a source by ID. Returns `undefined` if not found.
    #[wasm_bindgen(method, js_name = getSource)]
    pub fn get_source(this: &Map, id: &str) -> Option<GeoJsonSource>;

    /// Get the map's bounds as a `LngLatBounds` object.
    #[wasm_bindgen(method, js_name = getBounds)]
    pub fn get_bounds(this: &Map) -> LngLatBounds;

    /// Get the map's center as a `LngLat` object.
    #[wasm_bindgen(method, js_name = getCenter)]
    pub fn get_center(this: &Map) -> LngLat;

    /// Whether the map's style is fully loaded.
    #[wasm_bindgen(method, js_name = isStyleLoaded)]
    pub fn is_style_loaded(this: &Map) -> bool;

    /// Get the map's `<canvas>` element.
    #[wasm_bindgen(method, js_name = getCanvas)]
    pub fn get_canvas(this: &Map) -> web_sys::HtmlCanvasElement;

    /// Query rendered features at a point, optionally filtered by layer.
    /// `options` is a JS object like `{ layers: ["entity-circles"] }`.
    #[wasm_bindgen(method, js_name = queryRenderedFeatures)]
    pub fn query_rendered_features(this: &Map, point: &JsValue, options: &JsValue) -> Array;

    /// Add an image to the map's style for use as an icon in symbol layers.
    /// `image` can be an `HTMLImageElement`, `ImageData`, or `ImageBitmap`.
    #[wasm_bindgen(method, js_name = addImage, catch)]
    pub fn add_image(this: &Map, id: &str, image: &JsValue) -> Result<(), JsValue>;

    /// Add an image with options (e.g., `{ pixelRatio: 2 }` for Retina).
    #[wasm_bindgen(method, js_name = addImage, catch)]
    pub fn add_image_with_options(
        this: &Map,
        id: &str,
        image: &JsValue,
        options: &JsValue,
    ) -> Result<(), JsValue>;

    /// Check if an image with the given ID has been added to the map style.
    #[wasm_bindgen(method, js_name = hasImage)]
    pub fn has_image(this: &Map, id: &str) -> bool;

    /// Remove a previously added image from the map style.
    #[wasm_bindgen(method, js_name = removeImage, catch)]
    pub fn remove_image(this: &Map, id: &str) -> Result<(), JsValue>;

    /// Get the current zoom level.
    #[wasm_bindgen(method, js_name = getZoom)]
    pub fn get_zoom_raw(this: &Map) -> f64;

    /// Animate the map to a new center/zoom with smooth flying motion.
    #[wasm_bindgen(method, js_name = flyTo)]
    pub fn fly_to(this: &Map, options: &JsValue);

    /// Set state on a feature. The feature is identified by `{source, id}`.
    /// State is merged (not replaced) on each call. Used for selection
    /// highlighting without rebuilding the entire GeoJSON.
    #[wasm_bindgen(method, js_name = setFeatureState, catch)]
    pub fn set_feature_state(this: &Map, feature: &JsValue, state: &JsValue)
    -> Result<(), JsValue>;

    /// Remove all state from a feature identified by `{source, id}`.
    #[wasm_bindgen(method, js_name = removeFeatureState, catch)]
    pub fn remove_feature_state(this: &Map, feature: &JsValue) -> Result<(), JsValue>;
}

#[wasm_bindgen]
extern "C" {
    /// A MapLibre `LngLat` object.
    pub type LngLat;

    #[wasm_bindgen(method, getter)]
    pub fn lng(this: &LngLat) -> f64;

    #[wasm_bindgen(method, getter)]
    pub fn lat(this: &LngLat) -> f64;
}

#[wasm_bindgen]
extern "C" {
    /// A MapLibre `LngLatBounds` object.
    pub type LngLatBounds;

    #[wasm_bindgen(method, js_name = getWest)]
    pub fn west(this: &LngLatBounds) -> f64;

    #[wasm_bindgen(method, js_name = getSouth)]
    pub fn south(this: &LngLatBounds) -> f64;

    #[wasm_bindgen(method, js_name = getEast)]
    pub fn east(this: &LngLatBounds) -> f64;

    #[wasm_bindgen(method, js_name = getNorth)]
    pub fn north(this: &LngLatBounds) -> f64;
}

#[wasm_bindgen]
extern "C" {
    /// A MapLibre GeoJSON source, obtained via `map.getSource("id")`.
    pub type GeoJsonSource;

    /// Replace the source's GeoJSON data.
    #[wasm_bindgen(method, js_name = setData)]
    pub fn set_data(this: &GeoJsonSource, data: &JsValue);

    /// Apply incremental updates to the source's GeoJSON without replacing
    /// the entire dataset. Takes a `GeoJSONSourceDiff` object with optional
    /// `add`, `update`, and `remove` arrays. Requires features to have
    /// unique IDs (via `promoteId`).
    #[wasm_bindgen(method, js_name = updateData)]
    pub fn update_data(this: &GeoJsonSource, diff: &JsValue);
}

// ==================== Clone impls ====================
// wasm_bindgen extern types don't auto-derive Clone. Each wraps a JsValue
// handle to the same JS object — cloning creates a new Rust handle, not
// a deep copy.

impl Clone for Map {
    fn clone(&self) -> Self {
        let js: &JsValue = self.as_ref();
        js.clone().unchecked_into()
    }
}

impl Clone for GeoJsonSource {
    fn clone(&self) -> Self {
        let js: &JsValue = self.as_ref();
        js.clone().unchecked_into()
    }
}

// ==================== Map construction ====================

/// Options for constructing a MapLibre map.
#[derive(Serialize)]
pub struct MapOptions<'a> {
    /// The style URL (e.g., `"https://tiles.openfreemap.org/styles/liberty"`).
    pub style: &'a str,
    /// Initial center as `[longitude, latitude]`.
    pub center: [f64; 2],
    /// Initial zoom level.
    pub zoom: f64,
}

/// Create a new MapLibre map in the given container element.
///
/// Returns `None` if `maplibregl` is not loaded (e.g., script tag failed to
/// load) or the constructor throws for any other reason.
pub fn create_map(container: &web_sys::HtmlDivElement, options: &MapOptions<'_>) -> Option<Map> {
    let opts = match crate::components::map::to_js(options) {
        Ok(v) => v,
        Err(e) => {
            web_sys::console::error_1(&format!("Failed to serialize map options: {e}").into());
            return None;
        }
    };

    // `container` is a DOM element, not serializable — set it on the JS object directly.
    if let Err(e) = js_sys::Reflect::set(&opts, &"container".into(), container) {
        web_sys::console::error_1(&format!("Failed to set container on map options: {e:?}").into());
        return None;
    }

    match Map::new(&opts) {
        Ok(map) => Some(map),
        Err(e) => {
            web_sys::console::error_1(&format!("maplibregl.Map constructor failed: {e:?}").into());
            None
        }
    }
}

// ==================== Cursor helper ====================

/// Set the cursor style on the map's canvas element.
pub fn set_cursor(map: &Map, cursor: &str) {
    let canvas = map.get_canvas();
    canvas.style().set_property("cursor", cursor).ok();
}

// ==================== Viewport bounds ====================

/// Wrap a longitude into `[-180, 180]`.
///
/// **The inversion this can produce is load-bearing.** When the map wraps
/// around the antimeridian, MapLibre reports e.g. `west = 170, east = 190`.
/// After wrapping, that becomes `(170, -170)` — i.e. `west > east`.
///
/// Our [`Viewport`] convention treats `min_lon > max_lon` as "this viewport
/// crosses the antimeridian", and the API + DB queries already branch on
/// that. So the wrap result feeds straight through correctly without any
/// special-case glue. Don't "fix" the inversion here without also
/// teaching the entity/cluster query code to ignore the flag.
///
/// [`Viewport`]: chronoscope_core::geo::Viewport
fn wrap_lon(lon: f64) -> f64 {
    ((lon + 180.0).rem_euclid(360.0)) - 180.0
}

/// Get the map's viewport bounds as `(west, south, east, north)`,
/// with longitudes normalized to `[-180, 180]`. See [`wrap_lon`] for the
/// load-bearing antimeridian behavior.
pub fn get_viewport_bounds(map: &Map) -> (f64, f64, f64, f64) {
    let bounds = map.get_bounds();
    (
        wrap_lon(bounds.west()),
        bounds.south().clamp(-90.0, 90.0),
        wrap_lon(bounds.east()),
        bounds.north().clamp(-90.0, 90.0),
    )
}

// (No unit tests for `wrap_lon` here: the `web` crate is bin-only and
//  brings in wasm-bindgen extern declarations that don't compile on the
//  host test target. The behavior is pinned by the antimeridian
//  integration tests in `dev/tests/web.rs`.)
