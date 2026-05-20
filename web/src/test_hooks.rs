//! Browser test hooks compiled into the WASM binary behind `cfg(feature = "test-hooks")`.
//!
//! Split into two registration phases:
//! - `register_base()` — called from `App`, provides DOM helpers and wait hooks
//!   that work on all routes (`window.__test` existing signals WASM boot).
//! - `register_map_hooks()` — called from `Landing`, adds map queries, actions,
//!   API client swapping, and map-specific wait hooks.
//!
//! ## Philosophy
//!
//! **Interactions** (click, type, navigate) should use real DOM events — like a
//! real user would produce. The hooks help *locate* elements and verify
//! visibility, but the actual interaction should be a genuine browser event.
//!
//! **Waits** (map idle, fetch complete, transitions) are fine to observe via
//! internal signals, since there's no clean black-box way to know when async
//! operations finish.
//!
//! **Queries** (marker count, cursor style) are fine to read via internal access
//! since we're just observing state, not driving behavior.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::api::Client;
use crate::components::map::{FETCH_COMPLETE_EVENT, MAP_READY_EVENT, current_fetch_settled};
use crate::maplibre;

// ==================== Hook storage ====================
//
// Closures registered on `window.__test` must outlive the registration call,
// because JS holds the only reference to them. The previous version used
// `mem::forget`, which is correct on the happy path but means re-registering
// a hook (e.g., when `register_map_hooks` runs again after a hot-reload or
// re-mount) leaks the prior closure forever and any in-flight call to it
// targets a freed handler if the new registration replaces the JS-side ref.
//
// Storing closures in a `thread_local` map keyed by hook name lets a
// re-registration deterministically drop the old `Closure` (which severs
// its JS-side function reference). Even if the code path that triggers
// re-registration changes in the future, hook lifetime stays sound.
thread_local! {
    static REGISTERED_HOOKS: RefCell<HashMap<&'static str, Box<dyn std::any::Any>>> =
        RefCell::new(HashMap::new());
}

// ==================== Public API ====================

/// Phase 1: register route-independent hooks on `window.__test`.
///
/// Idempotent: if `__test` already exists (e.g., re-mount during HMR),
/// reuses the existing object so previously registered hooks aren't lost.
pub fn register_base() {
    let Some(window) = web_sys::window() else {
        return;
    };
    let obj = js_sys::Reflect::get(&window, &"__test".into())
        .ok()
        .and_then(|v| v.dyn_into::<js_sys::Object>().ok())
        .unwrap_or_else(js_sys::Object::new);

    // DOM interaction
    register(
        &obj,
        "clickVisible",
        Closure::<dyn Fn(String) -> js_sys::Promise>::new(click_visible),
    );
    register(
        &obj,
        "dispatchError",
        Closure::<dyn Fn(String)>::new(dispatch_error),
    );

    // Wait hooks (listen for DOM events from the app)
    register(
        &obj,
        "waitForMapExists",
        Closure::<dyn Fn() -> js_sys::Promise>::new(wait_for_map_exists),
    );
    // Fetch readiness: tests sample `currentFetchSettled()` before triggering
    // an action, then await `waitForFetchSettledAfter(prev)` to wake on the
    // fetch they just caused (not an unrelated in-flight one).
    register(
        &obj,
        "currentFetchSettled",
        Closure::<dyn Fn() -> f64>::new(|| current_fetch_settled() as f64),
    );
    register(
        &obj,
        "waitForFetchSettledAfter",
        Closure::<dyn Fn(f64) -> js_sys::Promise>::new(|min| {
            wait_for_fetch_settled_after(min as u64)
        }),
    );

    let _ = js_sys::Reflect::set(&window, &"__test".into(), &obj);
}

