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
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::api::Client;
use crate::components::map::{FETCH_COMPLETE_EVENT, MAP_READY_EVENT};
use crate::maplibre;

// ==================== Public API ====================

/// Phase 1: register route-independent hooks on `window.__test`.
pub fn register_base() {
    let Some(window) = web_sys::window() else {
        return;
    };
    let obj = js_sys::Object::new();

    // DOM interaction
    register(
        &obj,
        "clickVisible",
        Closure::<dyn Fn(String) -> JsValue>::new(click_visible),
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
        Closure::<dyn Fn() -> JsValue>::new(wait_for_map_exists),
    );
    register(
        &obj,
        "waitForFetchComplete",
        Closure::<dyn Fn() -> JsValue>::new(wait_for_fetch_complete),
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
        Closure::<dyn Fn() -> JsValue>::new(move || wait_for_map_idle(&h)),
    );

    let _ = js_sys::Reflect::set(&window, &"__test".into(), &obj);
}

// ==================== Registration helper ====================

/// Register a wasm_bindgen Closure on a JS object. Consumes the closure
/// via `into_js_value()` which transfers ownership to JS GC — no `forget()`
/// or manual leak management needed.
fn register(obj: &js_sys::Object, name: &str, closure: impl AsRef<JsValue>) {
    if js_sys::Reflect::set(obj, &name.into(), closure.as_ref()).is_err() {
        web_sys::console::warn_1(&format!("failed to set test hook: {name}").into());
    }
    // Intentionally leak — the closure must outlive this scope since JS holds
    // a reference to it. This is the standard wasm-bindgen pattern.
    std::mem::forget(closure);
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
fn click_visible(selector: String) -> JsValue {
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
    .into()
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
fn wait_for_map_exists() -> JsValue {
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
    .into()
}

/// Returns a Promise that resolves on the next `chronoscope-fetch-complete`.
fn wait_for_fetch_complete() -> JsValue {
    js_sys::Promise::new(&mut |resolve, _reject| {
        listen_once_and_resolve(FETCH_COMPLETE_EVENT, resolve);
    })
    .into()
}

/// Returns a Promise that resolves when the map becomes idle.
fn wait_for_map_idle(handle: &Rc<RefCell<Option<maplibre::Map>>>) -> JsValue {
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
    .into()
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

fn marker_count(map: &maplibre::Map) -> f64 {
    let opts = js_sys::Object::new();
    let layers = js_sys::Array::new();
    layers.push(&"entity-circles".into());
    let _ = js_sys::Reflect::set(&opts, &"layers".into(), &layers);
    map.query_rendered_features(&JsValue::UNDEFINED, &opts)
        .length() as f64
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

    #[wasm_bindgen(method, js_class = "Map")]
    fn once(this: &maplibre::Map, event_type: &str, callback: &JsValue);
}
