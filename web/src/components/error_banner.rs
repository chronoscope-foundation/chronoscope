use std::cell::{Cell, RefCell};
use std::rc::Rc;

use leptos::prelude::*;
use send_wrapper::SendWrapper;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::components::dismiss_button::DismissButton;

// ==================== Error pushing ====================

/// Creates a callback that assigns an ID and pushes an error message into the
/// signal, capping the list at 10 entries.
fn make_push_error(
    set_errors: WriteSignal<Vec<(u64, String)>>,
    id_counter: &Rc<Cell<u64>>,
) -> Rc<dyn Fn(String)> {
    let id_counter = Rc::clone(id_counter);
    Rc::new(move |msg: String| {
        let id = id_counter.get();
        id_counter.set(id + 1);
        set_errors.update(|errs| {
            if errs.len() >= 10 {
                errs.remove(0);
            }
            errs.push((id, msg));
        });
    })
}

// ==================== Event listener registration ====================

/// Registered listener info for cleanup: `(event_name, js_function)`.
type ListenerRef = (String, js_sys::Function);

/// Extract a message from a synchronous JS error event.
fn error_message(e: &web_sys::ErrorEvent) -> String {
    let msg = e.message();
    if msg.is_empty() {
        "Unknown error".to_string()
    } else {
        msg
    }
}

/// Register a `window.addEventListener` callback. Returns the closure and the
/// listener ref needed for cleanup.
fn register_event_handler<E: JsCast + wasm_bindgen::convert::FromWasmAbi + 'static>(
    window: &web_sys::Window,
    event_name: &str,
    push_error: &Rc<dyn Fn(String)>,
    extract_msg: fn(&E) -> String,
) -> (Box<dyn std::any::Any>, ListenerRef) {
    let push = Rc::clone(push_error);
    let cb = Closure::<dyn Fn(E)>::new(move |e: E| {
        push(extract_msg(&e));
    });
    let js_func: js_sys::Function = cb.as_ref().unchecked_ref::<js_sys::Function>().clone();
    let _ = window.add_event_listener_with_callback(event_name, &js_func);
    (Box::new(cb), (event_name.to_string(), js_func))
}

/// Extract a message from an unhandled promise rejection.
fn rejection_message(e: &web_sys::PromiseRejectionEvent) -> String {
    let reason = e.reason();
    reason
        .as_string()
        .or_else(|| {
            js_sys::Reflect::get(&reason, &"message".into())
                .ok()
                .and_then(|v| v.as_string())
        })
        .unwrap_or_else(|| format!("{reason:?}"))
}

/// Extract a message from a custom error event.
fn custom_error_message(e: &web_sys::CustomEvent) -> String {
    e.detail()
        .as_string()
        .unwrap_or_else(|| "Unknown error".to_string())
}

// ==================== Component ====================

/// Global error banner that captures unhandled JS errors and promise rejections.
#[component]
pub fn ErrorBanner() -> impl IntoView {
    let (errors, set_errors) = signal(Vec::<(u64, String)>::new());
    let next_id: Rc<Cell<u64>> = Rc::new(Cell::new(0));

    let closures: Rc<RefCell<Vec<Box<dyn std::any::Any>>>> = Rc::new(RefCell::new(Vec::new()));
    let closures_effect = Rc::clone(&closures);

    let listener_refs: Rc<RefCell<Vec<ListenerRef>>> = Rc::new(RefCell::new(Vec::new()));
    let listener_refs_cleanup = Rc::clone(&listener_refs);
    let listener_refs_effect = Rc::clone(&listener_refs);

    Effect::new(move || {
        closures_effect.borrow_mut().clear();
        listener_refs_effect.borrow_mut().clear();

        let Some(window) = web_sys::window() else {
            return;
        };

        let push_error = make_push_error(set_errors, &next_id);

        // Synchronous JS errors (via addEventListener, not set_onerror — the
        // latter receives 5 positional args, not an ErrorEvent).
        let (cb, listener) = register_event_handler(&window, "error", &push_error, error_message);
        closures_effect.borrow_mut().push(cb);
        listener_refs_effect.borrow_mut().push(listener);

        // window.onunhandledrejection — unhandled promise rejections
        let (cb, listener) = register_event_handler(
            &window,
            "unhandledrejection",
            &push_error,
            rejection_message,
        );
        closures_effect.borrow_mut().push(cb);
        listener_refs_effect.borrow_mut().push(listener);

        // chronoscope-error — custom events dispatched by application code
        let (cb, listener) = register_event_handler(
            &window,
            "chronoscope-error",
            &push_error,
            custom_error_message,
        );
        closures_effect.borrow_mut().push(cb);
        listener_refs_effect.borrow_mut().push(listener);
    });

    let cleanup_closures = SendWrapper::new(Rc::clone(&closures));
    let cleanup_listeners = SendWrapper::new(listener_refs_cleanup);
    on_cleanup(move || {
        if let Some(window) = web_sys::window() {
            for (event_name, func) in cleanup_listeners.borrow().iter() {
                let _ = window.remove_event_listener_with_callback(event_name, func);
            }
        }
        cleanup_listeners.borrow_mut().clear();
        cleanup_closures.borrow_mut().clear();
    });

    let dismiss = move |id: u64| {
        set_errors.update(|errs| {
            errs.retain(|(err_id, _)| *err_id != id);
        });
    };

    view! {
        // Below the nav trigger, not under it. The trigger is `fixed` chrome in
        // the top-left corner at every width, and at `top-0` this strip's first
        // words sat behind an opaque pill — unreadable, which for an error
        // message is the whole point of it. Raising the strip's z-index instead
        // would have hidden the trigger and made the nav unreachable while any
        // error showed.
        <div class="fixed top-14 left-0 right-0 z-50 pointer-events-none">
            {move || {
                errors.get().into_iter().map(|(id, msg)| {
                    let dismiss_this = move |_| dismiss(id);
                    view! {
                        <div role="alert" class="bg-red-600/95 text-white px-4 py-2 text-sm font-sans flex items-center justify-between pointer-events-auto">
                            <span class="truncate mr-4">{msg}</span>
                            <DismissButton
                                on_click=dismiss_this
                                label="Dismiss"
                                color="text-white/70 hover:text-white"
                                extra_class="shrink-0"
                            />
                        </div>
                    }
                }).collect::<Vec<_>>()
            }}
        </div>
    }
}
