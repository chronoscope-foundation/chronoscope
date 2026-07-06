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
                    let feature = back.first().map(|e| e.id).unwrap_or(id);
                    set_selected.set(Some(EntitySelection::Single {
                        detail: id,
                        feature,
                        back: Some((*back).clone()),
                    }));
                };
                let display_name = entry.name.as_deref().unwrap_or("Unknown");
                let aria = display_name.to_string();
                view! {
                    <li>
                        <button
                            class="w-full text-left p-3 rounded-lg bg-parchment hover:bg-copper/10 \
                                   border border-copper/20 cursor-pointer transition-colors"
                            on:click=select
                            aria-label=aria
                        >
                            <span class="text-sm font-semibold text-ink">{display_name.to_string()}</span>
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
    id: MemoryEntityId,
    api_client: Rc<RefCell<Option<api::Client>>>,
) -> impl IntoView {
    let (retry_count, set_retry_count) = signal(0u32);
    let detail = LocalResource::new(move || {
        // Include retry_count in the dependency so incrementing it re-fetches.
        let _retry = retry_count.get();
        let api = api_client.clone();
        async move {
            let client = crate::api::get_or_init_client(&api)
                .await
                .ok_or_else(|| "Failed to load API configuration".to_string())?;
            fetch_entity_detail(&id, &client).await
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

                                // Images
                                {(!entity.media.is_empty()).then(|| {
                                    let count = entity.media.len();
                                    let lightbox = expect_context::<LightboxState>();

                                    view! {
                                        <div class="mb-3">
                                            <p class="text-xs font-sans text-copper font-semibold mb-1">
                                                {format!("Images ({count})")}
                                            </p>
                                            <ul class="grid grid-cols-2 gap-2" role="list">
                                                {entity.media.iter().map(|m| {
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
                                        </div>
                                    }
                                })}

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
/// Each row corresponds to one dated (or dateless) endpoint of a
/// [`TimelineEntry`]: a period-shaped phase (construction, demolition, an
/// interior span like modification or repair) contributes one row per
/// endpoint that actually carries a date, collapsing to a single dateless
/// row when neither endpoint does; a point-shaped interior event (usage
/// change, designation) contributes one row for its instant.
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
    media: Vec<MediaInfo>,
}

use std::collections::BTreeSet;

use chronoscope_core::Claimed;
use chronoscope_core::date::{DateBound, DatePrecision, UncertainDate};
use chronoscope_core::facts::citations::ExternalReference;
use chronoscope_core::facts::lifecycle::{DamageCause, MoveMethod, Usage};
use chronoscope_core::facts::memory::{MemoryEntityId, MemoryEventId, MemoryImageId};
use chronoscope_core::facts::typed::{
    Attributed, Bounded, Consensus, EventDetail, InteriorEvent, Period, TimelineEntry, best_name,
};
use chronoscope_core::ids::OsmElementType;
use chronoscope_core::location::{LocationReference, UnresolvedLocation};

/// Fetch entity detail using the typed API client and flatten it into the
/// view model the panel renders.
async fn fetch_entity_detail(
    id: &MemoryEntityId,
    client: &api::Client,
) -> Result<EntityDetailView, String> {
    let api::EntityDetail { entity, images } =
        client.get_entity(id).await.map_err(|e| e.to_string())?;

    let name = best_name(&entity.names, &browser_language_prefix()).map(|n| n.text.clone());
    let timeline = entity.timeline.iter().flat_map(timeline_rows).collect();
    let links = entity
        .external_refs
        .iter()
        .filter_map(|r| link_info(&r.value))
        .collect();
    let media = images
        .into_iter()
        .map(|img| MediaInfo {
            display_url: img.display_url,
            source_url: img.source_url,
            label: img.label,
        })
        .collect();

    Ok(EntityDetailView {
        name,
        timeline,
        links,
        media,
    })
}

/// The browser's preferred language prefix (e.g. "en" from "en-US"), falling
/// back to "en" when the navigator API is unavailable. Feeds the display-name
/// choice so the detail heading prefers the viewer's own language.
fn browser_language_prefix() -> String {
    web_sys::window()
        .map(|w| w.navigator().language().unwrap_or_default())
        .and_then(|lang| lang.split('-').next().map(String::from))
        .unwrap_or_else(|| "en".to_string())
}

// ==================== Timeline flattening ====================

/// The endpoint-specific and bare labels for one period-shaped lifecycle
/// phase (construction, demolition, or a durational interior event).
struct PhaseLabels {
    started: &'static str,
    completed: &'static str,
    /// Used when neither endpoint carries a date.
    bare: &'static str,
}

/// Flatten one timeline entry into its display row(s).
fn timeline_rows(entry: &TimelineEntry<MemoryEventId, MemoryImageId>) -> Vec<TimelineRow> {
    match &entry.detail {
        EventDetail::Constructed { period, .. } => period_rows(
            PhaseLabels {
                started: "Construction started",
                completed: "Construction completed",
                bare: "Constructed",
            },
            period,
            None,
        ),
        EventDetail::Demolished { period } => period_rows(
            PhaseLabels {
                started: "Demolition started",
                completed: "Demolition completed",
                bare: "Demolished",
            },
            period,
            None,
        ),
        EventDetail::Interior {
            descriptions, kind, ..
        } => interior_rows(kind, join_descriptions(descriptions)),
    }
}

/// Flatten one interior event's kind into its display row(s), threading
/// through the entry-level free-text descriptions.
fn interior_rows(
    kind: &InteriorEvent<MemoryImageId>,
    description: Option<String>,
) -> Vec<TimelineRow> {
    match kind {
        InteriorEvent::Modified { period } => period_rows(
            PhaseLabels {
                started: "Modification started",
                completed: "Modification completed",
                bare: "Modified",
            },
            period,
            description,
        ),
        InteriorEvent::Repaired { period } => period_rows(
            PhaseLabels {
                started: "Repair started",
                completed: "Repair completed",
                bare: "Repaired",
            },
            period,
            description,
        ),
        InteriorEvent::Damaged { period, cause } => period_rows(
            PhaseLabels {
                started: "Damage started",
                completed: "Damage completed",
                bare: "Damaged",
            },
            period,
            combine_description(cause.settled().map(damage_cause_label), description),
        ),
        InteriorEvent::Moved { period, method, to } => period_rows(
            PhaseLabels {
                started: "Move started",
                completed: "Move completed",
                bare: "Moved",
            },
            period,
            combine_description(move_summary(method, to), description),
        ),
        InteriorEvent::UsageChanged { at, usages } => vec![point_row(
            "Usage changed",
            at,
            combine_description(usages.settled().map(usage_set_label), description),
        )],
        InteriorEvent::Designated { at, designation } => vec![point_row(
            "Designated",
            at,
            combine_description(settled_designation(designation), description),
        )],
        InteriorEvent::Ambiguous { facts, .. } => vec![TimelineRow {
            label: "Event".to_string(),
            date: best_bound(&[&facts.started, &facts.completed, &facts.occurred]),
            description,
        }],
    }
}

/// Flatten a period into one row per dated endpoint, or a single bare row
/// with "date unknown" when neither endpoint carries a date. `description`
/// attaches to the terminal-most row present (completed over started).
fn period_rows(
    labels: PhaseLabels,
    period: &Period<MemoryImageId>,
    description: Option<String>,
) -> Vec<TimelineRow> {
    let started = has_date(&period.started).then(|| period.started.possible.clone());
    let completed = has_date(&period.completed).then(|| period.completed.possible.clone());

    match (started, completed) {
        (None, None) => vec![TimelineRow {
            label: labels.bare.to_string(),
            date: None,
            description,
        }],
        (Some(s), None) => vec![TimelineRow {
            label: labels.started.to_string(),
            date: Some(s),
            description,
        }],
        (None, Some(c)) => vec![TimelineRow {
            label: labels.completed.to_string(),
            date: Some(c),
            description,
        }],
        (Some(s), Some(c)) => vec![
            TimelineRow {
                label: labels.started.to_string(),
                date: Some(s),
                description: None,
            },
            TimelineRow {
                label: labels.completed.to_string(),
                date: Some(c),
                description,
            },
        ],
    }
}

/// One point-shaped interior event's row.
fn point_row(
    label: &str,
    at: &Bounded<UncertainDate, MemoryImageId>,
    description: Option<String>,
) -> TimelineRow {
    TimelineRow {
        label: label.to_string(),
        date: has_date(at).then(|| at.possible.clone()),
        description,
    }
}

/// Whether a claim actually touched this date slot — `Absent` means no
/// source ever asserted it, so the row renders "date unknown" rather than
/// the honest-but-meaningless bottom value.
fn has_date(bounded: &Bounded<UncertainDate, MemoryImageId>) -> bool {
    !matches!(bounded.consensus, Consensus::Absent)
}

/// The first dated bound among several candidates, in priority order. Used
/// for `Ambiguous` events, whose kind (and thus which date field is
/// authoritative) never settled.
fn best_bound(bounds: &[&Bounded<UncertainDate, MemoryImageId>]) -> Option<UncertainDate> {
    bounds
        .iter()
        .find_map(|b| has_date(b).then(|| b.possible.clone()))
}

/// Join an interior event's free-text descriptions into one secondary line.
fn join_descriptions(descriptions: &[Attributed<String, MemoryImageId>]) -> Option<String> {
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
fn settled_designation(designation: &Bounded<Claimed<String>, MemoryImageId>) -> Option<String> {
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
    method: &Bounded<Claimed<MoveMethod>, MemoryImageId>,
    to: &Bounded<UnresolvedLocation, MemoryImageId>,
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

/// Build a display link for one external reference. `Wikidata` is the
/// common case (every Wikidata-sourced entity carries one); the rest cover
/// the full [`ExternalReference`] vocabulary with the same host/path shapes
/// [`ExternalReference::from_url`] recognizes, so a reference round-trips
/// back to the URL it was likely parsed from.
fn link_info(reference: &ExternalReference) -> Option<LinkInfo> {
    match reference {
        ExternalReference::Wikidata { qid } => Some(LinkInfo {
            label: "Wikidata".to_string(),
            url: format!("https://www.wikidata.org/wiki/{qid}"),
        }),
        ExternalReference::Wikipedia { language, title } => Some(LinkInfo {
            label: format!("Wikipedia ({})", language.as_str()),
            url: format!(
                "https://{}.wikipedia.org/wiki/{}",
                language.as_str(),
                title.replace(' ', "_")
            ),
        }),
        ExternalReference::OpenStreetMap { element_type, id } => Some(LinkInfo {
            label: "OpenStreetMap".to_string(),
            url: format!(
                "https://www.openstreetmap.org/{}/{id}",
                osm_element_path(*element_type)
            ),
        }),
        ExternalReference::OpenHistoricalMap { element_type, id } => Some(LinkInfo {
            label: "OpenHistoricalMap".to_string(),
            url: format!(
                "https://www.openhistoricalmap.org/{}/{id}",
                osm_element_path(*element_type)
            ),
        }),
        ExternalReference::GeoNames { id } => Some(LinkInfo {
            label: "GeoNames".to_string(),
            url: format!("https://www.geonames.org/{id}"),
        }),
        ExternalReference::GettyTgn { id } => Some(LinkInfo {
            label: "Getty TGN".to_string(),
            url: format!("https://vocab.getty.edu/tgn/{id}"),
        }),
        ExternalReference::Pleiades { place_id } => Some(LinkInfo {
            label: "Pleiades".to_string(),
            url: format!("https://pleiades.stoa.org/places/{place_id}"),
        }),
        ExternalReference::Nrhp { reference_number } => Some(LinkInfo {
            label: "NRHP".to_string(),
            url: format!("https://npgallery.nps.gov/NRHP/AssetDetail?assetID={reference_number}"),
        }),
        ExternalReference::WikimediaCommonsCategory { category } => Some(LinkInfo {
            label: "Wikimedia Commons".to_string(),
            url: format!(
                "https://commons.wikimedia.org/wiki/Category:{}",
                category.as_str().replace(' ', "_")
            ),
        }),
        ExternalReference::UnmodeledUrl { url } => {
            extract_domain(url.as_str()).map(|domain| LinkInfo {
                label: domain,
                url: url.to_string(),
            })
        }
    }
}

fn osm_element_path(element_type: OsmElementType) -> &'static str {
    match element_type {
        OsmElementType::Node => "node",
        OsmElementType::Way => "way",
        OsmElementType::Relation => "relation",
    }
}

/// Extract the domain from a URL string (e.g., `"https://example.com/path"` -> `"example.com"`).
fn extract_domain(url: &str) -> Option<String> {
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    Some(after_scheme.split('/').next()?.to_string())
}
