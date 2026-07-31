use std::cell::RefCell;
use std::rc::Rc;

use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::api::Client;
use crate::components::controls::OVERLAY_CONTROL;
use crate::components::entity_detail::{EntityDetailPanel, LightboxState};
use crate::components::focus_trap::contain_tab;
use crate::components::map::{MapStatus, MapView, SelectedEntity};
use crate::components::map_card::{Corner, MapCard};
use crate::components::motion::REVEAL;
use crate::components::time_slider::TimeSlider;

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

    // Lightbox state — provided via context so the overlay renders here, clear
    // of the detail panel's slide transform, which would otherwise anchor its
    // `position: fixed` to the panel instead of the viewport.
    let (lightbox_content, set_lightbox_content) =
        signal(None::<crate::components::entity_detail::LightboxContent>);
    let lightbox = LightboxState(lightbox_content, set_lightbox_content);
    provide_context(lightbox.clone());

    view! {
        // Map fills the viewport — no scrolling, and no chrome to subtract now
        // that the nav floats over it. `dvh` rather than `vh` so mobile browser
        // toolbars shrink the map instead of pushing its bottom off-screen.
        <div class="h-dvh relative">
            <MapView api_client=api_client.clone() map_handle=map_handle/>

            // Entity detail panel (slides in from right on marker click)
            <EntityDetailPanel api_client=api_client.clone()/>

            // One column owns the map's bottom-left corner, on the same 12 px
            // inset every other floating gizmo uses. The time card and the
            // status chips are flow children of it, so the chips ride whatever
            // height the card takes rather than being told a number that a
            // taller card silently invalidates.
            //
            // Spacing belongs to the chips rather than to a `gap` here: the
            // status overlay is always mounted for its live region, and a gap
            // would stand as a stripe of nothing above the card whenever there
            // is nothing to announce.
            //
            // Clicks pass through to the map, and each occupant that wants them
            // takes them back. The column is as wide as its widest row, so
            // otherwise the space beside a short chip would swallow map drags.
            //
            // On the controls rung: the time card is what the reader is working,
            // and on a phone it grows tall enough to meet the About card.
            <div class=format!(
                "absolute bottom-3 left-3 {OVERLAY_CONTROL} flex flex-col \
                 items-start pointer-events-none"
            )>
                <MapStatusOverlay/>

                // The time control — rewinds the map to a historical moment.
                <TimeSlider/>
            </div>

            // Dismissible info card — collapsible "about" overlay for new visitors.
            <AboutCard/>

            // Image lightbox overlay — rendered here, outside the detail panel,
            // so `position: fixed` resolves against the viewport rather than
            // the panel's transform.
            <ImageLightbox lightbox=lightbox/>
        </div>
    }
}

/// The dismissible "about" overlay for new visitors.
///
/// Steps aside while an entity is selected. On desktop the detail panel is a
/// right-hand sheet whose own close button lands in this same corner, and this
/// card draws later, so it would cover the panel's dismiss. The map status chips
/// yield to the panel for the same reason.
///
/// A component rather than markup in `Landing` because `SelectedEntity` is
/// provided by `MapView`: only something rendered after it can read the context.
#[component]
fn AboutCard() -> impl IntoView {
    // Mirrored to localStorage so a returning visitor keeps the clean map they
    // left. `MapCard` owns no persistence — this is the one caller that wants any.
    let info_open = RwSignal::new(!is_info_dismissed());
    Effect::new(move |_| set_info_dismissed(!info_open.get()));

    let SelectedEntity(selected, _) = expect_context::<SelectedEntity>();

    view! {
        <Show when=move || selected.get().is_none()>
            <MapCard
                corner=Corner::TopRight
                open=info_open
                chip_class="rounded-full w-8 h-8 text-sm font-semibold"
                expand_label="About Chronoscope"
                collapse_label="Hide the About panel"
                body_id="about-card"
                body_radius="rounded-xl"
                chip=move || view! {
                    {move || if info_open.get() { "\u{2715}" } else { "?" }}
                }
            >
                // `pr-12` keeps the heading clear of the toggle, which sits over
                // the card's top-right corner rather than in this flow.
                //
                // No `text-wrap` override. `pretty` measured line-for-line
                // identical to the default here, and `balance` equalises within
                // a block but lets each block settle on its own width, so the
                // paragraphs and bullets stopped sharing a right margin.
                <div class="p-5 pr-12 w-[calc(100vw-1.5rem)] md:w-[30rem]">
                    <h2 class="text-sm font-semibold font-sans text-copper mb-3">"About Chronoscope"</h2>
                    <p class="text-sm text-sepia leading-relaxed mb-2">
                        "Pull up historical photographs of the street you\u{2019}re on, see a "
                        "demolished building as it once stood, or work out what that ruin on the "
                        "hillside used to be. Chronoscope connects the built world to the "
                        "photographs, maps, and records that document it, through time."
                    </p>
                    <p class="text-sm text-sepia leading-relaxed mb-4">
                        "This is an early technical preview. Much of the planned functionality "
                        "isn\u{2019}t wired up yet, but you\u{2019}re welcome to look around."
                    </p>
                    <div class="space-y-1.5 text-xs text-sepia/80">
                        <p><span class="text-copper font-semibold">"Explicit uncertainty"</span>
                            ": a date like \u{201c}circa 1920s\u{201d} stays vague until evidence narrows it. No false precision."</p>
                        <p><span class="text-copper font-semibold">"Pervasive citations"</span>
                            ": every assertion points back to the photograph, map, or record behind it."</p>
                        <p><span class="text-copper font-semibold">"Compounding evidence"</span>
                            ": one dated photograph bounds everything else in it. Claims that can\u{2019}t hold simultaneously are flagged for review."</p>
                    </div>
                </div>
            </MapCard>
        </Show>
    }
}