/// Phase 2: register map + API hooks on the existing `window.__test`.
pub fn register_map_hooks(
    map_handle: Rc<RefCell<Option<maplibre::Map>>>,
    api_client: Rc<RefCell<Option<Client>>>,
) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let obj = js_sys::Reflect::get(&window, &"__test".into())
        .ok()
        .and_then(|v| v.dyn_into::<js_sys::Object>().ok())
        .unwrap_or_else(js_sys::Object::new);

    // Map queries (curried: capture the handle, expose a no-arg function)
    let h = map_handle.clone();
    register(
        &obj,
        "mapExists",
        Closure::<dyn Fn() -> bool>::new(move || h.borrow().is_some()),
    );

    let h = map_handle.clone();
    register(
        &obj,
        "mapIsIdle",
        Closure::<dyn Fn() -> bool>::new(move || {
            with_map(&h, |m| m.is_style_loaded() && !m.is_moving()).unwrap_or(false)
        }),
    );

    let h = map_handle.clone();
    register(
        &obj,
        "mapCursor",
        Closure::<dyn Fn() -> JsValue>::new(move || {
            with_map(&h, |m| {
                JsValue::from_str(
                    &m.get_canvas()
                        .style()
                        .get_property_value("cursor")
                        .unwrap_or_default(),
                )
            })
            .unwrap_or_else(|| JsValue::from_str(""))
        }),
    );

    let h = map_handle.clone();
    register(
        &obj,
        "markerCount",
        Closure::<dyn Fn() -> f64>::new(move || with_map(&h, marker_count).unwrap_or(0.0)),
    );

    let h = map_handle.clone();
    register(
        &obj,
        "thumbnailMarkerCount",
        Closure::<dyn Fn() -> f64>::new(move || {
            with_map(&h, thumbnail_marker_count).unwrap_or(0.0)
        }),
    );

    let h = map_handle.clone();
    register(
        &obj,
        "markerProperties",
        Closure::<dyn Fn() -> JsValue>::new(move || {
            with_map(&h, marker_properties).unwrap_or(JsValue::NULL)
        }),
    );

    let h = map_handle.clone();
    register(
        &obj,
        "layerOrder",
        Closure::<dyn Fn() -> JsValue>::new(move || {
            with_map(&h, layer_order).unwrap_or(JsValue::NULL)
        }),
    );

    let h = map_handle.clone();
    register(
        &obj,
        "getZoom",
        Closure::<dyn Fn() -> f64>::new(move || with_map(&h, |m| m.get_zoom_raw()).unwrap_or(0.0)),
    );

    let h = map_handle.clone();
    register(
        &obj,
        "getCenter",
        Closure::<dyn Fn() -> JsValue>::new(move || {
            with_map(&h, |m| {
                let center = m.get_center();
                let arr = js_sys::Array::new();
                arr.push(&center.lng().into());
                arr.push(&center.lat().into());
                JsValue::from(arr)
            })
            .unwrap_or(JsValue::NULL)
        }),
    );

    // Map actions
    let h = map_handle.clone();
    register(
        &obj,
        "jumpTo",
        Closure::<dyn Fn(f64, f64, f64)>::new(move |lng: f64, lat: f64, zoom: f64| {
            with_map(&h, |m| jump_to(m, lng, lat, zoom));
        }),
    );

    let h = map_handle.clone();
    register(
        &obj,
        "fireMapClick",
        Closure::<dyn Fn(f64, f64)>::new(move |lng: f64, lat: f64| {
            with_map(&h, |m| fire_map_click(m, lng, lat));
        }),
    );

    let h = map_handle.clone();
    register(
        &obj,
        "fireCanvasMousemove",
        Closure::<dyn Fn(f64, f64)>::new(move |lng: f64, lat: f64| {
            with_map(&h, |m| fire_canvas_mousemove(m, lng, lat));
        }),
    );

    // API client
    let c = api_client;
    register(
        &obj,
        "setApiUrl",
        Closure::<dyn Fn(String)>::new(move |url: String| {
            *c.borrow_mut() = Some(Client::new(url));
        }),
    );

    // Map wait hooks
    let h = map_handle;
    register(
        &obj,
        "waitForMapIdle",
        Closure::<dyn Fn() -> js_sys::Promise>::new(move || wait_for_map_idle(&h)),
    );

    register(
        &obj,
        "waitForThumbnailsLoaded",
        Closure::<dyn Fn() -> js_sys::Promise>::new(wait_for_thumbnails_loaded),
    );

    let _ = js_sys::Reflect::set(&window, &"__test".into(), &obj);
}

