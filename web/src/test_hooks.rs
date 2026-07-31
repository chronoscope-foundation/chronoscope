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
use crate::components::focus_trap::is_rendered;
use crate::components::map::{FETCH_COMPLETE_EVENT, MAP_READY_EVENT, current_fetch_settled};
use crate::maplibre;
use crate::time_scale::{TimeScale, era_label};

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

// ==================== Hook registration ====================

/// Trait-based dispatch so `func.register_on(&obj, "name")` picks the right
/// `Closure::<dyn Fn(...) -> R>` from the function's own signature — no need
/// to spell the arg/return types again at the call site.
///
/// The `Args` type parameter is a marker tuple that lets us define one impl
/// per arity without Rust treating them as overlapping (the axum/actix-web
/// handler pattern). Callers never name `Args` — it's inferred from the
/// function's `Fn` impl.
trait RegisterFn<Args>: 'static {
    fn register_on(self, obj: &js_sys::Object, name: &'static str);
}

macro_rules! impl_register_fn {
    ($($A:ident),*) => {
        impl<F, R, $($A,)*> RegisterFn<($($A,)*)> for F
        where
            F: Fn($($A,)*) -> R + 'static,
            $($A: wasm_bindgen::convert::FromWasmAbi + 'static,)*
            R: wasm_bindgen::convert::ReturnWasmAbi + 'static,
        {
            fn register_on(self, obj: &js_sys::Object, name: &'static str) {
                register(obj, name, Closure::<dyn Fn($($A,)*) -> R>::new(self));
            }
        }
    };
}

impl_register_fn!();
impl_register_fn!(A);
impl_register_fn!(A, B);
impl_register_fn!(A, B, C);
impl_register_fn!(A, B, C, D);

// ==================== Hook registration macro ====================

/// Register one or more hooks on a JS object. Each entry is either:
///
/// - `name` — registers the free fn `name` under `stringify!(name)`.
/// - `name: expr` — registers the expression (a closure, typically) under
///   `stringify!(name)`. Used for hooks that need to adapt arg/return types
///   across the FFI boundary.
///
/// The Rust function/closure must satisfy `RegisterFn<Args>` (one impl per
/// arity, 0..=3). The hook name on the wire is always
/// `stringify!(name)` — no second string to keep in sync.
macro_rules! register_hooks {
    ($obj:expr, [$($body:tt)*]) => {
        register_hooks!(@munch $obj, $($body)*);
    };
    (@munch $obj:expr $(,)?) => {};

    // name: expr (with trailing comma)
    (@munch $obj:expr, $name:ident: $val:expr, $($rest:tt)*) => {
        ($val).register_on($obj, stringify!($name));
        register_hooks!(@munch $obj, $($rest)*);
    };
    // name: expr (final entry, no trailing comma)
    (@munch $obj:expr, $name:ident: $val:expr) => {
        ($val).register_on($obj, stringify!($name));
    };

    // name (with trailing comma) — must come after the colon arms so the
    // parser tries them first (greedy on the colon).
    (@munch $obj:expr, $name:ident, $($rest:tt)*) => {
        $name.register_on($obj, stringify!($name));
        register_hooks!(@munch $obj, $($rest)*);
    };
    // name (final entry, no trailing comma)
    (@munch $obj:expr, $name:ident) => {
        $name.register_on($obj, stringify!($name));
    };
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
        .unwrap_or_default();

    register_hooks!(&obj, [
        // DOM interaction (waits internally — safer than the raw primitives)
        click,
        dispatch_error,
        focus_element,
        press_key,

        // DOM queries (each waits where the natural pairing would otherwise
        // be `wait_for_selector` + read).
        text,
        attr,
        body_text,
        is_visible,
        is_active_inside,
        count: |sel: String| f64::from(count_matching(&sel)),
        element_rect: |sel: String| element_rect(&sel),
        is_hittable,
        sample_animation_at,
        slow_animations,
        // Plural: returns the attribute for every match. Used by accessibility
        // tests that need to verify a property over all matches.
        attributes: |sel: String, attr: String| element_attributes(&sel, &attr),
        active_element_attribute: |attr: String| {
            active_element_attribute(&attr)
                .map(JsValue::from)
                .unwrap_or(JsValue::NULL)
        },

        // DOM-mutation waits (MutationObserver-based, eager-check first)
        wait_for_selector,
        wait_for_selector_removal,
        wait_for_body_text,
        wait_for_fonts,
        wait_for_media_query,

        // Fetch-settled counter — sample then await (closes listener race).
        // f64 across the FFI: integer counters fit losslessly under 2^53.
        current_fetch_settled: || current_fetch_settled() as f64,
        wait_for_fetch_settled_after: |prev: f64| {
            wait_for_fetch_settled_after(prev as u64)
        },
    ]);

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
        .unwrap_or_default();

    // Map hooks. `map_query` handles the `let h = clone; move || with_map(&h,
    // …).unwrap_or(default)` boilerplate for the no-arg query family. The
    // composed action hooks (`click_map_at`, `pan_map_to`,
    // `click_and_wait_for_fetch`) bake their internal waits so tests get
    // one CDP roundtrip per logical step.
    register_hooks!(&obj, [
        // Map queries.
        map_cursor: map_query(&map_handle, String::new(), |m| {
            m.get_canvas()
                .style()
                .get_property_value("cursor")
                .unwrap_or_default()
        }),
        marker_count: map_query(&map_handle, 0.0, |m| f64::from(marker_count(m))),
        thumbnail_marker_count: map_query(&map_handle, 0.0, |m| {
            f64::from(thumbnail_marker_count(m))
        }),
        layer_order: map_query(&map_handle, JsValue::NULL, layer_order),
        zoom: map_query(&map_handle, 0.0, |m| m.get_zoom_raw()),
        get_center: map_query(&map_handle, JsValue::NULL, |m| {
            let center = m.get_center();
            let arr = js_sys::Array::new();
            arr.push(&center.lng().into());
            arr.push(&center.lat().into());
            JsValue::from(arr)
        }),
        // Canvas size in CSS pixels `[width, height]` — the frame `project`
        // returns coordinates in, so a test can bound-check a rendered marker's
        // `_x`/`_y` against the visible viewport (the direction-B in-view check).
        map_canvas_size: map_query(&map_handle, JsValue::NULL, |m| {
            let canvas = m.get_canvas();
            let arr = js_sys::Array::new();
            arr.push(&f64::from(canvas.client_width()).into());
            arr.push(&f64::from(canvas.client_height()).into());
            JsValue::from(arr)
        }),

        // Settled-wait baked: both read `query_rendered_features`, which needs
        // the MapLibre feature index ready. `marker_properties` covers the
        // individual-entity layers; `badge_properties` covers the cluster
        // badges (proximity clusters + lone server `Expand` cells).
        marker_properties: {
            let h = map_handle.clone();
            move || descriptors_settled(h.clone(), marker_properties)
        },
        badge_properties: {
            let h = map_handle.clone();
            move || descriptors_settled(h.clone(), badge_properties)
        },
        ring_properties: {
            let h = map_handle.clone();
            move || descriptors_settled(h.clone(), ring_properties)
        },
        ring_sprites_registered: map_query(
            &map_handle,
            false,
            crate::components::map::ring_sprites_registered,
        ),

        // Aiming helpers. A verdict ring reaches past its marker's disc, so a
        // test that wants to land on one has to read the band it occupies, turn
        // that into a screen offset, and ask which layer owns the pixel it
        // reached.
        unevidenced_ring_band: || {
            let (inner, outer) = crate::components::map::unevidenced_ring_band();
            JsValue::from(js_sys::Array::of2(&inner.into(), &outer.into()))
        },
        offset_lnglat: {
            let h = map_handle.clone();
            move |lng: f64, lat: f64, dx: f64, dy: f64| {
                with_map(&h, |m| offset_lnglat(m, lng, lat, dx, dy)).unwrap_or(JsValue::NULL)
            }
        },
        marker_layers_at: {
            let h = map_handle.clone();
            move |lng: f64, lat: f64| marker_layers_at(h.clone(), lng, lat)
        },

        // Composed map actions (settled-wait baked).
        click_map_at: {
            let h = map_handle.clone();
            move |lng: f64, lat: f64| click_map_at(h.clone(), lng, lat)
        },
        // pan_map_to: sample counter → jump → await counter advance → idle.
        // One CDP roundtrip per call (vs. four if composed harness-side).
        pan_map_to: {
            let h = map_handle.clone();
            move |lng: f64, lat: f64, zoom: f64| pan_map_to(h.clone(), lng, lat, zoom)
        },
        // click_and_wait_for_fetch: sample → click (waits for selector) →
        // await counter advance.
        click_and_wait_for_fetch: |selector: String| click_and_wait_for_fetch(selector),
        fire_canvas_mousemove: {
            let h = map_handle.clone();
            move |lng: f64, lat: f64| {
                with_map(&h, |m| fire_canvas_mousemove(m, lng, lat));
            }
        },

        // API client.
        set_api_url: {
            let c = api_client.clone();
            move |url: String| { *c.borrow_mut() = Some(Client::new(url)); }
        },

        // Map wait hook (existence-wait baked in; see `wait_for_map_idle`).
        wait_for_map_idle: {
            let h = map_handle.clone();
            move || wait_for_map_idle(&h)
        },

        // Time slider — set a year and await the refetch it triggers.
        set_time_slider_year: |year: f64| set_time_slider_year(year),

        // Thumbnails loaded — sample-then-await pattern (mirrors fetch-settled).
        current_thumbnails_loaded: || crate::components::map::current_thumbnails_loaded() as f64,
        wait_for_thumbnails_loaded_after: |prev: f64| {
            wait_for_thumbnails_loaded_after(prev as u64)
        },
    ]);

    let _ = js_sys::Reflect::set(&window, &"__test".into(), &obj);
}

// ==================== Registration helper ====================

/// Register a `wasm_bindgen` Closure on a JS object.
///
/// The closure is stored in `REGISTERED_HOOKS` keyed by `name`. Re-registering
/// the same name drops the prior closure (deterministically severing its
/// JS-side reference) before installing the new one — so HMR or re-mount
/// can't leave dangling handlers.
///
/// Call sites use `func.register_on(&obj, "name")` (via the [`RegisterFn`]
/// trait) instead of constructing the `Closure` directly.
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

// ==================== Map access helpers ====================

/// Run a function with the map if it exists, avoiding verbose borrow chains.
fn with_map<R>(
    handle: &Rc<RefCell<Option<maplibre::Map>>>,
    f: impl FnOnce(&maplibre::Map) -> R,
) -> Option<R> {
    handle.borrow().as_ref().map(f)
}

/// Build a no-arg closure that calls `f` with the map (if it exists) and
/// returns `default` otherwise. Collapses the per-hook
/// `let h = map_handle.clone(); move || with_map(&h, ...).unwrap_or(...)`
/// dance into one line per registration.
fn map_query<R: Clone + 'static>(
    handle: &Rc<RefCell<Option<maplibre::Map>>>,
    default: R,
    f: impl Fn(&maplibre::Map) -> R + 'static,
) -> impl Fn() -> R + 'static {
    let h = handle.clone();
    move || {
        h.borrow()
            .as_ref()
            .map(&f)
            .unwrap_or_else(|| default.clone())
    }
}

