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
    /// CDN URL for the full image.
    pub url: String,
    /// Alt text for accessibility.
    pub alt: String,
    /// Original upstream URL (for "Open original" link).
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
                        EntitySelection::Single(_, _) => "Entity Details",
                        EntitySelection::Multiple(_) => "Multiple Entities",
                    };
                    view! {
                        <div class="p-5">
                            <div class="flex justify-between items-start mb-4">
                                <h2 class="text-lg font-semibold font-sans text-ink">{title}</h2>
                                <DismissButton on_click=dismiss extra_class="ml-4"/>
                            </div>
                            {match selection {
                                EntitySelection::Single(id, back_entries) => view! {
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
                                        <EntityDetailContent id=id.clone() api_client=Rc::clone(&api_client)/>
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
                let id = entry.id.clone();
                let back = Rc::clone(&all_entries);
                let select = move |_| {
                    set_selected.set(Some(EntitySelection::Single(id.clone(), Some((*back).clone()))));
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
fn EntityDetailContent(id: String, api_client: Rc<RefCell<Option<api::Client>>>) -> impl IntoView {
    let id_clone = id.clone();
    let (retry_count, set_retry_count) = signal(0u32);
    let detail = LocalResource::new(move || {
        // Include retry_count in the dependency so incrementing it re-fetches.
        let _retry = retry_count.get();
        let id = id_clone.clone();
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
                                    {entity.name.unwrap_or_else(|| "Unnamed entity".to_string())}
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
                                                        <span class="font-semibold">{r.label}</span>
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
                                                    let alt = format!("{} view", m.kind_label);
                                                    let aria = format!("{} — opens preview", alt);
                                                    let content = LightboxContent {
                                                        url: m.url.clone(),
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
                                                                    src={m.url.clone()}
                                                                    alt=alt
                                                                    loading="lazy"
                                                                    class="w-full aspect-square object-cover relative"
                                                                />
                                                                // Date as primary badge (top-left pill)
                                                                {m.date_label.as_ref().map(|d| view! {
                                                                    <span class="absolute top-1 left-1 px-1.5 py-0.5 \
                                                                                 bg-ink/70 text-white text-xs font-sans \
                                                                                 rounded-full">
                                                                        {d.clone()}
                                                                    </span>
                                                                })}
                                                                // Kind badge (bottom-right pill)
                                                                <span class="absolute bottom-1 right-1 px-1.5 py-0.5 \
                                                                             bg-ink/70 text-white text-xs font-sans \
                                                                             rounded-full">
                                                                    {m.kind_label.clone()}
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
/// Each row corresponds to one [`chronoscope_core::Moment`] — a per-endpoint
/// projection of an `EntityTransition`. A `Constructed` with both
/// `started_at` and `completed_at` becomes two rows ("Construction started"
/// and "Construction completed"), each sortable independently by its own
/// date.
///
/// We hold an `UncertainDate` rather than a pre-formatted string so the
/// renderer can decide presentation (precision, range collapsing) at the
/// point of use, and so the sort key derives from the same value the
/// renderer displays.
#[derive(Debug, Clone)]
struct TimelineRow {
    label: &'static str,
    /// `None` when the date for this endpoint is unknown.
    date: Option<UncertainDate>,
    /// Optional secondary text shown beneath the row (e.g. the description
    /// on `UsageModified` or `Modified`).
    description: Option<String>,
}

#[derive(Debug, Clone)]
struct LinkInfo {
    label: String,
    url: String,
}

#[derive(Debug, Clone)]
struct MediaInfo {
    url: String,
    source_url: String,
    kind_label: String,
    date_label: Option<String>,
}

#[derive(Debug, Clone)]
struct EntityDetailView {
    name: Option<String>,
    timeline: Vec<TimelineRow>,
    links: Vec<LinkInfo>,
    media: Vec<MediaInfo>,
}

/// Return the browser's preferred language prefix (e.g. "en" from "en-US"),
/// falling back to "en" if the navigator API is unavailable.
fn browser_language_prefix() -> String {
    web_sys::window()
        .map(|w| w.navigator().language().unwrap_or_default())
        .and_then(|lang| lang.split('-').next().map(String::from))
        .unwrap_or_else(|| "en".to_string())
}

use chronoscope_api_client::EntityId;
use chronoscope_core::AnnotationKind;
use chronoscope_core::date::{DateBound, DatePrecision, UncertainDate};
use chronoscope_core::entity::EntityTransition;
use chronoscope_core::links::{LinkTarget, LinkType};
use chronoscope_core::moment::{Moment, TransitionRole, decompose, topological_order};

/// Fetch entity detail using the typed API client.
async fn fetch_entity_detail(id: &str, client: &api::Client) -> Result<EntityDetailView, String> {
    let entity_id = EntityId::new(id);
    let resp = client
        .get_entity(&entity_id)
        .await
        .map_err(|e| e.to_string())?;

    let name = resp
        .entity
        .best_name(&browser_language_prefix())
        .map(String::from);

    // Decompose into per-endpoint moments and sort using core's topological
    // order, then map each Moment to a display row. The sort respects both
    // structural edges (construction-end before demolition-start) and date
    // edges, so the Mole Antonelliana case (Construction completed 1889 +
    // UsageModified 1888) renders the usage change between the unknown
    // construction start and the dated construction completion.
    let moments = topological_order(decompose(&resp.entity.transitions));
    let timeline: Vec<TimelineRow> = moments.iter().map(moment_to_row).collect();

    let links = resp
        .links
        .iter()
        .filter_map(|link| {
            let (url, label) = format_link(&link.link_type, &link.target)?;
            Some(LinkInfo { label, url })
        })
        .collect();

    let media = resp
        .media
        .iter()
        .map(|m| MediaInfo {
            url: m.url.clone(),
            source_url: m.source_url.clone(),
            kind_label: format_annotation_kind(&m.annotation_kind),
            date_label: m.captured.as_ref().map(format_uncertain_date),
        })
        .collect();

    Ok(EntityDetailView {
        name,
        timeline,
        links,
        media,
    })
}

/// Map a [`Moment`] to a display row.
fn moment_to_row(m: &Moment<'_, EntityId, chronoscope_api_client::SourceId>) -> TimelineRow {
    TimelineRow {
        label: role_label(m.role, m.collapsed),
        date: m.date.map(|c| c.value.clone()),
        description: moment_description(m),
    }
}

/// Human-readable label for a transition role. Uses the bare form
/// ("Constructed") when the moment was collapsed from a both-undated
/// durational, otherwise the endpoint-specific form ("Construction started").
fn role_label(role: TransitionRole, collapsed: bool) -> &'static str {
    if collapsed {
        match role {
            TransitionRole::ConstructionStart => "Constructed",
            TransitionRole::ModificationStart => "Modified",
            TransitionRole::RepairStart => "Repaired",
            TransitionRole::DemolitionStart => "Demolished",
            _ => role_label(role, false),
        }
    } else {
        match role {
            TransitionRole::ConstructionStart => "Construction started",
            TransitionRole::ConstructionEnd => "Construction completed",
            TransitionRole::ModificationStart => "Modification started",
            TransitionRole::ModificationEnd => "Modification completed",
            TransitionRole::RepairStart => "Repair started",
            TransitionRole::RepairEnd => "Repair completed",
            TransitionRole::Damaged => "Damaged",
            TransitionRole::Moved => "Moved",
            TransitionRole::UsageModified => "Usage modified",
            TransitionRole::Designated => "Designated",
            TransitionRole::DemolitionStart => "Demolition started",
            TransitionRole::DemolitionEnd => "Demolition completed",
        }
    }
}

/// Secondary display text for a moment, extracted from the parent transition.
fn moment_description<E, S>(m: &Moment<'_, E, S>) -> Option<String> {
    match m.transition {
        EntityTransition::Modified { description, .. }
        | EntityTransition::Repaired { description, .. }
        | EntityTransition::Damaged { description, .. }
        | EntityTransition::UsageModified { description, .. } => description.clone(),
        EntityTransition::Demolished { cause, .. } | EntityTransition::Moved { cause, .. } => {
            cause.clone()
        }
        EntityTransition::Designated {
            designation,
            description,
            ..
        } => Some(match description {
            Some(extra) => format!("{designation} \u{2014} {extra}"),
            None => designation.clone(),
        }),
        EntityTransition::Constructed { .. } => None,
    }
}

/// Format a [`DateBound`] for display, truncating to the appropriate precision.
fn format_date_bound(bound: &DateBound) -> String {
    match bound.precision() {
        DatePrecision::Year
        | DatePrecision::Decade
        | DatePrecision::Century
        | DatePrecision::Millennium => {
            format!("{}", bound.date().format("%Y"))
        }
        DatePrecision::Month => format!("{}", bound.date().format("%Y-%m")),
        DatePrecision::Day => format!("{}", bound.date().format("%Y-%m-%d")),
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

/// Format an annotation kind into a short display label.
fn format_annotation_kind(kind: &AnnotationKind) -> String {
    match kind {
        AnnotationKind::SpatialTrace { .. } => "Spatial".to_string(),
        AnnotationKind::ExteriorView { .. } => "Exterior".to_string(),
        AnnotationKind::InteriorView { .. } => "Interior".to_string(),
        AnnotationKind::TextualNote { .. } => "Note".to_string(),
    }
}

/// Format a link target into a URL and human-readable label.
fn format_link(link_type: &LinkType, target: &LinkTarget) -> Option<(String, String)> {
    let url = target.to_url().to_string();

    // Filter out non-HTTP URLs (e.g., javascript:)
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return None;
    }

    let target_label = match target {
        LinkTarget::Wikidata { .. } => "Wikidata".to_string(),
        LinkTarget::Wikipedia { language, .. } => format!("Wikipedia ({})", language.as_str()),
        LinkTarget::Pleiades { .. } => "Pleiades".to_string(),
        LinkTarget::OpenStreetMap { .. } => "OpenStreetMap".to_string(),
        LinkTarget::WikimediaCommons { .. } => "Wikimedia Commons".to_string(),
        LinkTarget::Nrhp { .. } => "NRHP".to_string(),
        LinkTarget::GeoNames { .. } => "GeoNames".to_string(),
        LinkTarget::GettyTgn { .. } => "Getty TGN".to_string(),
        LinkTarget::Sanborn { .. } => "Sanborn Maps".to_string(),
        LinkTarget::Url { url } => {
            extract_domain(url.as_str()).unwrap_or_else(|| "Link".to_string())
        }
    };

    // For same_as links, just show the target name.
    // For other relationship types, prefix with the relationship.
    let label = match link_type {
        LinkType::SameAs => target_label,
        _ => {
            let rel = link_type.as_ref().replace('_', " ");
            format!("{rel}: {target_label}")
        }
    };

    Some((url, label))
}

/// Extract the domain from a URL string (e.g., `"https://example.com/path"` -> `"example.com"`).
fn extract_domain(url: &str) -> Option<String> {
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    Some(after_scheme.split('/').next()?.to_string())
}
