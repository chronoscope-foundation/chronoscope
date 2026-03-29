use std::cell::RefCell;
use std::rc::Rc;

use leptos::prelude::*;

use crate::api::ApiClient;
use crate::components::dismiss_button::DismissButton;
use crate::components::entity_detail::EntityDetailPanel;
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
    let api_client: Rc<RefCell<Option<ApiClient>>> = Rc::new(RefCell::new(None));

    view! {
        // Map fills the entire main area — no scrolling.
        // Mobile: subtract the 3.5rem top bar. Desktop: full viewport (sidebar is flex, not stacked).
        <div class="h-[calc(100vh-3.5rem)] md:h-screen relative">
            <MapView api_client=api_client.clone()/>

            // Entity detail panel (slides in from right on marker click)
            <EntityDetailPanel api_client=api_client.clone()/>

            // Map status overlays (loading, empty, truncated)
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
        </div>
    }
}

/// Renders map status overlays: loading indicator, empty state, and truncation banner.
#[component]
fn MapStatusOverlay() -> impl IntoView {
    let Some(status) = use_context::<MapStatus>() else {
        return view! { <div/> }.into_any();
    };
    let loading = status.loading;
    let empty = status.empty;
    let truncated = status.truncated;
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

            // Truncation banner (hidden when detail panel is open)
            {move || (!panel_open()).then(|| view! {
                <TruncatedBanner truncated=truncated/>
            })}
        </div>
    }.into_any()
}

/// Dismissible banner shown when the entity cap (500) is hit.
#[component]
fn TruncatedBanner(truncated: ReadSignal<bool>) -> impl IntoView {
    let (dismissed, set_dismissed) = signal(false);

    // Reset dismissal when truncated state changes (e.g., user pans to new area)
    Effect::new(move || {
        let _ = truncated.get();
        set_dismissed.set(false);
    });

    let dismiss = move |_| {
        set_dismissed.set(true);
    };

    view! {
        {move || (truncated.get() && !dismissed.get()).then(|| view! {
            <div class="absolute bottom-3 left-1/2 -translate-x-1/2 pointer-events-auto">
                <div class="bg-parchment/95 backdrop-blur-sm rounded-lg shadow-md px-4 py-2 flex items-center gap-3 font-sans text-xs text-sepia">
                    <span>"Showing first 500 entities. Zoom in for more detail."</span>
                    <DismissButton on_click=dismiss label="Dismiss" extra_class="shrink-0"/>
                </div>
            </div>
        })}
    }
}
