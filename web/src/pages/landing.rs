use std::cell::RefCell;
use std::rc::Rc;

use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::api::Client;
use crate::components::dismiss_button::DismissButton;
use crate::components::entity_detail::{EntityDetailPanel, LightboxState};
use crate::components::map::{MapStatus, MapView, SelectedEntity};

const INFO_DISMISSED_KEY: &str = "chronoscope-info-dismissed";

fn is_info_dismissed() -> bool {
    web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
        .and_then(|s| s.get_item(INFO_DISMISSED_KEY).ok().flatten())
        .is_some()
}

fn set_info_dismissed(dismissed: bool) {
    if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        if dismissed {
            let _ = storage.set_item(INFO_DISMISSED_KEY, "1");
        } else {
            let _ = storage.remove_item(INFO_DISMISSED_KEY);
        }
    }
}

#[component]
pub fn Landing() -> impl IntoView {
    let (info_open, set_info_open) = signal(!is_info_dismissed());

    let dismiss = move |_| {
        set_info_open.set(false);
        set_info_dismissed(true);
    };

    let restore = move |_| {
        set_info_open.set(true);
        set_info_dismissed(false);
    };

    // API client handle — lazily initialized on first fetch, shared between
    // the map (entity list) and the detail panel (entity detail).
    let api_client: Rc<RefCell<Option<Client>>> = Rc::new(RefCell::new(None));

    // Map handle — shared with MapView (which populates it on mount) and test
    // hooks (which query/drive the map programmatically).
    let map_handle: Rc<RefCell<Option<crate::maplibre::Map>>> = Rc::new(RefCell::new(None));

    // Register browser test hooks when compiled with `--features test-hooks`.
    // These live on `window.__test` and give the test harness typed access to
    // the map and API client without exposing raw handles on the window.
    #[cfg(feature = "test-hooks")]
    crate::test_hooks::register_map_hooks(map_handle.clone(), api_client.clone());

    // Lightbox state — provided via context so the overlay renders here
    // (outside the sidebar's CSS transform, which breaks `position: fixed`).
    let (lightbox_content, set_lightbox_content) =
        signal(None::<crate::components::entity_detail::LightboxContent>);
    let lightbox = LightboxState(lightbox_content, set_lightbox_content);
    provide_context(lightbox.clone());

    view! {
        // Map fills the entire main area — no scrolling.
        // Mobile: subtract the 3.5rem top bar. Desktop: full viewport (sidebar is flex, not stacked).
        <div class="h-[calc(100vh-3.5rem)] md:h-screen relative">
            <MapView api_client=api_client.clone() map_handle=map_handle/>

            // Entity detail panel (slides in from right on marker click)
            <EntityDetailPanel api_client=api_client.clone()/>

            // Map status overlays (loading, empty)
            <MapStatusOverlay/>

            // Dismissible info card — collapsible "about" overlay for new visitors.
            // Dismissal persists in localStorage so returning users get a clean map.
            <div class="absolute top-3 right-3 left-3 md:left-auto">
                {move || if info_open.get() {
                    view! {
                        <div class="bg-parchment/95 backdrop-blur-sm rounded-lg shadow-md p-5 w-full md:w-[30rem] pointer-events-auto"
                            style="text-wrap: pretty"
                        >
                            <div class="flex justify-between items-start mb-3">
                                <h2 class="text-sm font-semibold font-sans text-copper">"About Chronoscope"</h2>
                                <DismissButton on_click=dismiss extra_class="ml-4"/>
                            </div>
                            <p class="text-sm text-sepia leading-relaxed mb-2">
                                "Every place has layers. Chronoscope lets you peel them back: pull up "
                                "historical photos of the street you\u{2019}re on, see a demolished "
                                "building as it once stood, or identify the ruins you just stumbled across."
                            </p>
                            <p class="text-sm text-sepia leading-relaxed mb-4">
                                "Chronoscope gets better the more people use it. Link a photo to a location, "
                                "help resolve conflicting dates, or chase down a mystery nobody\u{2019}s solved yet."
                            </p>
                            <div class="space-y-1.5 text-xs text-sepia/80">
                                <p><span class="text-copper font-semibold">"Explicit uncertainty"</span>
                                    " \u{2014} \u{201c}built circa 1920s\u{201d} narrows as more evidence arrives. No false precision."</p>
                                <p><span class="text-copper font-semibold">"Pervasive citations"</span>
                                    " \u{2014} every assertion is grounded in a specific photo, map, or record."</p>
                                <p><span class="text-copper font-semibold">"Transparent contributions"</span>
                                    " \u{2014} every change, human or AI, gets versioned and reviewed."</p>
                            </div>
                        </div>
                    }.into_any()
                } else {
                    view! {
                        <button
                            class="bg-parchment/95 backdrop-blur-sm rounded-full w-8 h-8 flex items-center justify-center shadow-md text-sepia hover:text-ink cursor-pointer pointer-events-auto font-sans text-sm font-semibold"
                            on:click=restore
                            aria-label="About Chronoscope"
                        >
                            "?"
                        </button>
                    }.into_any()
                }}
            </div>

            // Image lightbox overlay — rendered outside the sidebar so
            // `position: fixed` is relative to the viewport, not the
            // sidebar's CSS transform.
            <ImageLightbox lightbox=lightbox/>
        </div>
    }
}

