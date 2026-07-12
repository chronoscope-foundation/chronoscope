use std::cell::RefCell;
use std::rc::Rc;

use leptos::prelude::*;
use send_wrapper::SendWrapper;
use wasm_bindgen::JsCast;

use crate::api;
use crate::components::dismiss_button::DismissButton;
use crate::components::map::{EntityPickerEntry, EntitySelection, SelectedEntity};

/// Content currently displayed in the lightbox overlay.
#[derive(Clone, Debug)]
pub struct LightboxContent {
    /// URL the overlay `<img>` loads (the resolved `display_url`).
    pub url: String,
    /// Alt text for accessibility.
    pub alt: String,
    /// Real upstream URL, for the "Open original" link.
    pub source_url: String,
}

/// Lightbox state provided via context so the overlay can render
/// outside the sidebar's CSS transform (which breaks `position: fixed`).
///
/// A single `Option<LightboxContent>` signal avoids partial-update races
/// that would occur with separate signals for `url`/`alt`/`source_url`.
#[derive(Clone)]
pub struct LightboxState(
    pub ReadSignal<Option<LightboxContent>>,
    pub(crate) WriteSignal<Option<LightboxContent>>,
);

impl LightboxState {
    /// Open the lightbox with the given content.
    pub fn open(&self, content: LightboxContent) {
        self.1.set(Some(content));
    }
}

/// Side panel showing entity details or a disambiguation picker.
///
/// The panel is always rendered (for CSS transitions) but translated off-screen
/// when nothing is selected. On desktop it slides in from the right; on mobile
/// it slides up as a bottom sheet.
#[component]
pub fn EntityDetailPanel(api_client: Rc<RefCell<Option<api::Client>>>) -> impl IntoView {
    let api_client = SendWrapper::new(api_client);
    let SelectedEntity(selected, set_selected) = expect_context::<SelectedEntity>();
    let panel_ref = NodeRef::<leptos::html::Div>::new();

    // Focus the panel when selection opens; return focus to the main content
    // area when it closes (so screen reader users aren't stranded on a hidden
    // off-screen panel).
    Effect::new(move || {
        if selected.get().is_some() {
            if let Some(el) = panel_ref.get() {
                let _ = el.focus();
            }
        } else if let Some(main) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.get_element_by_id("main-content"))
            && let Ok(el) = main.dyn_into::<web_sys::HtmlElement>()
        {
            let _ = el.focus();
        }
    });

    let dismiss = move |_| {
        set_selected.set(None);
    };

    let on_keydown = move |ev: leptos::ev::KeyboardEvent| {
        if ev.key() == "Escape" {
            set_selected.set(None);
        }
    };

    view! {
        <div
            node_ref=panel_ref
            tabindex="-1"
            // Mobile bottom sheet, desktop side sheet with slide transitions
            class=move || {
                let base = "fixed bg-parchment/95 backdrop-blur-sm shadow-lg z-10 overflow-y-auto pointer-events-auto \
                            outline-none transition-transform duration-200 \
                            bottom-0 left-0 right-0 h-2/3 rounded-t-xl \
                            md:top-0 md:right-0 md:bottom-0 md:left-auto md:h-full md:w-96 md:rounded-none";
                if selected.get().is_some() {
                    format!("{base} translate-y-0 md:translate-y-0 md:translate-x-0")
                } else {
                    format!("{base} translate-y-full md:translate-y-0 md:translate-x-full")
                }
            }
            role="complementary"
            aria-label="Entity detail panel"
            on:keydown=on_keydown
        >
            {move || {
                selected.get().map(|selection| {
                    let title = match &selection {
                        EntitySelection::Single { .. } => "Entity Details",
                        EntitySelection::Multiple(_) => "Multiple Entities",
                    };
                    view! {
                        <div class="p-5">
                            <div class="flex justify-between items-start mb-4">
                                <h2 class="text-lg font-semibold font-sans text-ink">{title}</h2>
                                <DismissButton on_click=dismiss extra_class="ml-4"/>
                            </div>
                            {match selection {
                                EntitySelection::Single { detail: id, back: back_entries, .. } => view! {
                                    <div>
                                        {back_entries.map(|entries| {
                                            let go_back = move |_| {
                                                set_selected.set(Some(EntitySelection::Multiple(entries.clone())));
                                            };
                                            view! {
                                                <button
                                                    class="text-sm text-ink hover:underline cursor-pointer mb-3 flex items-center gap-1"
                                                    on:click=go_back
                                                >
                                                    "\u{2190} Back to list"
                                                </button>
                                            }
                                        })}
                                        <EntityDetailContent id=id api_client=Rc::clone(&api_client)/>
                                    </div>
                                }.into_any(),
                                EntitySelection::Multiple(entries) => view! {
                                    <EntityPicker entries=entries.clone() />
                                }.into_any(),
                            }}
                        </div>
                    }
                })
            }}
        </div>
    }
}