/// Fullscreen image preview overlay, centered in the browser viewport.
#[component]
fn ImageLightbox(lightbox: LightboxState) -> impl IntoView {
    let set_content = lightbox.1;
    let close = move |_: leptos::ev::MouseEvent| set_content.set(None);
    let close_key = move |ev: leptos::ev::KeyboardEvent| match ev.key().as_str() {
        "Escape" => set_content.set(None),
        "Tab" => {
            if let Some(dialog) = ev
                .current_target()
                .and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok())
            {
                contain_tab(&ev, &[&dialog]);
            }
        }
        _ => {}
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
                    class=format!("fixed inset-0 z-[100] bg-ink/90 flex items-center \
                                   justify-center p-8 {REVEAL}")
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
                    // Close sits top-right, where every other close in the app
                    // lives; the source link takes the opposite corner.
                    <button
                        class="absolute top-4 right-4 text-parchment/80 hover:text-parchment text-xl font-sans \
                               bg-ink/60 rounded-full w-11 h-11 flex items-center justify-center cursor-pointer"
                        on:click=close
                        aria-label="Close preview"
                    >
                        "\u{2715}"
                    </button>
                    <a
                        href=c.source_url.clone()
                        target="_blank"
                        rel="noopener noreferrer"
                        class="absolute top-4 left-4 text-parchment/80 hover:text-parchment text-sm font-sans \
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

    // Each chip carries its own bottom margin, so an overlay with nothing to
    // say takes no room in the column it sits in.
    let chip = "mb-2 text-xs text-sepia/70 bg-parchment/90 backdrop-blur-sm \
                rounded px-2 py-1 font-sans";

    view! {
        <div aria-live="polite">
            // Loading indicator (hidden when detail panel is open)
            {move || (loading.get() && !panel_open()).then(|| view! {
                <div class=format!("{chip} animate-pulse")>"Loading..."</div>
            })}

            // Empty state (hidden when detail panel is open)
            {move || (!loading.get() && empty.get() && !panel_open()).then(|| view! {
                <div class=chip>"No entities in this area"</div>
            })}

            // Announce entity availability to screen readers
            {move || announced.get().then(|| view! {
                <span class="sr-only">"Entities loaded"</span>
            })}

            // Fetch error with retry button (hidden when detail panel is open)
            {move || if panel_open() { None } else { fetch_error.get() }.map(|msg| {
                let on_retry = move |_| retry.set(true);
                view! {
                    // Takes its clicks back from the column, which waves them
                    // through to the map: the Retry button is the one thing in
                    // here anybody presses.
                    <div class="mb-2 pointer-events-auto bg-red-600/90 text-white rounded \
                                px-3 py-2 text-xs font-sans flex items-center gap-2">
                        <span class="truncate max-w-xs md:max-w-md">{msg}</span>
                        <button
                            class="bg-white/20 hover:bg-white/30 rounded px-2 py-1 cursor-pointer shrink-0"
                            on:click=on_retry
                            aria-label="Retry loading entities"
                        >
                            "Retry"
                        </button>
                    </div>
                }
            })}
        </div>
    }.into_any()
}