// ==================== Visibility check ====================

// ==================== DOM interaction hooks ====================

/// Click the first visible element matching a selector. Returns a Promise
/// that resolves after the click and any resulting CSS transition.
///
/// Uses [`is_rendered`] to skip elements the layout has removed (a `display:
/// none` responsive variant) and a 250ms fallback timeout to handle clicks that
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
                if is_rendered(&el) {
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

/// Dispatch a `chronoscope-error` `CustomEvent` on the window.
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

/// Resolves when the map is mounted on the handle. Eager-checks the handle
/// directly (no JS reflection); falls back to `MAP_READY_EVENT` if not yet
/// present.
fn wait_for_map_exists(handle: &Rc<RefCell<Option<maplibre::Map>>>) -> js_sys::Promise {
    let handle = handle.clone();
    js_sys::Promise::new(&mut |resolve, _reject| {
        if handle.borrow().is_some() {
            let _ = resolve.call0(&JsValue::NULL);
            return;
        }
        listen_once_and_resolve(MAP_READY_EVENT, resolve);
    })
}

/// Resolves once `FETCH_SETTLED` has advanced past `min` — at least one
/// fetch has settled since the caller sampled the counter.
fn wait_for_fetch_settled_after(min: u64) -> js_sys::Promise {
    wait_for_counter_after(min, FETCH_COMPLETE_EVENT, current_fetch_settled)
}

/// Resolves once `THUMBNAILS_LOADED` has advanced past `min`.
fn wait_for_thumbnails_loaded_after(min: u64) -> js_sys::Promise {
    use crate::components::map::{THUMBNAILS_LOADED_EVENT, current_thumbnails_loaded};
    wait_for_counter_after(min, THUMBNAILS_LOADED_EVENT, current_thumbnails_loaded)
}

/// Resolves once `current()` advances past `min`. Resolves immediately if
/// the counter is already past. Otherwise listens on `event_name` and
/// re-checks `event.detail` (the post-bump count carried by `CustomEvent`).
///
/// The listener self-detaches: it keeps the JS-side `Function` reference
/// (a thin `JsValue` handle), not the owning `Closure`, then calls
/// `remove_event_listener` with that reference. The `Closure` is `forget`'d
/// so JS owns it for the listener's lifetime; after detach, JS GC can free
/// it. Avoids the drop-the-closure-from-inside-itself UAF that an
/// `Rc<Cell<Option<Closure>>>` self-capture would invite.
fn wait_for_counter_after(
    min: u64,
    event_name: &'static str,
    current: fn() -> u64,
) -> js_sys::Promise {
    js_sys::Promise::new(&mut |resolve, _reject| {
        if current() > min {
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
                let _ = window_for_cb.remove_event_listener_with_callback(event_name, &func);
            }
            if let Some(resolve_fn) = r.take() {
                let _ = resolve_fn.call0(&JsValue::NULL);
            }
        });
        let func: js_sys::Function = cb.as_ref().clone().unchecked_into();
        let _ = window.add_event_listener_with_callback(event_name, &func);
        listener_fn.set(Some(func));
        cb.forget();
    })
}