/// Disambiguation picker for co-located entities.
#[component]
fn EntityPicker(entries: Vec<EntityPickerEntry>) -> impl IntoView {
    let SelectedEntity(_, set_selected) = expect_context::<SelectedEntity>();
    let all_entries = Rc::new(entries.clone());

    view! {
        <p class="text-sm text-sepia mb-3">
            "Multiple entities share this location. Select one:"
        </p>
        <ul class="space-y-2">
            {entries.into_iter().map(|entry| {
                let id = entry.id;
                let back = Rc::clone(&all_entries);
                let select = move |_| {
                    // The marker's feature id is its representative — the first
                    // (earliest) entry the server sorted the group by, which is
                    // also `marker.id`. Highlight that, whichever member is picked.
                    let feature = back
                        .first()
                        .map(|e| e.id.clone())
                        .unwrap_or_else(|| id.clone());
                    set_selected.set(Some(EntitySelection::Single {
                        detail: id.clone(),
                        feature,
                        back: Some((*back).clone()),
                    }));
                };
                let display_name = entry
                    .name
                    .clone()
                    .unwrap_or_else(|| "Unknown".to_string());
                let aria = display_name.clone();
                view! {
                    <li>
                        <button
                            class="w-full text-left p-3 rounded-lg bg-parchment hover:bg-copper/10 \
                                   border border-copper/20 cursor-pointer transition-colors"
                            on:click=select
                            aria-label=aria
                        >
                            <span class="text-sm font-semibold text-ink">{display_name}</span>
                        </button>
                    </li>
                }
            }).collect::<Vec<_>>()}
        </ul>
    }
}