// ==================== Registration helper ====================

/// Register a wasm_bindgen Closure on a JS object.
///
/// The closure is stored in `REGISTERED_HOOKS` keyed by `name`. Re-registering
/// the same name drops the prior closure (deterministically severing its
/// JS-side reference) before installing the new one — so HMR or re-mount
/// can't leave dangling handlers.
fn register<C>(obj: &js_sys::Object, name: &'static str, closure: C)
where
    C: AsRef<JsValue> + 'static,
{
    if js_sys::Reflect::set(obj, &name.into(), closure.as_ref()).is_err() {
        web_sys::console::warn_1(&format!("failed to set test hook: {name}").into());
    }
    REGISTERED_HOOKS.with(|hooks| {
        // Inserting drops the prior Box<dyn Any> if one exists, which drops
        // the inner Closure and frees its JS-side function reference.
        hooks.borrow_mut().insert(name, Box::new(closure));
    });
}

// ==================== Map access helper ====================

/// Run a function with the map if it exists, avoiding verbose borrow chains.
fn with_map<R>(
    handle: &Rc<RefCell<Option<maplibre::Map>>>,
    f: impl FnOnce(&maplibre::Map) -> R,
) -> Option<R> {
    handle.borrow().as_ref().map(f)
}

// ==================== Visibility check ====================

// ==================== DOM interaction hooks ====================

/// Click the first visible element matching a selector. Returns a Promise
/// that resolves after the click and any resulting CSS transition.
///
/// Uses `offsetParent` to skip hidden elements (e.g., desktop sidebar links
/// at mobile viewport) and a 250ms fallback timeout to handle clicks that
/// don't trigger CSS transitions.
fn click_visible(selector: String) -> js_sys::Promise {
    let mut selector = Some(selector);
    js_sys::Promise::new(&mut |resolve, reject| {
        let Some(selector) = selector.take() else {
            return;
        };

        // Find and click the first visible element
        let clicked_el: Option<web_sys::HtmlElement> = (|| {
            let doc = web_sys::window()?.document()?;
            let nodes = doc.query_selector_all(&selector).ok()?;
            for i in 0..nodes.length() {
                let el: web_sys::HtmlElement = nodes.item(i)?.dyn_into().ok()?;
                if el.offset_parent().is_some() || el.offset_width() > 0 {
                    el.click();
                    return Some(el);
                }
            }
            None
        })();

        let Some(el) = clicked_el else {
            let _ = reject.call1(
                &JsValue::NULL,
                &JsValue::from_str("no visible element found"),
            );
            return;
        };

        // Wait for any CSS transition on the clicked element, with a 250ms
        // fallback for clicks that don't trigger transitions. Both the
        // transitionend listener and the timeout race to resolve the promise;
        // the `done` flag ensures only the first one wins.
        let done = Rc::new(Cell::new(false));
        let resolve = Rc::new(resolve);

        let resolve_once = |done: &Rc<Cell<bool>>, resolve: &Rc<js_sys::Function>| {
            if !done.get() {
                done.set(true);
                let _ = resolve.call0(&JsValue::NULL);
            }
        };

        // Race participant 1: transitionend event on the clicked element
        let d = done.clone();
        let r = resolve.clone();
        let on_transition = Closure::once(move || resolve_once(&d, &r));
        let opts = web_sys::AddEventListenerOptions::new();
        opts.set_once(true);
        let _ = el.add_event_listener_with_callback_and_add_event_listener_options(
            "transitionend",
            on_transition.as_ref().unchecked_ref(),
            &opts,
        );
        on_transition.forget();

        // Race participant 2: 250ms timeout (covers clicks with no transition)
        let d = done;
        let r = resolve;
        let on_timeout = Closure::once(move || resolve_once(&d, &r));
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                on_timeout.as_ref().unchecked_ref(),
                250,
            );
        }
        on_timeout.forget();
    })
}