/// Resolves once every webfont has loaded and the page has stopped reflowing
/// around them.
///
/// Any assertion about geometry is meaningless before this: the page first
/// renders in a fallback face and reflows when the real one arrives, so a
/// measurement taken either side of that swap disagrees with itself. The nav
/// trigger's wordmark moves ~15 px between the two.
fn wait_for_fonts() -> js_sys::Promise {
    let ready = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| js_sys::Reflect::get(&d, &"fonts".into()).ok())
        .and_then(|fonts| js_sys::Reflect::get(&fonts, &"ready".into()).ok())
        .and_then(|ready| ready.dyn_into::<js_sys::Promise>().ok());
    match ready {
        Some(promise) => promise,
        // No FontFaceSet to wait on: nothing will swap, so nothing to wait for.
        None => js_sys::Promise::resolve(&JsValue::NULL),
    }
}

/// Resolves once an element matching `selector` exists in the DOM.
/// Eager check first, then `MutationObserver` on document.body subtree.
fn wait_for_selector(selector: String) -> js_sys::Promise {
    observe_until(selector, ObserveMode::AppearsMatching)
}

/// Resolves once no element matching `selector` exists.
fn wait_for_selector_removal(selector: String) -> js_sys::Promise {
    observe_until(selector, ObserveMode::Removed)
}

/// Resolves once `document.body.innerText` contains `needle` as a substring.
fn wait_for_body_text(needle: String) -> js_sys::Promise {
    observe_until(needle, ObserveMode::BodyTextContains)
}

/// Resolves once `query` evaluates to `expected`.
///
/// Resizing the viewport is asynchronous in a way nothing in the DOM announces:
/// the CDP call returns before the renderer has resized and recalculated style,
/// so a responsive assertion taken straight afterwards can read the layout the
/// page had before. A media query is the same predicate the stylesheet is
/// branching on, so waiting for it to flip is waiting for exactly the recalc the
/// assertion depends on.
fn wait_for_media_query(query: String, expected: bool) -> js_sys::Promise {
    js_sys::Promise::new(&mut |resolve, _reject| {
        let Some(list) = web_sys::window().and_then(|w| w.match_media(&query).ok().flatten())
        else {
            let _ = resolve.call0(&JsValue::NULL);
            return;
        };
        if list.matches() == expected {
            let _ = resolve.call0(&JsValue::NULL);
            return;
        }
        // Same shape as `observe_until`: the listener is owned by JS via
        // `forget`, and detaching it inside the callback severs the only
        // reference, so it becomes collectable once the predicate has fired.
        let resolve_cell: Rc<Cell<Option<js_sys::Function>>> = Rc::new(Cell::new(Some(resolve)));
        let listener_cell: Rc<Cell<Option<js_sys::Function>>> = Rc::new(Cell::new(None));
        let listener_cb = listener_cell.clone();
        let list_cb = list.clone();
        let cb = Closure::<dyn FnMut()>::new(move || {
            if list_cb.matches() != expected {
                return;
            }
            if let Some(listener) = listener_cb.take() {
                let _ = list_cb.remove_event_listener_with_callback("change", &listener);
            }
            if let Some(resolve_fn) = resolve_cell.take() {
                let _ = resolve_fn.call0(&JsValue::NULL);
            }
        });
        let listener: js_sys::Function = cb.as_ref().unchecked_ref::<js_sys::Function>().clone();
        cb.forget();
        if list
            .add_event_listener_with_callback("change", &listener)
            .is_err()
        {
            return;
        }
        listener_cell.set(Some(listener));
    })
}

#[derive(Clone, Copy)]
enum ObserveMode {
    AppearsMatching,
    Removed,
    BodyTextContains,
}

impl ObserveMode {
    /// Evaluate the current DOM against this mode's target string.
    fn satisfied(self, target: &str) -> bool {
        let Some(document) = web_sys::window().and_then(|w| w.document()) else {
            return false;
        };
        match self {
            Self::AppearsMatching => document.query_selector(target).ok().flatten().is_some(),
            Self::Removed => document.query_selector(target).ok().flatten().is_none(),
            Self::BodyTextContains => document
                .body()
                .and_then(|b| b.inner_text().into())
                .is_some_and(|text: String| text.contains(target)),
        }
    }