/// Fetches and displays entity detail content.
#[component]
fn EntityDetailContent(
    id: EntityId,
    api_client: Rc<RefCell<Option<api::Client>>>,
) -> impl IntoView {
    let (retry_count, set_retry_count) = signal(0u32);
    let detail = LocalResource::new({
        let api_client = api_client.clone();
        let id = id.clone();
        move || {
            // Include retry_count in the dependency so incrementing it re-fetches.
            let _retry = retry_count.get();
            let api = api_client.clone();
            // The resource re-runs on each dependency change, so hand the future
            // its own id clone rather than moving the captured one out.
            let id = id.clone();
            async move {
                let client = crate::api::get_or_init_client(&api)
                    .await
                    .ok_or_else(|| "Failed to load API configuration".to_string())?;
                fetch_entity_detail(&id, &client).await
            }
        }
    });

    // The depicting images load page-by-page into `media`: the first page
    // eagerly, further pages when the reader clicks "Load more". `next_cursor` is
    // the resume token for the following page (`None` once the grid is
    // exhausted); `loading` guards against overlapping fetches. `page_req` is the
    // "Load more" trigger — a plain counter the (Send-bound) grid button bumps,
    // so the button never has to capture the `!Send` API client.
    let (media, set_media) = signal(Vec::<MediaInfo>::new());
    let (next_cursor, set_next_cursor) = signal(Option::<api::Cursor>::None);
    let (loading, set_loading) = signal(false);
    let (page_req, set_page_req) = signal(0u32);
    // The error for the images section, `None` while healthy. A failed fetch
    // surfaces here; a retryable error offers a retry, a terminal one (the
    // entity no longer exists) reports without one so the reader isn't stranded
    // re-triggering a fetch that can only fail again.
    let (images_error, set_images_error) = signal(Option::<ImagesFetchError>::None);

    // Fetch one page past `cursor`, pinned to `snapshot`, and fold it into the
    // accumulator. Holds the `!Send` API client, so it lives here in the
    // component body rather than in the view.
    let load_page = move |cursor: Option<api::Cursor>, snapshot: Option<api::Snapshot>| {
        if loading.get_untracked() {
            return;
        }
        set_loading.set(true);
        set_images_error.set(None);
        let api = api_client.clone();
        let id = id.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let result = match crate::api::get_or_init_client(&api).await {
                Some(client) => {
                    fetch_entity_images_page(&id, cursor.as_ref(), snapshot.as_ref(), &client).await
                }
                None => Err(ImagesFetchError::Other(
                    "Failed to load API configuration".to_string(),
                )),
            };
            match result {
                Ok((tiles, next)) => {
                    set_media.update(|acc| acc.extend(tiles));
                    set_next_cursor.set(next);
                    set_loading.set(false);
                }
                // A snapshot-pinned cursor never goes stale, so a load-more reads
                // the past snapshot where the entity existed and can't 404. A
                // page-1 fetch (at the live snapshot) still can, if the entity was
                // deleted after the detail load — that's terminal. The stored
                // error carries whether a retry could help.
                Err(e) => {
                    set_images_error.set(Some(e));
                    set_loading.set(false);
                }
            }
        });
    };

    // Page 1 waits for the entity detail to resolve successfully: a failed or
    // missing entity renders its own error and depicts nothing, so gating the
    // first fetch on a loaded detail keeps a 404 from drawing a wasted image
    // request. Each "Load more" bump (`req > 0`) pages past the stored resume
    // cursor regardless of the detail.
    Effect::new(move |_| {
        let req = page_req.get();
        if req > 0 {
            // Load-more resumes the cursor, which already pins the page-1
            // snapshot; no separate snapshot needed.
            load_page(next_cursor.get_untracked(), None);
        } else if let Some(Ok(view)) = detail.get() {
            // Page 1 pins to the detail's snapshot so the grid reads the same
            // point the detail was projected at.
            load_page(None, Some(view.snapshot.clone()));
        }
    });

    view! {
        <div aria-live="polite">
        <Suspense fallback=move || view! {
            <p class="text-sm text-sepia/60">"Loading..."</p>
        }>
            {move || {
                detail.get().map(|result| {
                    match result {
                        Ok(entity) => view! {
                            <div>
                                <h3 class="text-base font-semibold text-ink mb-3">
                                    {entity.name.clone().unwrap_or_else(|| "Unnamed entity".to_string())}
                                </h3>

                                // Timeline
                                {(!entity.timeline.is_empty()).then(|| {
                                    let count = entity.timeline.len();
                                    view! {
                                    <div class="mb-3">
                                        <p class="text-xs font-sans text-copper font-semibold mb-1">
                                            {format!("Timeline ({count})")}
                                        </p>
                                        <ul class="text-sm text-sepia space-y-1.5">
                                            {entity.timeline.iter().map(|r| {
                                                let date_view = match r.date.as_ref() {
                                                    Some(d) => view! {
                                                        <span class="text-sepia/70">
                                                            {format!(" \u{2014} {}", format_uncertain_date(d))}
                                                        </span>
                                                    }.into_any(),
                                                    None => view! {
                                                        <span class="text-sepia/40 italic">
                                                            " \u{2014} date unknown"
                                                        </span>
                                                    }.into_any(),
                                                };
                                                view! {
                                                    <li class="pl-2 border-l-2 border-copper/30">
                                                        <span class="font-semibold">{r.label.clone()}</span>
                                                        {date_view}
                                                        {r.description.as_ref().map(|desc| view! {
                                                            <p class="text-xs text-sepia/70 mt-0.5">{desc.clone()}</p>
                                                        })}
                                                    </li>
                                                }
                                            }).collect::<Vec<_>>()}
                                        </ul>
                                    </div>
                                    }
                                })}

                                // Images — a separate paginated fetch. The section
                                // shows whenever there are tiles, another page to
                                // fetch, or a recoverable error to retry, so a
                                // first page that fully filters out still surfaces
                                // "Load more" instead of stranding an empty grid.
                                {move || {
                                    let tiles = media.get();
                                    let has_more = next_cursor.get().is_some();
                                    let error = images_error.get();
                                    let show = !tiles.is_empty() || has_more || error.is_some();
                                    show.then(move || {
                                        let count = tiles.len();
                                        // Only a retryable error offers a retry; a
                                        // terminal error (the entity no longer
                                        // exists) doesn't, and neither does a
                                        // healthy "more to load".
                                        let retryable = matches!(error, Some(ImagesFetchError::Other(_)));
                                        let button_label = if retryable { "Retry" } else { "Load more" };
                                        let show_button = has_more || retryable;

                                        view! {
                                        <div class="mb-3">
                                            {(!tiles.is_empty()).then(move || {
                                                let lightbox = expect_context::<LightboxState>();
                                                view! {
                                                    <p class="text-xs font-sans text-copper font-semibold mb-1">
                                                        {format!("Images ({count} loaded)")}
                                                    </p>
                                                    <ul class="grid grid-cols-2 gap-2" role="list">
                                                        {tiles.iter().map(|m| {
                                                            let alt = format!("{} view", m.label);
                                                            let aria = format!("{alt} \u{2014} opens preview");
                                                            let content = LightboxContent {
                                                                url: m.display_url.clone(),
                                                                alt: alt.clone(),
                                                                source_url: m.source_url.clone(),
                                                            };
                                                            let lb = lightbox.clone();
                                                            let on_click = move |_: leptos::ev::MouseEvent| {
                                                                lb.open(content.clone());
                                                            };
                                                            view! {
                                                                <li>
                                                                    <button
                                                                        class="relative w-full rounded-md overflow-hidden shadow-sm \
                                                                               ring-1 ring-sepia/10 cursor-pointer \
                                                                               hover:scale-[1.02] transition-transform"
                                                                        on:click=on_click
                                                                        aria-label=aria
                                                                    >
                                                                        // Loading placeholder (visible until image loads)
                                                                        <div class="w-full aspect-square bg-sepia/10 animate-pulse absolute inset-0"/>
                                                                        <img
                                                                            src={m.display_url.clone()}
                                                                            alt=alt
                                                                            loading="lazy"
                                                                            class="w-full aspect-square object-cover relative"
                                                                        />
                                                                        // Caption badge (bottom-right pill)
                                                                        <span class="absolute bottom-1 right-1 px-1.5 py-0.5 \
                                                                                     bg-ink/70 text-white text-xs font-sans \
                                                                                     rounded-full">
                                                                            {m.label.clone()}
                                                                        </span>
                                                                    </button>
                                                                </li>
                                                            }
                                                        }).collect::<Vec<_>>()}
                                                    </ul>
                                                }
                                            })}
                                            {error.map(|e| {
                                                let text = match e {
                                                    ImagesFetchError::Gone => {
                                                        "Images are no longer available; this entity no longer exists.".to_string()
                                                    }
                                                    ImagesFetchError::Other(m) => {
                                                        format!("Couldn\u{2019}t load images: {m}")
                                                    }
                                                };
                                                view! {
                                                    <p class="text-sm text-red-600 mt-2">{text}</p>
                                                }
                                            })}
                                            {show_button.then(move || view! {
                                                <button
                                                    class="mt-2 w-full text-sm text-ink hover:underline \
                                                           cursor-pointer disabled:opacity-50 disabled:cursor-default"
                                                    on:click=move |_: leptos::ev::MouseEvent| {
                                                        set_page_req.update(|n| *n += 1);
                                                    }
                                                    disabled=move || loading.get()
                                                    aria-label="Load more images"
                                                >
                                                    {button_label}
                                                </button>
                                            })}
                                        </div>
                                        }
                                    })
                                }}

                                // Links
                                {(!entity.links.is_empty()).then(|| {
                                    let count = entity.links.len();
                                    view! {
                                    <div class="mb-3">
                                        <p class="text-xs font-sans text-copper font-semibold mb-1">
                                            {format!("Links ({count})")}
                                        </p>
                                        <ul class="text-sm space-y-1">
                                            {entity.links.iter().map(|link| view! {
                                                <li>
                                                    <a
                                                        href={link.url.clone()}
                                                        target="_blank"
                                                        rel="noopener noreferrer"
                                                        class="text-copper hover:underline"
                                                    >
                                                        {link.label.clone()}
                                                        <span aria-hidden="true" class="text-[0.75em]">" \u{2197}"</span>
                                                    </a>
                                                </li>
                                            }).collect::<Vec<_>>()}
                                        </ul>
                                    </div>
                                    }
                                })}
                            </div>
                        }.into_any(),
                        Err(e) => {
                            let on_retry = move |_| set_retry_count.update(|c| *c += 1);
                            view! {
                                <div>
                                    <p class="text-sm text-red-600 mb-2">{format!("Error: {e}")}</p>
                                    <button
                                        class="text-sm text-ink hover:underline cursor-pointer"
                                        on:click=on_retry
                                        aria-label="Retry loading entity details"
                                    >
                                        "Retry"
                                    </button>
                                </div>
                            }.into_any()
                        }
                    }
                })
            }}
        </Suspense>
        </div>
    }
}