/// Dispatch a `chronoscope-error` CustomEvent on the window.
fn dispatch_error(msg: String) {
    if let Some(window) = web_sys::window() {
        let init = web_sys::CustomEventInit::new();
        init.set_detail(&JsValue::from_str(&msg));
        if let Ok(event) =
            web_sys::CustomEvent::new_with_event_init_dict("chronoscope-error", &init)
        {
            let _ = window.dispatch_event(&event);
        }
    }
}

// ==================== Wait hooks ====================

/// Returns a Promise that resolves when `chronoscope-map-ready` fires.
fn wait_for_map_exists() -> js_sys::Promise {
    js_sys::Promise::new(&mut |resolve, _reject| {
        // Check if map hooks are already registered
        if let Some(window) = web_sys::window() {
            if let Ok(test_obj) = js_sys::Reflect::get(&window, &"__test".into()) {
                if js_sys::Reflect::get(&test_obj, &"mapExists".into())
                    .ok()
                    .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
                    .and_then(|f| f.call0(&JsValue::NULL).ok())
                    .and_then(|v| v.as_bool())
                    == Some(true)
                {
                    let _ = resolve.call0(&JsValue::NULL);
                    return;
                }
            }
        }
        listen_once_and_resolve(MAP_READY_EVENT, resolve);
    })
}

/// Resolves a Promise once `FETCH_SETTLED` has advanced past `min` — i.e. at
/// least one fetch has settled since the caller sampled the counter. Resolves
/// immediately if the counter has already advanced.
///
/// The listener handles its own detach: it keeps the JS-side `Function`
/// reference (a thin `JsValue` handle) rather than the owning `Closure`, then
/// calls `remove_event_listener` with that reference once a matching event
/// arrives. The `Closure` itself is `forget`'d so JS owns it for the listener's
/// lifetime; after detach JS GC can free it. This avoids the
/// drop-the-closure-from-inside-itself UAF that an `Rc<Cell<Option<Closure>>>`
/// self-capture would invite.
fn wait_for_fetch_settled_after(min: u64) -> js_sys::Promise {
    js_sys::Promise::new(&mut |resolve, _reject| {
        if current_fetch_settled() > min {
            let _ = resolve.call0(&JsValue::NULL);
            return;
        }
        let Some(window) = web_sys::window() else {
            let _ = resolve.call0(&JsValue::NULL);
            return;
        };
        let resolve_cell: Rc<Cell<Option<js_sys::Function>>> = Rc::new(Cell::new(Some(resolve)));
        let listener_fn: Rc<Cell<Option<js_sys::Function>>> = Rc::new(Cell::new(None));
        let r = resolve_cell.clone();
        let f = listener_fn.clone();
        let window_for_cb = window.clone();
        let cb = Closure::<dyn FnMut(JsValue)>::new(move |event: JsValue| {
            let detail_ok = event
                .dyn_ref::<web_sys::CustomEvent>()
                .and_then(|ce| ce.detail().as_f64())
                .is_some_and(|d| (d as u64) > min);
            if !detail_ok {
                return;
            }
            if let Some(func) = f.take() {
                let _ =
                    window_for_cb.remove_event_listener_with_callback(FETCH_COMPLETE_EVENT, &func);
            }
            if let Some(resolve_fn) = r.take() {
                let _ = resolve_fn.call0(&JsValue::NULL);
            }
        });
        let func: js_sys::Function = cb.as_ref().clone().unchecked_into();
        let _ = window.add_event_listener_with_callback(FETCH_COMPLETE_EVENT, &func);
        listener_fn.set(Some(func));
        cb.forget();
    })
}