    /// Whether observing `characterData` mutations matters for this check.
    /// Text-substring waits need it; selector waits don't.
    fn observe_character_data(self) -> bool {
        matches!(self, Self::BodyTextContains)
    }
}

/// Shared implementation for the three "resolve when DOM matches predicate"
/// hooks. The observer self-detaches once the predicate flips so we never
/// leak listeners past resolution. The `Closure` is `forget`'d so JS owns it
/// for the listener's lifetime; the `disconnect` call inside the callback
/// severs the only reference, allowing GC.
fn observe_until(target: String, mode: ObserveMode) -> js_sys::Promise {
    js_sys::Promise::new(&mut |resolve, _reject| {
        if mode.satisfied(&target) {
            let _ = resolve.call0(&JsValue::NULL);
            return;
        }
        let Some(document) = web_sys::window().and_then(|w| w.document()) else {
            let _ = resolve.call0(&JsValue::NULL);
            return;
        };
        let Some(root) = document
            .document_element()
            .or_else(|| document.body().map(Into::into))
        else {
            let _ = resolve.call0(&JsValue::NULL);
            return;
        };

        // Observer is owned by JS via `Closure::forget`; the closure captures
        // a `Rc<Cell<Option<MutationObserver>>>` to call `disconnect()` once
        // the predicate fires.
        let observer_cell: Rc<Cell<Option<web_sys::MutationObserver>>> = Rc::new(Cell::new(None));
        let resolve_cell: Rc<Cell<Option<js_sys::Function>>> = Rc::new(Cell::new(Some(resolve)));
        let target_owned = target.clone();
        let observer_cell_cb = observer_cell.clone();
        let resolve_cb = resolve_cell.clone();
        let cb = Closure::<dyn FnMut()>::new(move || {
            if !mode.satisfied(&target_owned) {
                return;
            }
            if let Some(obs) = observer_cell_cb.take() {
                obs.disconnect();
            }
            if let Some(resolve_fn) = resolve_cb.take() {
                let _ = resolve_fn.call0(&JsValue::NULL);
            }
        });
        let Ok(observer) = web_sys::MutationObserver::new(cb.as_ref().unchecked_ref()) else {
            return;
        };
        cb.forget();
        let init = web_sys::MutationObserverInit::new();
        init.set_child_list(true);
        init.set_subtree(true);
        if mode.observe_character_data() {
            init.set_character_data(true);
        }
        if observer.observe_with_options(&root, &init).is_err() {
            return;
        }
        observer_cell.set(Some(observer));
    })
}

/// Resolves when the map exists *and* is idle. If the map hasn't mounted
/// yet, waits for `MAP_READY_EVENT` first. Once the map exists, checks the
/// idle predicate eagerly so an already-idle map resolves without leaving
/// a `once` listener attached.
fn wait_for_map_idle(handle: &Rc<RefCell<Option<maplibre::Map>>>) -> js_sys::Promise {
    let handle = handle.clone();
    wasm_bindgen_futures::future_to_promise(async move {
        // Wait for map to mount, if needed (`wait_for_map_exists` short-circuits
        // if the map already exists).
        if handle.borrow().is_none() {
            let _ = wasm_bindgen_futures::JsFuture::from(wait_for_map_exists(&handle)).await;
        }
        let Some(map) = handle.borrow().as_ref().cloned() else {
            return Ok(JsValue::NULL);
        };
        // Idle is a state, not a transient event — if it holds now, it holds
        // until the next move. So an eager check is race-free; only attach a
        // listener if we observe active state.
        if map.is_style_loaded() && !map.is_moving() {
            return Ok(JsValue::NULL);
        }
        let idle_promise = js_sys::Promise::new(&mut |resolve, _reject| {
            let cb = Closure::once(move || {
                let _ = resolve.call0(&JsValue::NULL);
            });
            map.once("idle", cb.as_ref());
            cb.forget(); // Transferred to MapLibre's event system; one event, one drop.
        });
        let _ = wasm_bindgen_futures::JsFuture::from(idle_promise).await;
        Ok(JsValue::NULL)
    })
}

/// Returns a Promise that resolves on map idle + two `requestAnimationFrame`
/// ticks. MapLibre's `idle` event fires once its render pipeline is quiet,
/// but its internal feature index may not be queryable for one more frame;
/// the extra RAFs cover that. Bake the dance here so test drivers stop
/// repeating `idle → rAF → rAF` at each call site.
fn wait_for_map_settled(handle: &Rc<RefCell<Option<maplibre::Map>>>) -> js_sys::Promise {
    let idle = wait_for_map_idle(handle);
    wasm_bindgen_futures::future_to_promise(async move {
        wasm_bindgen_futures::JsFuture::from(idle).await?;
        request_animation_frame_async().await;
        request_animation_frame_async().await;
        Ok(JsValue::NULL)
    })
}