// ==================== Lightweight detail types ====================

/// One row in the rendered entity timeline.
///
/// Each row is one ordered [`Moment`]: a durational endpoint (construction,
/// demolition, or an interior span like modification or repair) or a
/// point-shaped interior event (usage change, designation). A durational whose
/// endpoints are both dateless collapses to a single bare row.
#[derive(Debug, Clone)]
struct TimelineRow {
    label: String,
    /// `None` when the date for this row is unknown.
    date: Option<UncertainDate>,
    /// Optional secondary text shown beneath the row (free-text
    /// descriptions authored on the underlying event, plus — for
    /// `Designated` — the settled designation text).
    description: Option<String>,
}

#[derive(Debug, Clone)]
struct LinkInfo {
    label: String,
    url: String,
}

/// One image in the detail grid: the URL the grid/lightbox load
/// (`display_url`), the real upstream URL for the "Open original" link
/// (`source_url`), and a short caption.
#[derive(Debug, Clone)]
struct MediaInfo {
    display_url: String,
    source_url: String,
    label: String,
}

#[derive(Debug, Clone)]
struct EntityDetailView {
    name: Option<String>,
    timeline: Vec<TimelineRow>,
    links: Vec<LinkInfo>,
    /// The read-consistency point the detail was served at. Passed to the
    /// images fetch so the grid reads the same state as the detail.
    snapshot: api::Snapshot,
}