/// Returns a Promise that resolves on the next `chronoscope-thumbnails-loaded`.
fn wait_for_thumbnails_loaded() -> js_sys::Promise {
    use crate::components::map::THUMBNAILS_LOADED_EVENT;
    js_sys::Promise::new(&mut |resolve, _reject| {
        listen_once_and_resolve(THUMBNAILS_LOADED_EVENT, resolve);
    })
}

/// Returns a Promise that resolves when the map becomes idle.
fn wait_for_map_idle(handle: &Rc<RefCell<Option<maplibre::Map>>>) -> js_sys::Promise {
    let mut map = handle.borrow().as_ref().cloned();
    js_sys::Promise::new(&mut |resolve, _reject| {
        let Some(map) = map.take() else {
            let _ = resolve.call0(&JsValue::NULL);
            return;
        };

        // Register listener before checking — avoids the race where the map
        // becomes idle between our check and the listener registration.
        let already_resolved = Rc::new(Cell::new(false));
        let resolve = Rc::new(resolve);

        let flag = already_resolved.clone();
        let res = resolve.clone();
        let cb = Closure::once(move || {
            if !flag.get() {
                flag.set(true);
                let _ = res.call0(&JsValue::NULL);
            }
        });
        map.once("idle", cb.as_ref());
        cb.forget(); // Transferred to MapLibre's event system

        if map.is_style_loaded() && !map.is_moving() && !already_resolved.get() {
            already_resolved.set(true);
            let _ = resolve.call0(&JsValue::NULL);
        }
    })
}

/// Helper: register a one-shot DOM event listener that resolves a Promise.
fn listen_once_and_resolve(event_name: &str, resolve: js_sys::Function) {
    let Some(window) = web_sys::window() else {
        let _ = resolve.call0(&JsValue::NULL);
        return;
    };
    let cb = Closure::once(move || {
        let _ = resolve.call0(&JsValue::NULL);
    });
    let opts = web_sys::AddEventListenerOptions::new();
    opts.set_once(true);
    let _ = window.add_event_listener_with_callback_and_add_event_listener_options(
        event_name,
        cb.as_ref().unchecked_ref(),
        &opts,
    );
    cb.forget(); // Transferred to the browser's event system
}

// ==================== Map helpers ====================

/// Count rendered markers across both the circle and thumbnail layers.
///
/// No dedup is needed here (unlike `marker_properties`) because the circle
/// and thumbnail layers have mutually exclusive filters: circles use
/// `["!", ["has", "thumbnail"]]` and thumbnails use `["has", "thumbnail"]`,
/// so a given feature only appears in one layer at a time.
fn marker_count(map: &maplibre::Map) -> f64 {
    use crate::components::map::{ENTITY_CIRCLES_LAYER, ENTITY_THUMBNAILS_LAYER};
    let opts = js_sys::Object::new();
    let layers = js_sys::Array::new();
    layers.push(&ENTITY_CIRCLES_LAYER.into());
    layers.push(&ENTITY_THUMBNAILS_LAYER.into());
    let _ = js_sys::Reflect::set(&opts, &"layers".into(), &layers);
    map.query_rendered_features(&JsValue::UNDEFINED, &opts)
        .length() as f64
}

fn thumbnail_marker_count(map: &maplibre::Map) -> f64 {
    use crate::components::map::ENTITY_THUMBNAILS_LAYER;
    let opts = js_sys::Object::new();
    let layers = js_sys::Array::new();
    layers.push(&ENTITY_THUMBNAILS_LAYER.into());
    let _ = js_sys::Reflect::set(&opts, &"layers".into(), &layers);
    map.query_rendered_features(&JsValue::UNDEFINED, &opts)
        .length() as f64
}