/// Resolve once `requestAnimationFrame` fires its callback.
async fn request_animation_frame_async() {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let cb = Closure::once_into_js(move |_: JsValue| {
            let _ = resolve.call0(&JsValue::NULL);
        });
        if let Some(window) = web_sys::window() {
            let _ = window.request_animation_frame(cb.unchecked_ref());
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

/// Waits for map-settled, then returns the descriptors `query` reads. The
/// settled-wait is baked here so the harness wrapper is a one-line macro
/// entry; MapLibre's `query_rendered_features` needs the feature index
/// queryable, which requires 2 RAFs after `idle`. `query` is a plain fn
/// pointer so `marker_properties` and `badge_properties` share this wait.
fn descriptors_settled(
    handle: Rc<RefCell<Option<maplibre::Map>>>,
    query: fn(&maplibre::Map) -> JsValue,
) -> js_sys::Promise {
    let settled = wait_for_map_settled(&handle);
    wasm_bindgen_futures::future_to_promise(async move {
        wasm_bindgen_futures::JsFuture::from(settled).await?;
        Ok(handle.borrow().as_ref().map(query).unwrap_or(JsValue::NULL))
    })
}

/// Waits for map-settled, then fires a synthetic click at lng/lat. The
/// settled-wait is baked here for the same reason as `marker_properties`:
/// MapLibre's click dispatcher calls `queryRenderedFeatures` and panics if
/// the feature index isn't ready.
fn click_map_at(handle: Rc<RefCell<Option<maplibre::Map>>>, lng: f64, lat: f64) -> js_sys::Promise {
    let settled = wait_for_map_settled(&handle);
    wasm_bindgen_futures::future_to_promise(async move {
        wasm_bindgen_futures::JsFuture::from(settled).await?;
        if let Some(map) = handle.borrow().as_ref() {
            fire_map_click(map, lng, lat);
        }
        Ok(JsValue::NULL)
    })
}

/// Waits for map-settled, then reports which marker layers are under
/// `lng`/`lat`, deduplicated in query order.
///
/// The settled-wait is baked for the same reason as [`marker_properties`]:
/// `query_rendered_features` needs the feature index queryable.
fn marker_layers_at(
    handle: Rc<RefCell<Option<maplibre::Map>>>,
    lng: f64,
    lat: f64,
) -> js_sys::Promise {
    let settled = wait_for_map_settled(&handle);
    wasm_bindgen_futures::future_to_promise(async move {
        wasm_bindgen_futures::JsFuture::from(settled).await?;
        Ok(handle
            .borrow()
            .as_ref()
            .map(|map| layers_at(map, lng, lat))
            .unwrap_or(JsValue::NULL))
    })
}

/// Pan the map to coordinates and wait for the resulting entity fetch +
/// map idle. Race-free: samples the counter, then jumps, then awaits the
/// counter advance, then builds the idle wait (so the eager idle check
/// happens *after* the fetch-triggered re-render is underway, not before).
fn pan_map_to(
    handle: Rc<RefCell<Option<maplibre::Map>>>,
    lng: f64,
    lat: f64,
    zoom: f64,
) -> js_sys::Promise {
    wasm_bindgen_futures::future_to_promise(async move {
        let prev = current_fetch_settled();
        if let Some(map) = handle.borrow().as_ref() {
            jump_to(map, lng, lat, zoom);
        }
        // Wait for the fetch this pan triggered. Counter advances on success
        // and failure, so the retry-banner test path works too.
        wasm_bindgen_futures::JsFuture::from(wait_for_fetch_settled_after(prev)).await?;
        // Only NOW build the idle wait — if we'd built it upfront, its eager
        // check would see the just-finished jump and resolve before the
        // fetch-triggered re-render started.
        wasm_bindgen_futures::JsFuture::from(wait_for_map_idle(&handle)).await?;
        Ok(JsValue::NULL)
    })
}

/// Click an element matching `selector` and wait for the resulting fetch
/// to settle. Same race-free shape as `pan_map_to`: sample counter, do the
/// click (which itself waits for the element first), then wait for the
/// counter to advance.
fn click_and_wait_for_fetch(selector: String) -> js_sys::Promise {
    wasm_bindgen_futures::future_to_promise(async move {
        let prev = current_fetch_settled();
        wasm_bindgen_futures::JsFuture::from(click(selector)).await?;
        wasm_bindgen_futures::JsFuture::from(wait_for_fetch_settled_after(prev)).await?;
        Ok(JsValue::NULL)
    })
}

/// Drive the time slider to `year` and await the refetch it triggers.
///
/// Sets the range input's value and dispatches the `input` event the component
/// listens for, so the test exercises the same path a user's drag does rather
/// than poking the signal behind it. The input carries track units, so the year
/// goes through the same scale the component uses, built from the axis's own
/// `data-max-year`.
fn set_time_slider_year(year: f64) -> js_sys::Promise {
    wasm_bindgen_futures::future_to_promise(async move {
        let prev = current_fetch_settled();
        wasm_bindgen_futures::JsFuture::from(wait_for_selector("#time-slider".to_string())).await?;
        let input = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.query_selector("#time-slider").ok().flatten())
            .and_then(|el| el.dyn_into::<web_sys::HtmlInputElement>().ok());
        let Some(input) = input else {
            return Err(JsValue::from_str("time slider input not found"));
        };
        let max_year = input
            .get_attribute("data-max-year")
            .and_then(|attr| attr.parse::<i32>().ok());
        let Some(max_year) = max_year else {
            return Err(JsValue::from_str(
                "set_time_slider_year: the slider carries no numeric data-max-year to build its scale from",
            ));
        };
        let scale = TimeScale::new(max_year);
        let requested = year as i32;
        let unit = scale.position(requested);
        let landed = scale.year(unit);
        // Only the photographic band resolves to the year; before it the axis
        // steps 2 years, and before 1500, 20. A test that named a year off that
        // grid would assert about an instant the map never showed, and would
        // report the year it asked for rather than the one it got. Refuse it
        // here, where the year is still in hand, and name the one to use.
        if landed != requested {
            return Err(JsValue::from_str(&format!(
                "set_time_slider_year: the axis has no position for {}, whose band steps to {}",
                era_label(requested),
                era_label(landed)
            )));
        }
        let target = unit.0.to_string();
        // A year the slider already holds arms no refetch, so awaiting the
        // settled counter would hang until timeout instead of returning. Report
        // it as an error the test can read rather than a mysterious stall.
        if input.value() == target {
            return Err(JsValue::from_str(&format!(
                "set_time_slider_year: the slider is already at {}, so no refetch would follow",
                era_label(landed)
            )));
        }
        input.set_value(&target);
        // Dispatched on the input itself, which is where Leptos attached the
        // listener, so the event needs no bubbling.
        let event = web_sys::Event::new("input")?;
        input.dispatch_event(&event)?;
        wasm_bindgen_futures::JsFuture::from(wait_for_fetch_settled_after(prev)).await?;
        Ok(JsValue::NULL)
    })
}

// ==================== DOM query / action hooks ====================

/// Return `document.body.innerText`, or `""` if body is missing.
fn body_text() -> String {
    web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.body())
        .map(|b| b.inner_text())
        .unwrap_or_default()
}