use std::collections::BTreeSet;
use std::num::NonZeroU32;

use chronoscope_core::Claimed;
use chronoscope_core::date::{DateBound, DatePrecision, UncertainDate};
use chronoscope_core::grammar::citations::ExternalReference;
use chronoscope_core::grammar::lifecycle::{DamageCause, MoveMethod, Usage};
use chronoscope_core::location::{LocationReference, UnresolvedLocation};
use chronoscope_core::moment::TransitionRole;
use chronoscope_core::typed::{Attributed, Bounded, EventDetail, InteriorEvent, MomentView};

use chronoscope_api_client::{EntityId, EventId, ImageId};

/// Image tiles fetched per grid page. The panel loads the first page eagerly and
/// appends further pages on demand via "Load more".
const IMAGES_PAGE_SIZE: NonZeroU32 = match NonZeroU32::new(24) {
    Some(n) => n,
    None => NonZeroU32::MIN,
};

/// Fetch entity detail using the typed API client and flatten it into the
/// view model the panel renders. The depicting images are a separate paginated
/// fetch ([`fetch_entity_images_page`]).
async fn fetch_entity_detail(
    id: &EntityId,
    client: &api::Client,
) -> Result<EntityDetailView, String> {
    let api::EntityDetail {
        entity,
        display_name,
        conflicts: _,
        snapshot,
    } = client.get_entity(id).await.map_err(|e| e.to_string())?;

    let timeline = entity.timeline.moments().map(moment_row).collect();
    let links = entity
        .external_refs
        .iter()
        .filter_map(|r| link_info(&r.value))
        .collect();

    Ok(EntityDetailView {
        name: display_name,
        timeline,
        links,
        snapshot,
    })
}

/// Why an image-page fetch failed, split by whether a retry can help.
#[derive(Clone)]
enum ImagesFetchError {
    /// The entity no longer exists — a 404. Terminal: a snapshot-pinned cursor
    /// never goes stale, so only a page-1 read (at the live snapshot) can hit
    /// this, when the entity was deleted after the detail load. A retry would
    /// just 404 again.
    Gone,
    /// Any other failure; surfaced with a retry affordance.
    Other(String),
}

