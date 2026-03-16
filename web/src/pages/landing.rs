use leptos::prelude::*;

use crate::components::map::MapView;

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

    view! {
        // Map fills the entire main area — no scrolling.
        // Mobile: subtract the 3.5rem top bar. Desktop: full viewport (sidebar is flex, not stacked).
        <div class="h-[calc(100vh-3.5rem)] md:h-screen relative">
            <MapView/>

            // "Coming soon" watermark centered on the map
            <div class="absolute inset-0 flex items-center justify-center pointer-events-none">
                <p class="text-5xl font-bold text-ink/30 font-sans tracking-widest uppercase select-none">
                    "Coming Soon"
                </p>
            </div>

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
                                <button
                                    class="text-sepia/50 hover:text-ink text-lg leading-none cursor-pointer ml-4"
                                    on:click=dismiss
                                    aria-label="Close"
                                >
                                    "\u{2715}"
                                </button>
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