/// Waits for `selector` to appear, then returns the first match's `innerText`.
/// Returns `""` if the element disappears again between the wait and the
/// read (vanishingly unlikely but defended against).
fn text(selector: String) -> js_sys::Promise {
    let wait = wait_for_selector(selector.clone());
    wasm_bindgen_futures::future_to_promise(async move {
        wasm_bindgen_futures::JsFuture::from(wait).await?;
        let value = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.query_selector(&selector).ok().flatten())
            .and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok())
            .map(|el| el.inner_text())
            .unwrap_or_default();
        Ok(JsValue::from(value))
    })
}

/// Waits for `selector` to appear, then returns the first match's attribute
/// value. `null` if the element has no such attribute.
fn attr(selector: String, attribute: String) -> js_sys::Promise {
    let wait = wait_for_selector(selector.clone());
    wasm_bindgen_futures::future_to_promise(async move {
        wasm_bindgen_futures::JsFuture::from(wait).await?;
        let value = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.query_selector(&selector).ok().flatten())
            .and_then(|el| el.get_attribute(&attribute))
            .map(JsValue::from)
            .unwrap_or(JsValue::NULL);
        Ok(value)
    })
}

/// Waits for `selector` to appear, then clicks the first visible match
/// (the `click_visible` rules — `offsetParent.is_some()`). Resolves after
/// any resulting CSS transition (with a 250ms fallback).
fn click(selector: String) -> js_sys::Promise {
    let sel_for_wait = selector.clone();
    wasm_bindgen_futures::future_to_promise(async move {
        wasm_bindgen_futures::JsFuture::from(wait_for_selector(sel_for_wait)).await?;
        wasm_bindgen_futures::JsFuture::from(click_visible(selector)).await?;
        Ok(JsValue::NULL)
    })
}

/// Whether the first matching element is rendered.
///
/// The rule comes from [`is_rendered`], which the focus trap uses to decide
/// what it may focus. The two answering the same question differently is how a
/// test ends up asserting something false.
fn is_visible(selector: String) -> bool {
    web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.query_selector(&selector).ok().flatten())
        .and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok())
        .is_some_and(|el| is_rendered(&el))
}

/// `document.querySelectorAll(selector).length`.
fn count_matching(selector: &str) -> u32 {
    web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.query_selector_all(selector).ok())
        .map(|nodes| nodes.length())
        .unwrap_or(0)
}

/// `Array.from(document.querySelectorAll(selector)).map(el => el.getAttribute(attr))`,
/// returned as a JS array. Missing attributes appear as `null`. Used by
/// accessibility tests that need to verify every matching element has a
/// well-formed attribute.
fn element_attributes(selector: &str, attr: &str) -> JsValue {
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return js_sys::Array::new().into();
    };
    let Ok(nodes) = document.query_selector_all(selector) else {
        return js_sys::Array::new().into();
    };
    let arr = js_sys::Array::new();
    for i in 0..nodes.length() {
        let Some(node) = nodes.item(i) else {
            arr.push(&JsValue::NULL);
            continue;
        };
        let value = node
            .dyn_ref::<web_sys::Element>()
            .and_then(|el| el.get_attribute(attr))
            .map(JsValue::from)
            .unwrap_or(JsValue::NULL);
        arr.push(&value);
    }
    arr.into()
}

/// Stretch every animation on the page to `ms`, so entry animations are still
/// live when [`sample_animation_at`] seeks them.
///
/// A finished CSS animation drops out of `getAnimations()`, so a sample that
/// arrived after a real 150 ms run would find nothing, read the settled state,
/// and pass whatever the animation did in between. Stretching first removes that
/// race; the seek then makes each sample exact rather than "whenever we looked".
fn slow_animations(ms: f64) {
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let Ok(style) = document.create_element("style") else {
        return;
    };
    style.set_text_content(Some(&format!(
        "*, *::before, *::after {{ animation-duration: {ms}ms !important; }}"
    )));
    if let Some(head) = document.head() {
        let _ = head.append_child(&style);
    }
}

/// Hold the first match's entry animation at `progress` (0.0..=1.0) and read it
/// there, returning `[animation_count, opacity, x, y, width, height]`. Empty
/// when nothing matches.
///
/// `animation_count` is first so a caller can refuse to draw conclusions from a
/// sample that found nothing to seek — otherwise this reads the settled element
/// and reports success no matter what happened mid-flight.
///
/// A transition's endpoints can both be correct while a frame between them is
/// not: an overlay card that fades up from transparent leaves its chip's area
/// unpainted for a frame, and one that scales flattens its own rounded ends
/// mid-flight. Neither is visible to a before/after assertion.
///
/// Seeks via the Web Animations API rather than sleeping, so the sample is
/// exact rather than "whatever had rendered by the time we looked" — a timing
/// race here would be a flaky test, which is worse than no test. Animations on
/// descendants are seeked too, since the surface and its contents animate
/// separately.
///
/// Throws when an effect's duration reads back as anything but a number, which
/// is how `getTiming()` reports `"auto"`. Treating that as zero would seek every
/// requested progress to the first frame and report the readings as if they
/// spanned the animation. `getComputedTiming()` is no help: it resolves `"auto"`
/// to 0, which is the same silence with a number on it.
fn sample_animation_at(selector: String, progress: f64) -> Result<JsValue, JsValue> {
    let arr = js_sys::Array::new();
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return Ok(arr.into());
    };
    let Some(element) = document.query_selector(&selector).ok().flatten() else {
        return Ok(arr.into());
    };

    // `getAnimations({subtree: true})` — reached through Reflect because web-sys
    // doesn't bind the options form.
    let opts = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&opts, &"subtree".into(), &JsValue::TRUE);
    let animations = js_sys::Reflect::get(&element, &"getAnimations".into())
        .ok()
        .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
        .and_then(|f| f.call1(&element, &opts).ok())
        .and_then(|v| v.dyn_into::<js_sys::Array>().ok())
        .unwrap_or_default();

    for animation in animations.iter() {
        let duration = js_sys::Reflect::get(&animation, &"effect".into())
            .ok()
            .and_then(|effect| {
                js_sys::Reflect::get(&effect, &"getTiming".into())
                    .ok()
                    .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
                    .and_then(|f| f.call0(&effect).ok())
            })
            .and_then(|timing| js_sys::Reflect::get(&timing, &"duration".into()).ok())
            .unwrap_or(JsValue::UNDEFINED);
        let Some(ms) = duration.as_f64() else {
            return Err(JsValue::from_str(&format!(
                "sample_animation_at({selector:?}): an effect's duration read back as \
                 {duration:?}, so there is no time axis to seek along"
            )));
        };
        let _ = js_sys::Reflect::set(&animation, &"currentTime".into(), &(ms * progress).into());
    }

    let opacity = web_sys::window()
        .and_then(|w| w.get_computed_style(&element).ok().flatten())
        .and_then(|style| style.get_property_value("opacity").ok())
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(1.0);
    let rect = element.get_bounding_client_rect();
    for value in [
        f64::from(animations.length()),
        opacity,
        rect.x(),
        rect.y(),
        rect.width(),
        rect.height(),
    ] {
        arr.push(&value.into());
    }
    Ok(arr.into())
}