/// Fetch one page of an entity's depicting images, flattening each tile into a
/// [`MediaInfo`] and returning the resume cursor for the next page (`None` once
/// the grid is exhausted).
async fn fetch_entity_images_page(
    id: &EntityId,
    cursor: Option<&api::Cursor>,
    snapshot: Option<&api::Snapshot>,
    client: &api::Client,
) -> Result<(Vec<MediaInfo>, Option<api::Cursor>), ImagesFetchError> {
    let page = client
        .get_entity_images(id, IMAGES_PAGE_SIZE, cursor, snapshot)
        .await
        .map_err(|e| match e {
            // A snapshot-pinned cursor never goes stale and the client always
            // sends a valid limit, so a 404 here means the entity was deleted
            // between the detail load and this fetch.
            api::ApiError::Api { status: 404, .. } => ImagesFetchError::Gone,
            other => ImagesFetchError::Other(other.to_string()),
        })?;
    let tiles = page
        .images
        .into_iter()
        .map(|img| MediaInfo {
            display_url: img.display_url.into(),
            source_url: img.source_url.into(),
            label: api::image_caption(img.perspective, img.medium),
        })
        .collect();
    Ok((tiles, page.next))
}

// ==================== Moment → row ====================

/// Map one ordered [`MomentView`] to its display row: the role's label (bare when
/// a durational pair collapsed to a single undated moment), the endpoint's date,
/// and — when this moment carries it — the event's secondary text.
fn moment_row(moment: MomentView<'_, EventId, ImageId>) -> TimelineRow {
    TimelineRow {
        label: moment_label(moment.role, moment.collapsed).to_string(),
        date: moment.date.map(|b| b.possible.clone()),
        description: moment
            .carries_description
            .then(|| entry_description(&moment.event.detail))
            .flatten(),
    }
}

/// The label for a moment's role. The durational start roles read as the bare
/// verb when collapsed (both endpoints dateless); otherwise start and end roles
/// read as "… started" / "… completed", and point roles carry their own phrase.
fn moment_label(role: TransitionRole, collapsed: bool) -> &'static str {
    use TransitionRole::{
        Ambiguous, ConstructionEnd, ConstructionStart, DamagedEnd, DamagedStart, DemolitionEnd,
        DemolitionStart, Designated, ModificationEnd, ModificationStart, MovedEnd, MovedStart,
        RepairEnd, RepairStart, UsageModified,
    };
    match (role, collapsed) {
        (ConstructionStart, true) => "Constructed",
        (ModificationStart, true) => "Modified",
        (RepairStart, true) => "Repaired",
        (DamagedStart, true) => "Damaged",
        (MovedStart, true) => "Moved",
        (DemolitionStart, true) => "Demolished",
        (ConstructionStart, false) => "Construction started",
        (ConstructionEnd, _) => "Construction completed",
        (ModificationStart, false) => "Modification started",
        (ModificationEnd, _) => "Modification completed",
        (RepairStart, false) => "Repair started",
        (RepairEnd, _) => "Repair completed",
        (DamagedStart, false) => "Damage started",
        (DamagedEnd, _) => "Damage completed",
        (MovedStart, false) => "Move started",
        (MovedEnd, _) => "Move completed",
        (DemolitionStart, false) => "Demolition started",
        (DemolitionEnd, _) => "Demolition completed",
        (UsageModified, _) => "Usage changed",
        (Designated, _) => "Designated",
        (Ambiguous, _) => "Event",
    }
}

