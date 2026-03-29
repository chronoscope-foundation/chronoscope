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

    /// Get a source by ID. Returns `undefined` if not found.
    #[wasm_bindgen(method, js_name = getSource)]
    pub fn get_source(this: &Map, id: &str) -> Option<GeoJsonSource>;

    /// Get the map's bounds as a `LngLatBounds` object.
    #[wasm_bindgen(method, js_name = getBounds)]
    pub fn get_bounds(this: &Map) -> LngLatBounds;

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
    let serializer = serde_wasm_bindgen::Serializer::json_compatible();
    let opts = match options.serialize(&serializer) {
        Ok(v) => v,
        Err(e) => {
            web_sys::console::error_1(
                &format!("Failed to serialize map options: {e}").into(),
            );
            return None;
        }
    };

    // `container` is a DOM element, not serializable — set it on the JS object directly.
    if let Err(e) = js_sys::Reflect::set(&opts, &"container".into(), container) {
        web_sys::console::error_1(
            &format!("Failed to set container on map options: {e:?}").into(),
        );
        return None;
    }

    match Map::new(&opts) {
        Ok(map) => Some(map),
        Err(e) => {
            web_sys::console::error_1(
                &format!("maplibregl.Map constructor failed: {e:?}").into(),
            );
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

/// Wrap a longitude into [-180, 180].
fn wrap_lon(lon: f64) -> f64 {
    ((lon + 180.0).rem_euclid(360.0)) - 180.0
}

/// Get the map's viewport bounds as `(west, south, east, north)`,
/// with longitudes normalized to `[-180, 180]`.
///
/// MapLibre reports bounds beyond `[-180, 180]` when the map wraps around
/// the world. This is a MapLibre display quirk, not a domain concern, so
/// we normalize here at the boundary.
pub fn get_viewport_bounds(map: &Map) -> (f64, f64, f64, f64) {
    let bounds = map.get_bounds();
    (
        wrap_lon(bounds.west()),
        bounds.south().clamp(-90.0, 90.0),
        wrap_lon(bounds.east()),
        bounds.north().clamp(-90.0, 90.0),
    )
}