/// `getBoundingClientRect()` of the first match as `[x, y, width, height]`,
/// empty when nothing matches.
///
/// Sub-pixel values pass through unrounded. The overlay toggles' invariant is
/// that this rect is *identical* either side of a collapse, and the drift that
/// breaks it is small — a content-sized chip that drops its border between
/// states moves its contents by a single pixel.
fn element_rect(selector: &str) -> JsValue {
    let arr = js_sys::Array::new();
    let Some(element) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.query_selector(selector).ok().flatten())
    else {
        return arr.into();
    };
    let rect = element.get_bounding_client_rect();
    for value in [rect.x(), rect.y(), rect.width(), rect.height()] {
        arr.push(&value.into());
    }
    arr.into()
}

/// Whether the first element matching `selector` actually receives a click at
/// its own centre — i.e. it is the topmost thing painted there.
///
/// `element_rect` proves a control is *where* it should be; this proves it is
/// *reachable*. The two failures that motivated it were both invisible to
/// geometry: a chip rendered under a fixed bar, and a trigger buried by the
/// panel it opens. Both had a perfect rect and swallowed every click.
///
/// A descendant counts as a hit — the point usually lands on an inner glyph or
/// label, and the event bubbles to the control regardless.
fn is_hittable(selector: String) -> bool {
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return false;
    };
    let Some(element) = document.query_selector(&selector).ok().flatten() else {
        return false;
    };
    let rect = element.get_bounding_client_rect();
    let Some(topmost) = document.element_from_point(
        (rect.left() + rect.width() / 2.0) as f32,
        (rect.top() + rect.height() / 2.0) as f32,
    ) else {
        return false;
    };
    topmost == element || element.contains(Some(topmost.as_ref()))
}

/// Whether `document.activeElement` is the element matching `selector` or
/// a descendant of it. Returns false if either lookup fails.
fn is_active_inside(selector: String) -> bool {
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return false;
    };
    let Some(active) = document.active_element() else {
        return false;
    };
    let Some(target) = document.query_selector(&selector).ok().flatten() else {
        return false;
    };
    target.contains(Some(active.as_ref()))
}

/// `document.activeElement?.getAttribute(attr)`.
fn active_element_attribute(attr: &str) -> Option<String> {
    web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.active_element())
        .and_then(|el| el.get_attribute(attr))
}

/// Call `.focus()` on the first matching `HTMLElement`. Returns whether the
/// call dispatched (i.e., the selector matched a focusable element).
fn focus_element(selector: String) -> bool {
    web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.query_selector(&selector).ok().flatten())
        .and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok())
        .is_some_and(|el| el.focus().is_ok())
}

/// Dispatch a bubbling `keydown` event with the given `key` on the first
/// matching element. Returns whether dispatch happened.
fn press_key(selector: String, key: String) -> bool {
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return false;
    };
    let Some(target) = document.query_selector(&selector).ok().flatten() else {
        return false;
    };
    let init = web_sys::KeyboardEventInit::new();
    init.set_key(&key);
    init.set_bubbles(true);
    let Ok(event) = web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init)
    else {
        return false;
    };
    target.dispatch_event(&event).unwrap_or(false)
}

// ==================== Internal helpers ====================

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
fn marker_count(map: &maplibre::Map) -> u32 {
    use crate::components::map::{ENTITY_CIRCLES_LAYER, ENTITY_THUMBNAILS_LAYER};
    let opts = js_sys::Object::new();
    let layers = js_sys::Array::new();
    layers.push(&ENTITY_CIRCLES_LAYER.into());
    layers.push(&ENTITY_THUMBNAILS_LAYER.into());
    let _ = js_sys::Reflect::set(&opts, &"layers".into(), &layers);
    map.query_rendered_features(&JsValue::UNDEFINED, &opts)
        .length()
}

fn thumbnail_marker_count(map: &maplibre::Map) -> u32 {
    use crate::components::map::ENTITY_THUMBNAILS_LAYER;
    let opts = js_sys::Object::new();
    let layers = js_sys::Array::new();
    layers.push(&ENTITY_THUMBNAILS_LAYER.into());
    let _ = js_sys::Reflect::set(&opts, &"layers".into(), &layers);
    map.query_rendered_features(&JsValue::UNDEFINED, &opts)
        .length()
}

/// Descriptors for the individual-entity layers (circles + thumbnails) — one
/// per rendered `Select`/`Disambiguate` marker. Each carries the feature
/// `properties` plus `_lng`/`_lat` and `_x`/`_y` (see [`layer_descriptors`]),
/// so tests can assert on properties, click the rendered coordinates, and
/// measure pairwise screen distance.
fn marker_properties(map: &maplibre::Map) -> JsValue {
    use crate::components::map::{ENTITY_CIRCLES_LAYER, ENTITY_THUMBNAILS_LAYER};
    layer_descriptors(map, &[ENTITY_CIRCLES_LAYER, ENTITY_THUMBNAILS_LAYER])
}

/// Descriptors for the cluster-badge layer — MapLibre proximity clusters
/// (carrying `point_count`) and lone server `Expand` cells (`kind ==
/// "cluster"`). Same shape as [`marker_properties`], so a test can count
/// badges and read each badge's centroid for a click.
fn badge_properties(map: &maplibre::Map) -> JsValue {
    use crate::components::map::ENTITY_BADGE_LAYER;
    layer_descriptors(map, &[ENTITY_BADGE_LAYER])
}