/// Return a JS array of marker descriptors for all rendered markers.
/// Each entry contains the feature `properties` plus `_lng`/`_lat` from
/// the feature geometry, so tests can both assert on properties and
/// click on the actual rendered coordinates.
fn marker_properties(map: &maplibre::Map) -> JsValue {
    use crate::components::map::{ENTITY_CIRCLES_LAYER, ENTITY_THUMBNAILS_LAYER};
    let opts = js_sys::Object::new();
    let layers = js_sys::Array::new();
    layers.push(&ENTITY_CIRCLES_LAYER.into());
    layers.push(&ENTITY_THUMBNAILS_LAYER.into());
    let _ = js_sys::Reflect::set(&opts, &"layers".into(), &layers);
    let features = map.query_rendered_features(&JsValue::UNDEFINED, &opts);

    // Some markers (those with thumbnails) appear in both the circle and
    // thumbnail layers — dedupe by `feature.id`. The source is configured
    // with `promoteId: "feature_id"`, so MapLibre uses the stable string
    // ID from the feature_id property (entity UUID or "cluster-{osm_id}").
    // If it's ever missing, that's a bug worth surfacing.
    let seen = js_sys::Set::new(&JsValue::UNDEFINED);
    let result = js_sys::Array::new();
    for i in 0..features.length() {
        let feature = features.get(i);
        let id = js_sys::Reflect::get(&feature, &"id".into()).unwrap_or(JsValue::UNDEFINED);
        if id.is_undefined() || id.is_null() {
            web_sys::console::warn_1(
                &"marker_properties: feature missing id (generateId not set?)".into(),
            );
            continue;
        }
        if seen.has(&id) {
            continue;
        }
        seen.add(&id);

        // Build the descriptor: properties + _lng/_lat from geometry
        let Ok(props) = js_sys::Reflect::get(&feature, &"properties".into()) else {
            continue;
        };
        // Clone the properties object so we don't mutate MapLibre's internal state
        let descriptor = js_sys::Object::new();
        if let Ok(keys) = js_sys::Reflect::own_keys(&props) {
            for j in 0..keys.length() {
                let k = keys.get(j);
                if let Ok(v) = js_sys::Reflect::get(&props, &k) {
                    let _ = js_sys::Reflect::set(&descriptor, &k, &v);
                }
            }
        }
        if let Ok(geometry) = js_sys::Reflect::get(&feature, &"geometry".into())
            && let Ok(coords) = js_sys::Reflect::get(&geometry, &"coordinates".into())
            && let Some(coords_arr) = coords.dyn_ref::<js_sys::Array>()
        {
            let _ = js_sys::Reflect::set(&descriptor, &"_lng".into(), &coords_arr.get(0));
            let _ = js_sys::Reflect::set(&descriptor, &"_lat".into(), &coords_arr.get(1));
        }
        result.push(&descriptor);
    }
    result.into()
}

/// Return a JS array of layer IDs in the order MapLibre will draw them
/// (bottom to top). Used by the layer-order regression test to lock in
/// the rule that thumbnails draw above labels (so labels for one
/// region's centroid never occlude another region's thumbnail).
fn layer_order(map: &maplibre::Map) -> JsValue {
    let style = map.get_style();
    let Ok(layers) = js_sys::Reflect::get(&style, &"layers".into()) else {
        return JsValue::NULL;
    };
    let Ok(layers) = layers.dyn_into::<js_sys::Array>() else {
        return JsValue::NULL;
    };
    let result = js_sys::Array::new();
    for i in 0..layers.length() {
        let layer = layers.get(i);
        if let Ok(id) = js_sys::Reflect::get(&layer, &"id".into()) {
            result.push(&id);
        }
    }
    result.into()
}