/// Fullscreen image preview overlay, centered in the browser viewport.
#[component]
fn ImageLightbox(lightbox: LightboxState) -> impl IntoView {
    let set_content = lightbox.1;
    let close = move |_: leptos::ev::MouseEvent| set_content.set(None);
    let close_key = move |ev: leptos::ev::KeyboardEvent| {
        match ev.key().as_str() {
            "Escape" => set_content.set(None),
            "Tab" => {
                // Trap focus within the dialog (close button + open-original link).
                if let Some(dialog) = ev
                    .current_target()
                    .and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok())
                    && let Ok(focusables) = dialog.query_selector_all("button, a[href]")
                {
                    let len = focusables.length();
                    if len > 0 {
                        let active = web_sys::window()
                            .and_then(|w| w.document())
                            .and_then(|d| d.active_element());
                        let first = focusables
                            .item(0)
                            .and_then(|n| n.dyn_into::<web_sys::Element>().ok());
                        let last = focusables
                            .item(len - 1)
                            .and_then(|n| n.dyn_into::<web_sys::Element>().ok());
                        if ev.shift_key() {
                            if active == first {
                                ev.prevent_default();
                                if let Some(el) =
                                    last.and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
                                {
                                    let _ = el.focus();
                                }
                            }
                        } else if active == last {
                            ev.prevent_default();
                            if let Some(el) =
                                first.and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
                            {
                                let _ = el.focus();
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    };
    let content = lightbox.0;

    view! {
        {move || content.get().map(|c| {
            let backdrop_ref = NodeRef::<leptos::html::Div>::new();
            Effect::new(move || {
                if let Some(el) = backdrop_ref.get() {
                    let _ = el.focus();
                }
            });
            view! {
                <div
                    node_ref=backdrop_ref
                    class="fixed inset-0 z-[100] bg-ink/90 flex items-center justify-center p-8 \
                           animate-[fadeIn_150ms_ease-out]"
                    on:click=close
                    on:keydown=close_key
                    tabindex="-1"
                    role="dialog"
                    aria-label="Image preview"
                >
                    <img
                        src=c.url.clone()
                        alt=c.alt.clone()
                        class="max-w-full max-h-full object-contain rounded-lg \
                               ring-1 ring-parchment/20"
                        on:click=|ev: leptos::ev::MouseEvent| ev.stop_propagation()
                    />
                    <button
                        class="absolute top-4 left-4 text-parchment/80 hover:text-parchment text-xl font-sans \
                               bg-ink/60 rounded-full w-8 h-8 flex items-center justify-center cursor-pointer"
                        on:click=close
                        aria-label="Close preview"
                    >
                        "\u{00d7}"
                    </button>
                    <a
                        href=c.source_url.clone()
                        target="_blank"
                        rel="noopener noreferrer"
                        class="absolute top-4 right-4 text-parchment/80 hover:text-parchment text-sm font-sans \
                               bg-ink/60 rounded px-2 py-1"
                        on:click=|ev: leptos::ev::MouseEvent| ev.stop_propagation()
                    >
                        "Open original \u{2197}"
                    </a>
                </div>
            }
        })}
    }
}

/// Renders map status overlays: loading indicator and empty state.
#[component]
fn MapStatusOverlay() -> impl IntoView {
    let Some(status) = use_context::<MapStatus>() else {
        return view! { <div/> }.into_any();
    };
    let loading = status.loading;
    let empty = status.empty;
    let fetch_error = status.fetch_error;
    let retry = status.retry;

    // Hide status overlays when entity detail panel is open (on mobile
    // the bottom sheet covers the overlay area).
    let SelectedEntity(selected, _) = expect_context::<SelectedEntity>();
    let panel_open = move || selected.get().is_some();

    // Track loading transitions to announce entity availability
    let (announced, set_announced) = signal(false);
    let prev_loading = std::cell::Cell::new(false);
    Effect::new(move || {
        let is_loading = loading.get();
        let is_empty = empty.get();
        if prev_loading.get() && !is_loading && !is_empty {
            set_announced.set(true);
        } else if is_loading {
            set_announced.set(false);
        }
        prev_loading.set(is_loading);
    });

    view! {
        <div aria-live="polite">
            // Loading indicator (hidden when detail panel is open)
            {move || (loading.get() && !panel_open()).then(|| view! {
                <div class="absolute bottom-3 left-3 pointer-events-none">
                    <span class="text-xs text-sepia/70 bg-parchment/90 backdrop-blur-sm rounded px-2 py-1 font-sans animate-pulse">
                        "Loading..."
                    </span>
                </div>
            })}

            // Empty state (hidden when detail panel is open)
            {move || (!loading.get() && empty.get() && !panel_open()).then(|| view! {
                <div class="absolute bottom-3 left-3 pointer-events-none">
                    <span class="text-xs text-sepia/70 bg-parchment/90 backdrop-blur-sm rounded px-2 py-1 font-sans">
                        "No entities in this area"
                    </span>
                </div>
            })}

            // Announce entity availability to screen readers
            {move || announced.get().then(|| view! {
                <span class="sr-only">"Entities loaded"</span>
            })}

            // Fetch error with retry button (hidden when detail panel is open)
            {move || if panel_open() { None } else { fetch_error.get() }.map(|msg| {
                let on_retry = move |_| retry.set(true);
                view! {
                    <div class="absolute bottom-3 left-3 pointer-events-auto">
                        <div class="bg-red-600/90 text-white rounded px-3 py-2 text-xs font-sans flex items-center gap-2">
                            <span class="truncate max-w-xs md:max-w-md">{msg}</span>
                            <button
                                class="bg-white/20 hover:bg-white/30 rounded px-2 py-1 cursor-pointer shrink-0"
                                on:click=on_retry
                                aria-label="Retry loading entities"
                            >
                                "Retry"
                            </button>
                        </div>
                    </div>
                }
            })}
        </div>
    }.into_any()
}