/// Descriptors for the verdict-ring layer alone.
///
/// Its own hook because a ring shares its marker's feature id, and
/// [`layer_descriptors`] keeps the first hit per id: folded in beside the circle
/// and thumbnail layers, a ring would be deduped away and an assertion aimed at
/// it would read its marker's disc instead.
fn ring_properties(map: &maplibre::Map) -> JsValue {
    use crate::components::map::ENTITY_RING_LAYER;
    layer_descriptors(map, &[ENTITY_RING_LAYER])
}

/// Deduplicated descriptors for every feature rendered across `layers`.
///
/// Each descriptor is the feature's flat `properties`, plus `_lng`/`_lat` from
/// the point geometry and `_x`/`_y` — the geometry projected to CSS pixels via
/// `map.project`. The pixel coordinates let a test assert that no two rendered
/// markers sit within the proximity-cluster radius (the near-split check).
fn layer_descriptors(map: &maplibre::Map, layers: &[&str]) -> JsValue {
    let opts = js_sys::Object::new();
    let layer_arr = js_sys::Array::new();
    for layer in layers {
        layer_arr.push(&(*layer).into());
    }
    let _ = js_sys::Reflect::set(&opts, &"layers".into(), &layer_arr);
    let features = map.query_rendered_features(&JsValue::UNDEFINED, &opts);

    // Dedupe by `feature.id`: a feature can surface once per tile it spans, and
    // an individual with a thumbnail is promoted across the circle and
    // thumbnail layers. The source sets `promoteId: "feature_id"` for leaves
    // and MapLibre assigns a numeric id to a proximity cluster, so every
    // rendered feature carries a stable id — a missing one is a bug to surface.
    let seen = js_sys::Set::new(&JsValue::UNDEFINED);
    let result = js_sys::Array::new();
    for i in 0..features.length() {
        let feature = features.get(i);
        let id = js_sys::Reflect::get(&feature, &"id".into()).unwrap_or(JsValue::UNDEFINED);
        if id.is_undefined() || id.is_null() {
            web_sys::console::warn_1(
                &"layer_descriptors: feature missing id (generateId not set?)".into(),
            );
            continue;
        }
        if seen.has(&id) {
            continue;
        }
        seen.add(&id);

        // Build the descriptor: properties + _lng/_lat + projected _x/_y.
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
            let lng = coords_arr.get(0);
            let lat = coords_arr.get(1);
            if let (Some(lng_f), Some(lat_f)) = (lng.as_f64(), lat.as_f64()) {
                let screen = project_lnglat(map, lng_f, lat_f);
                let sx = js_sys::Reflect::get(&screen, &"x".into())
                    .ok()
                    .and_then(|v| v.as_f64());
                let sy = js_sys::Reflect::get(&screen, &"y".into())
                    .ok()
                    .and_then(|v| v.as_f64());
                if let (Some(sx), Some(sy)) = (sx, sy) {
                    let _ = js_sys::Reflect::set(&descriptor, &"_x".into(), &sx.into());
                    let _ = js_sys::Reflect::set(&descriptor, &"_y".into(), &sy.into());
                }
            }
            let _ = js_sys::Reflect::set(&descriptor, &"_lng".into(), &lng);
            let _ = js_sys::Reflect::set(&descriptor, &"_lat".into(), &lat);
        }
        result.push(&descriptor);
    }
    result.into()
}

/// The marker layers rendering something at `lng`/`lat`, as a JS array of layer
/// ids. Which layer owns a pixel is what decides whether a click there selects
/// the marker or falls through to the background handler.
fn layers_at(map: &maplibre::Map, lng: f64, lat: f64) -> JsValue {
    let point = project_lnglat(map, lng, lat);
    let opts = js_sys::Object::new();
    let layers = js_sys::Array::new();
    for layer in crate::components::map::marker_layers() {
        layers.push(&layer.into());
    }
    let _ = js_sys::Reflect::set(&opts, &"layers".into(), &layers);
    let features = map.query_rendered_features(&point, &opts);

    let seen = js_sys::Set::new(&JsValue::UNDEFINED);
    let result = js_sys::Array::new();
    for i in 0..features.length() {
        let Ok(id) = js_sys::Reflect::get(&features.get(i), &"layer".into())
            .and_then(|layer| js_sys::Reflect::get(&layer, &"id".into()))
        else {
            continue;
        };
        if !seen.has(&id) {
            seen.add(&id);
            result.push(&id);
        }
    }
    result.into()
}

/// The coordinate `dx`/`dy` CSS pixels from `lng`/`lat` on screen, as
/// `[lng, lat]`.
fn offset_lnglat(map: &maplibre::Map, lng: f64, lat: f64, dx: f64, dy: f64) -> JsValue {
    let point = project_lnglat(map, lng, lat);
    let read = |key: &str, source: &JsValue| {
        js_sys::Reflect::get(source, &key.into())
            .ok()
            .and_then(|v| v.as_f64())
    };
    let (Some(x), Some(y)) = (read("x", &point), read("y", &point)) else {
        return JsValue::NULL;
    };
    let shifted = js_sys::Array::of2(&(x + dx).into(), &(y + dy).into());
    let lnglat = map.unproject(shifted.as_ref());
    let (Some(lng), Some(lat)) = (read("lng", &lnglat), read("lat", &lnglat)) else {
        return JsValue::NULL;
    };
    js_sys::Array::of2(&lng.into(), &lat.into()).into()
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

    let init = web_sys::MouseEventInit::new();
    init.set_client_x(client_x as i32);
    init.set_client_y(client_y as i32);
    init.set_bubbles(true);

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
    fn unproject(this: &maplibre::Map, point: &JsValue) -> JsValue;

    #[wasm_bindgen(method, js_class = "Map")]
    fn fire(this: &maplibre::Map, event_type: &str, data: &JsValue) -> JsValue;

    #[wasm_bindgen(method, js_class = "Map", js_name = getStyle)]
    fn get_style(this: &maplibre::Map) -> JsValue;
}