fn jump_to(map: &maplibre::Map, lng: f64, lat: f64, zoom: f64) {
    let opts = js_sys::Object::new();
    let center = js_sys::Array::new();
    center.push(&lng.into());
    center.push(&lat.into());
    let _ = js_sys::Reflect::set(&opts, &"center".into(), &center);
    let _ = js_sys::Reflect::set(&opts, &"zoom".into(), &zoom.into());
    map.jump_to_raw(&opts);
}

fn project_lnglat(map: &maplibre::Map, lng: f64, lat: f64) -> JsValue {
    let arr = js_sys::Array::new();
    arr.push(&lng.into());
    arr.push(&lat.into());
    map.project(&arr)
}

/// Synchronously fire a `click` event on the map at the given lng/lat.
///
/// Callers that drive this from a test must first ensure the map's render
/// pipeline is settled (`window.__test.waitForMapIdle()` plus two
/// `requestAnimationFrame` calls). MapLibre's `Map.fire("click")` calls
/// `queryRenderedFeatures` internally to dispatch layer-specific click
/// events, and that path will throw "feature index out of bounds" if a
/// recent `setData`/`updateData` hasn't been fully indexed yet. The wait
/// chain is expressed JS-side in `click_map_at` rather than baked into
/// this hook so the read-after-mutate guarantees stay visible at the call
/// site.
fn fire_map_click(map: &maplibre::Map, lng: f64, lat: f64) {
    let point = project_lnglat(map, lng, lat);

    let data = js_sys::Object::new();
    let lnglat_obj = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&lnglat_obj, &"lng".into(), &lng.into());
    let _ = js_sys::Reflect::set(&lnglat_obj, &"lat".into(), &lat.into());
    let _ = js_sys::Reflect::set(&data, &"lngLat".into(), &lnglat_obj);
    let _ = js_sys::Reflect::set(&data, &"point".into(), &point);

    let orig = js_sys::Object::new();
    let noop = Closure::<dyn Fn()>::new(|| {}).into_js_value();
    let _ = js_sys::Reflect::set(&orig, &"preventDefault".into(), &noop);
    let _ = js_sys::Reflect::set(&orig, &"stopPropagation".into(), &noop);
    let _ = js_sys::Reflect::set(&data, &"originalEvent".into(), &orig);

    map.fire("click", &data);
}

fn fire_canvas_mousemove(map: &maplibre::Map, lng: f64, lat: f64) {
    let point = project_lnglat(map, lng, lat);
    let canvas = map.get_canvas();
    let rect = canvas.get_bounding_client_rect();

    let client_x = js_sys::Reflect::get(&point, &"x".into())
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
        + rect.left();
    let client_y = js_sys::Reflect::get(&point, &"y".into())
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
        + rect.top();

    let mut init = web_sys::MouseEventInit::new();
    init.client_x(client_x as i32);
    init.client_y(client_y as i32);
    init.bubbles(true);

    if let Ok(event) = web_sys::MouseEvent::new_with_mouse_event_init_dict("mousemove", &init) {
        let _ = canvas.dispatch_event(&event);
    }
}

// ==================== MapLibre bindings (test-only extensions) ====================

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(method, js_class = "Map", js_name = isMoving)]
    fn is_moving(this: &maplibre::Map) -> bool;

    #[wasm_bindgen(method, js_class = "Map", js_name = jumpTo)]
    fn jump_to_raw(this: &maplibre::Map, options: &JsValue);

    #[wasm_bindgen(method, js_class = "Map")]
    fn project(this: &maplibre::Map, lnglat: &JsValue) -> JsValue;

    #[wasm_bindgen(method, js_class = "Map")]
    fn fire(this: &maplibre::Map, event_type: &str, data: &JsValue) -> JsValue;

    #[wasm_bindgen(method, js_class = "Map", js_name = getStyle)]
    fn get_style(this: &maplibre::Map) -> JsValue;
}