/// The secondary line for a timeline entry: the kind-specific summary (a damage
/// cause, a move's method and destination, a usage set, a designation) joined
/// with the entry's free-text descriptions. Bookends carry none.
fn entry_description(detail: &EventDetail<EventId, ImageId>) -> Option<String> {
    let (descriptions, kind) = match detail {
        EventDetail::Constructed { .. } | EventDetail::Demolished { .. } => return None,
        EventDetail::Interior {
            descriptions, kind, ..
        } => (descriptions, kind),
    };
    let free = join_descriptions(descriptions);
    match kind {
        InteriorEvent::Modified { .. } | InteriorEvent::Repaired { .. } => free,
        InteriorEvent::Damaged { cause, .. } => {
            combine_description(cause.settled().map(damage_cause_label), free)
        }
        InteriorEvent::Moved { method, to, .. } => {
            combine_description(move_summary(method, to), free)
        }
        InteriorEvent::UsageChanged { usages, .. } => {
            combine_description(usages.settled().map(usage_set_label), free)
        }
        InteriorEvent::Designated { designation, .. } => {
            combine_description(settled_designation(designation), free)
        }
        InteriorEvent::Ambiguous { .. } => free,
    }
}

/// Join an interior event's free-text descriptions into one secondary line.
fn join_descriptions(descriptions: &[Attributed<String, ImageId>]) -> Option<String> {
    if descriptions.is_empty() {
        return None;
    }
    Some(
        descriptions
            .iter()
            .map(|d| d.value.as_str())
            .collect::<Vec<_>>()
            .join("; "),
    )
}

/// The designation text when the claim settled to exactly one value —
/// conflicting or unsettled designations render dateless label + date only,
/// rather than guessing among rivals.
fn settled_designation(designation: &Bounded<Claimed<String>, ImageId>) -> Option<String> {
    designation.settled().cloned()
}

/// Combine a structured primary description (e.g. a settled designation)
/// with the entry's free-text descriptions.
fn combine_description(primary: Option<String>, extra: Option<String>) -> Option<String> {
    match (primary, extra) {
        (Some(a), Some(b)) => Some(format!("{a} \u{2014} {b}")),
        (Some(a), None) => Some(a),
        (None, b) => b,
    }
}

/// A human-readable label for a settled damage cause. `Other` surfaces its
/// authored free-text description verbatim.
fn damage_cause_label(cause: &DamageCause) -> String {
    match cause {
        DamageCause::Earthquake => "Earthquake".to_string(),
        DamageCause::Fire => "Fire".to_string(),
        DamageCause::Flood => "Flood".to_string(),
        DamageCause::Neglect => "Neglect".to_string(),
        DamageCause::Structural => "Structural failure".to_string(),
        DamageCause::Vandalism => "Vandalism".to_string(),
        DamageCause::War => "War".to_string(),
        DamageCause::Weather => "Weather".to_string(),
        DamageCause::Other { description } => description.clone(),
    }
}

/// A human-readable label for one usage category. `Other` surfaces its authored
/// free-text description verbatim.
fn usage_label(usage: &Usage) -> String {
    match usage {
        Usage::Unknown => "In use".to_string(),
        Usage::Agricultural => "Agricultural".to_string(),
        Usage::Commercial => "Commercial".to_string(),
        Usage::Cultural => "Cultural".to_string(),
        Usage::Educational => "Educational".to_string(),
        Usage::Healthcare => "Healthcare".to_string(),
        Usage::Industrial => "Industrial".to_string(),
        Usage::Infrastructure => "Infrastructure".to_string(),
        Usage::Institutional => "Institutional".to_string(),
        Usage::Military => "Military".to_string(),
        Usage::Recreational => "Recreational".to_string(),
        Usage::Religious => "Religious".to_string(),
        Usage::Residential => "Residential".to_string(),
        Usage::Transportation => "Transportation".to_string(),
        Usage::Other { description } => description.clone(),
    }
}

/// The new usage set a `UsageChanged` event settled on, joined for display. An
/// empty set is the type's documented "vacant or closed" state.
fn usage_set_label(usages: &BTreeSet<Usage>) -> String {
    if usages.is_empty() {
        return "Vacant or closed".to_string();
    }
    usages
        .iter()
        .map(usage_label)
        .collect::<Vec<_>>()
        .join(", ")
}

/// A settled move method as a short phrase for the timeline's secondary line.
fn move_method_label(method: &MoveMethod) -> &'static str {
    match method {
        MoveMethod::Disassembled => "Disassembled",
        MoveMethod::Whole => "Moved intact",
    }
}

/// The destination of a move when it names a displayable place — a named place
/// or a street address. A bare coordinate, a symbolic reference, or a region
/// combinator yields `None` rather than a fabricated label.
fn location_display(location: &UnresolvedLocation) -> Option<String> {
    match location {
        UnresolvedLocation::Reference(LocationReference::NamedPlace { name }) => Some(name.clone()),
        UnresolvedLocation::Reference(LocationReference::Address { address_text }) => {
            Some(address_text.clone())
        }
        _ => None,
    }
}

/// The secondary line for a `Moved` event: its settled method and, when the
/// destination names a place, where it went.
fn move_summary(
    method: &Bounded<Claimed<MoveMethod>, ImageId>,
    to: &Bounded<UnresolvedLocation, ImageId>,
) -> Option<String> {
    let method = method.settled().map(move_method_label);
    let destination = location_display(&to.possible);
    match (method, destination) {
        (Some(m), Some(d)) => Some(format!("{m}, to {d}")),
        (Some(m), None) => Some(m.to_string()),
        (None, Some(d)) => Some(format!("Moved to {d}")),
        (None, None) => None,
    }
}

// ==================== Date formatting ====================

/// Format a [`DateBound`] for display, truncating to the appropriate precision.
fn format_date_bound(bound: &DateBound) -> String {
    match bound.precision() {
        DatePrecision::Year
        | DatePrecision::Decade
        | DatePrecision::Century
        | DatePrecision::Millennium => {
            format!("{}", bound.period_start().format("%Y"))
        }
        DatePrecision::Month => format!("{}", bound.period_start().format("%Y-%m")),
        DatePrecision::Day => format!("{}", bound.period_start().format("%Y-%m-%d")),
    }
}

/// Format an `UncertainDate` for display, truncating to the appropriate precision.
fn format_uncertain_date(date: &UncertainDate) -> String {
    match (date.earliest_bound(), date.latest_bound()) {
        (Some(earliest), Some(latest)) if earliest == latest => {
            // Symmetric / exact: format by precision
            format_date_bound(earliest)
        }
        (Some(earliest), Some(latest)) => {
            // Asymmetric range
            format!(
                "{} \u{2013} {}",
                format_date_bound(earliest),
                format_date_bound(latest)
            )
        }
        (Some(earliest), None) => format!("after {}", format_date_bound(earliest)),
        (None, Some(latest)) => format!("before {}", format_date_bound(latest)),
        (None, None) => "date unknown".to_string(),
    }
}

// ==================== External links ====================

/// Build a display link for one external reference: the source-name label the
/// panel shows and the canonical URL. The URL is
/// [`ExternalReference::to_url`]'s job — this only owns the presentation label
/// (which stays in the web). An `UnmodeledUrl` with no extractable host renders
/// no link.
fn link_info(reference: &ExternalReference) -> Option<LinkInfo> {
    let label = source_label(reference)?;
    Some(LinkInfo {
        label,
        url: reference.to_url().to_string(),
    })
}

/// The human-readable source name for a link's anchor text — presentation that
/// stays in the web. `UnmodeledUrl` labels with its host, `None` when the URL
/// carries no extractable host (so the caller renders no link).
fn source_label(reference: &ExternalReference) -> Option<String> {
    Some(match reference {
        ExternalReference::Wikidata { .. } => "Wikidata".to_string(),
        ExternalReference::Wikipedia { language, .. } => {
            format!("Wikipedia ({})", language.as_str())
        }
        ExternalReference::OpenStreetMap { .. } => "OpenStreetMap".to_string(),
        ExternalReference::OpenHistoricalMap { .. } => "OpenHistoricalMap".to_string(),
        ExternalReference::GeoNames { .. } => "GeoNames".to_string(),
        ExternalReference::GettyTgn { .. } => "Getty TGN".to_string(),
        ExternalReference::Pleiades { .. } => "Pleiades".to_string(),
        ExternalReference::Nrhp { .. } => "NRHP".to_string(),
        ExternalReference::WikimediaCommonsCategory { .. } => "Wikimedia Commons".to_string(),
        ExternalReference::UnmodeledUrl { url } => extract_domain(url.as_str())?,
    })
}

/// Extract the domain from a URL string (e.g., `"https://example.com/path"` -> `"example.com"`).
fn extract_domain(url: &str) -> Option<String> {
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    Some(after_scheme.split('/').next()?.to_string())
}
