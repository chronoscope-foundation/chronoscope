use std::cell::RefCell;
use std::rc::Rc;

use leptos::portal::Portal;
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

    // The seam the images grid pins its page-1 fetch to: `Some` once the detail
    // resolves successfully, `None` while it's pending or errored. `EntityImages`
    // gates its first fetch on this so the grid and the detail read one
    // consistent snapshot, without the child reaching into the resource itself.
    let ready_snapshot = Memo::new(move |_| match detail.get() {
        Some(Ok(view)) => Some(view.snapshot.clone()),
        _ => None,
    });

    // Wrap the `!Send` client so the reactive view closure that hands it to
    // `EntityImages` stays `Send`.
    let api_client = SendWrapper::new(api_client);

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
                                    {match &entity.name {
                                        Some(name) => {
                                            let text = name.text.clone();
                                            let bullet = name.citations.clone().map(|lines| {
                                                view! { <CitationMarker lines=lines/> }
                                            });
                                            view! { <span>{text}</span>{bullet} }.into_any()
                                        }
                                        None => view! { <span>"Unnamed entity"</span> }.into_any(),
                                    }}
                                </h3>
                                <EntityTimeline
                                    timeline=entity.timeline.clone()
                                    conflicts=entity.conflicts.clone()
                                />
                                <EntityImages
                                    id=id.clone()
                                    api_client=api_client.clone()
                                    ready_snapshot=ready_snapshot
                                />
                                <EntityLinks links=entity.links.clone()/>
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

/// The entity's lifecycle timeline: a header with the moment count over a list
/// of dated rows. Renders nothing when the entity has no timeline. Entity-level
/// temporal conflicts ride inline on the participating rows (an amber marker
/// beside the citation bullet), not as a detached card.
#[component]
fn EntityTimeline(timeline: Vec<TimelineRow>, conflicts: Vec<ConflictInfo>) -> impl IntoView {
    (!timeline.is_empty()).then(move || {
        let count = timeline.len();
        let points = conflict_points(&timeline);
        let rows = timeline
            .iter()
            .map(|row| timeline_row_view(row, row_conflicts(row, &conflicts, &points)))
            .collect::<Vec<_>>();
        view! {
            <div class="mb-3">
                <p class="text-xs font-sans text-copper font-semibold mb-1">
                    {format!("Timeline ({count})")}
                </p>
                <ul class="text-sm text-sepia space-y-1.5">
                    {rows}
                </ul>
            </div>
        }
    })
}

/// The entity's depicting images: a paginated grid that loads its first page as
/// soon as the detail's snapshot is known and appends further pages on demand.
///
/// Page 1 waits for `ready_snapshot` to be `Some` and pins to it, so the grid
/// reads the same point the detail was projected at; a failed or missing entity
/// resolves no snapshot and draws no image request. Each "Load more" resumes the
/// stored cursor, which already pins that snapshot.
#[component]
fn EntityImages(
    id: EntityId,
    api_client: SendWrapper<Rc<RefCell<Option<api::Client>>>>,
    ready_snapshot: Memo<Option<api::Snapshot>>,
) -> impl IntoView {
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
    // surfaces here; a retryable error offers a retry, a terminal one (a 4xx
    // client error) reports without one so the reader isn't stranded
    // re-triggering a fetch that can only fail again.
    let (images_error, set_images_error) = signal(Option::<ApiError>::None);

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
                // Config discovery failed before any request went out — a
                // pre-request client failure, retryable so the grid offers a retry
                // that re-attempts the `/config.json` fetch.
                None => Err(ApiError::Client(
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

    // `ready_snapshot.get()` is tracked so page 1 fires when the detail resolves;
    // it's write-once-terminal, so tracking it on every run adds no extra fires.
    // `next_cursor` stays untracked — a "Load more" bumps `page_req`, which is
    // what should re-drive the effect, not a cursor write.
    Effect::new(move |_| {
        let req = page_req.get();
        if let Some((cursor, snapshot)) =
            images_fetch_params(req, next_cursor.get_untracked(), ready_snapshot.get())
        {
            load_page(cursor, snapshot);
        }
    });

    // The section shows whenever there are tiles, another page to fetch, or a
    // recoverable error to retry, so a first page that fully filters out still
    // surfaces "Load more" instead of stranding an empty grid.
    view! {
        {move || {
            let tiles = media.get();
            let has_more = next_cursor.get().is_some();
            // `ApiError` isn't `Clone` (it wraps a `reqwest::Error`), so read the
            // signal by reference rather than cloning it out: the view only needs
            // whether an error is present and which button it implies.
            let (has_error, button) = images_error
                .with(|error| (error.is_some(), images_button(has_more, error.as_ref())));
            let show = !tiles.is_empty() || has_more || has_error;
            show.then(move || {
                let count = tiles.len();

                view! {
                <div class="mb-3">
                    {(!tiles.is_empty()).then(move || {
                        view! {
                            <p class="text-xs font-sans text-copper font-semibold mb-1">
                                {format!("Images ({count} loaded)")}
                            </p>
                            <ul class="grid grid-cols-2 gap-2" role="list">
                                {tiles.into_iter().map(|m| view! {
                                    <ImageTile media=m/>
                                }).collect::<Vec<_>>()}
                            </ul>
                        }
                    })}
                    {has_error.then(|| view! {
                        <p class="text-sm text-red-600 mt-2">"Couldn\u{2019}t load images."</p>
                    })}
                    {button.map(move |b| {
                        let label = match b {
                            ImagesButton::Retry => "Retry",
                            ImagesButton::LoadMore => "Load more",
                        };
                        view! {
                        <button
                            class="mt-2 w-full text-sm text-ink hover:underline \
                                   cursor-pointer disabled:opacity-50 disabled:cursor-default"
                            on:click=move |_: leptos::ev::MouseEvent| {
                                set_page_req.update(|n| *n += 1);
                            }
                            disabled=move || loading.get()
                            aria-label="Load more images"
                        >
                            {label}
                        </button>
                        }
                    })}
                </div>
                }
            })
        }}
    }
}

/// One image grid tile: a lazy thumbnail with a caption badge that opens the
/// lightbox on click. Reads the lightbox handle from context.
#[component]
fn ImageTile(media: MediaInfo) -> impl IntoView {
    let lightbox = expect_context::<LightboxState>();
    let alt = format!("{} view", media.label);
    let aria = format!("{alt} \u{2014} opens preview");
    let content = LightboxContent {
        url: media.display_url.clone(),
        alt: alt.clone(),
        source_url: media.source_url.clone(),
    };
    let on_click = move |_: leptos::ev::MouseEvent| {
        lightbox.open(content.clone());
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
                    src={media.display_url.clone()}
                    alt=alt
                    loading="lazy"
                    class="w-full aspect-square object-cover relative"
                />
                // Caption badge (bottom-right pill)
                <span class="absolute bottom-1 right-1 px-1.5 py-0.5 \
                             bg-ink/70 text-white text-xs font-sans \
                             rounded-full">
                    {media.label.clone()}
                </span>
            </button>
        </li>
    }
}

/// The entity's external links: a header with the count over a list of anchors.
/// Renders nothing when the entity has no links.
#[component]
fn EntityLinks(links: Vec<LinkInfo>) -> impl IntoView {
    (!links.is_empty()).then(move || {
        let count = links.len();
        view! {
            <div class="mb-3">
                <p class="text-xs font-sans text-copper font-semibold mb-1">
                    {format!("Links ({count})")}
                </p>
                <ul class="text-sm space-y-1">
                    {links.iter().map(|link| {
                        let bullet = link.citations.clone().map(|lines| {
                            view! { <CitationMarker lines=lines/> }
                        });
                        view! {
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
                            {bullet}
                        </li>
                        }
                    }).collect::<Vec<_>>()}
                </ul>
            </div>
        }
    })
}

// ==================== Lightweight detail types ====================

/// One line in a citation popover: a value and the short label of the source
/// that attests it. `source` is `None` when the claim carries no citation;
/// `url` links the line to the source when it has a stable address.
#[derive(Debug, Clone)]
struct CiteEntry {
    value: String,
    source: Option<String>,
    /// The source's clickable URL — a Wikidata item or a crawled page. `None`
    /// for sources with no stable link (books, archives), which render as plain
    /// text.
    url: Option<String>,
}

/// The lines a citation bullet lists — the variant is the bullet's meaning: the
/// sources agreeing on a field's value, or the rival claims contesting it. The
/// count is the inner list's length; `Rivals` reads as the disputed badge.
#[derive(Debug, Clone)]
enum CitationLines {
    Sources(Vec<CiteEntry>),
    Rivals(Vec<CiteEntry>),
}

/// The entity's display name and the badge for the sources behind it.
#[derive(Debug, Clone)]
struct NameInfo {
    text: String,
    citations: Option<CitationLines>,
}

/// One row in the rendered entity timeline.
///
/// Each row is one ordered [`MomentView`]: a durational endpoint (construction,
/// demolition, or an interior span like modification or repair) or a
/// point-shaped interior event (usage change, designation). A durational whose
/// endpoints are both dateless collapses to a single bare row.
#[derive(Debug, Clone)]
struct TimelineRow {
    /// The moment's role — drives the label, the subtle existence styling, and
    /// which side of a conflict (witness vs bookend) this row plays.
    role: TransitionRole,
    label: String,
    date: DateCell,
    /// The fact ids behind this row's date, carried from the moment's
    /// [`Bounded::facts`]. A row participates in a conflict when one of these
    /// rides the conflict's own `facts`.
    facts: Vec<FactId>,
    /// Optional secondary text shown beneath the row (free-text
    /// descriptions authored on the underlying event, plus — for
    /// `Designated` — the settled designation text).
    description: Option<String>,
}

/// The date side of a timeline row: the value rendered inline, folded together
/// with the provenance behind it. Each variant carries its own citation lines —
/// agreeing sources, contested rivals, or a derivation's witnesses — so the
/// bullet the row shows follows from the variant.
#[derive(Debug, Clone)]
enum DateCell {
    /// No claim dated this row.
    Unknown,
    /// One value the sources agree on, each an agreeing line.
    Settled {
        value: UncertainDate,
        sources: Vec<CiteEntry>,
    },
    /// A value this layer left open, with the sources behind it.
    Pending {
        value: UncertainDate,
        sources: Vec<CiteEntry>,
    },
    /// Irreconcilable rival claims, rendered as their joined extent — the honest
    /// span of possible instants: disjoint like "1160 / 1163" when the rivals
    /// leave a gap, contiguous when they abut. Each rival's own claim rides in
    /// `rivals`.
    Disputed {
        value: UncertainDate,
        rivals: Vec<CiteEntry>,
    },
    /// A bound no source asserted — one or more solver rules reached past what
    /// the sources stated. The value reads inline in the muted sage tone; the
    /// rules that reached it and the facts they consumed ride alongside for the
    /// marker's popover, which phrases the claim from the rules themselves.
    Inferred {
        value: UncertainDate,
        rules: NonEmptyVec<DerivationRule>,
        witnesses: Vec<CiteEntry>,
    },
}

impl DateCell {
    /// The representative instant this row sits at: its earliest possible date, or
    /// `None` when it has none — an undated row, or a lower-open bound, which spans
    /// a range rather than sitting at one instant.
    fn instant(&self) -> Option<NaiveDate> {
        let date = match self {
            DateCell::Unknown => return None,
            DateCell::Settled { value, .. }
            | DateCell::Pending { value, .. }
            | DateCell::Disputed { value, .. }
            | DateCell::Inferred { value, .. } => value,
        };
        date.earliest()
    }
}

impl TimelineRow {
    /// This row as a point on a conflict's time axis, when it carries a date. A
    /// mid-life row — an existence witness or interior event — plays the witness,
    /// the out-of-lifetime side a conflict highlights; a bookend plays the anchor.
    fn point(&self) -> Option<ConflictPoint> {
        let instant = self.date.instant()?;
        let role = if self.role.is_midlife() {
            ConflictRole::Witness
        } else {
            ConflictRole::Anchor
        };
        Some(ConflictPoint {
            label: self.label.clone(),
            instant,
            role,
        })
    }
}

/// One entity-level temporal conflict surfaced inline: the structured clash and
/// the fact ids it names. The plain-language line is built at render time from
/// `kind` ([`conflict_summary`]), so localizing it never touches the fetch path.
/// A timeline row participates when a fact id here is among its own.
#[derive(Debug, Clone)]
struct ConflictInfo {
    kind: TemporalConflictKind,
    facts: Vec<FactId>,
}

/// One participant plotted on a conflict's time axis: the row's label, the
/// instant it sits at, and which side it plays — the witness (out-of-lifetime
/// evidence the conflict highlights) or the lifetime bookend it clashes with. The
/// year label and scaling position derive from `instant` at render.
#[derive(Debug, Clone)]
struct ConflictPoint {
    label: String,
    instant: NaiveDate,
    role: ConflictRole,
}

/// Which side of a conflict a plotted point plays: the witness (out-of-lifetime
/// evidence) or the lifetime bookend it sits on the wrong side of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConflictRole {
    Witness,
    Anchor,
}

/// A conflict resolved against the timeline for one marker: its summary and the
/// distinct participant points, left-to-right.
#[derive(Debug, Clone)]
struct ResolvedConflict {
    summary: String,
    points: Vec<ConflictPoint>,
}

#[derive(Debug, Clone)]
struct LinkInfo {
    label: String,
    url: String,
    citations: Option<CitationLines>,
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
    name: Option<NameInfo>,
    /// Entity-level temporal conflicts — facts that can't jointly hold. Surfaced
    /// inline on the participating timeline rows, not as a detached card.
    conflicts: Vec<ConflictInfo>,
    timeline: Vec<TimelineRow>,
    links: Vec<LinkInfo>,
    /// The read-consistency point the detail was served at. Passed to the
    /// images fetch so the grid reads the same state as the detail.
    snapshot: api::Snapshot,
}

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;

use chrono::{Datelike, NaiveDate};

use chronoscope_core::Claimed;
use chronoscope_core::date::{DateBound, DatePrecision, TimeRange, UncertainDate};
use chronoscope_core::grammar::citations::{
    ExternalReference, ExternalSource, JudgmentSource, WikidataField,
};
use chronoscope_core::grammar::ids::FactId;
use chronoscope_core::grammar::lifecycle::{DamageCause, MoveMethod, Usage};
use chronoscope_core::location::{LocationReference, UnresolvedLocation};
use chronoscope_core::moment::TransitionRole;
use chronoscope_core::nonempty::NonEmptyVec;
use chronoscope_core::projection::{Citation, DerivationRule};
use chronoscope_core::solvers::TemporalConflictKind;
use chronoscope_core::typed::{
    Attributed, Bounded, Consensus, Derivation, EventDetail, InteriorEvent, MomentView,
    distinct_rivals,
};

use chronoscope_api_client::{
    EntityId, EventId, ImageId, ImagesButton, images_button, images_fetch_params,
};

use crate::api::ApiError;

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
        temporal_conflicts,
        snapshot,
    } = client.get_entity(id).await.map_err(|e| e.to_string())?;

    let name = display_name.map(|text| {
        // Several name records can share the display text (the same proper name
        // spelled identically across languages or key types); the badge unions
        // the sources behind all of them in first-appearance order, keyed on the
        // attestation the reader sees — its source label and link — so a dozen
        // identical Wikidata labels collapse to their one attestation.
        let mut sources: Vec<Citation<ImageId>> = Vec::new();
        let mut seen: Vec<(Option<String>, Option<String>)> = Vec::new();
        for record in entity.names.iter().filter(|n| n.text == text) {
            for source in &record.sources {
                let key = (citation_label(source), citation_url(source));
                if !seen.contains(&key) {
                    seen.push(key);
                    sources.push(source.clone());
                }
            }
        }
        let citations = cited_field(&text, &sources);
        NameInfo { text, citations }
    });
    let timeline = entity.timeline.moments().map(moment_row).collect();
    let links = entity.external_refs.iter().filter_map(link_info).collect();
    // The entity-level temporal conflicts the server's read-time solver found —
    // each the structured clash plus the fact ids it blames, so the panel can
    // mark the participating rows and phrase the line at render time. Empty when
    // the facts are jointly consistent.
    let conflicts = temporal_conflicts
        .into_iter()
        .map(|conflict| ConflictInfo {
            kind: conflict.kind,
            facts: conflict.facts.iter().copied().collect(),
        })
        .collect();

    Ok(EntityDetailView {
        name,
        conflicts,
        timeline,
        links,
        snapshot,
    })
}

/// Fetch one page of an entity's depicting images, flattening each tile into a
/// [`MediaInfo`] and returning the resume cursor for the next page (`None` once
/// the grid is exhausted).
async fn fetch_entity_images_page(
    id: &EntityId,
    cursor: Option<&api::Cursor>,
    snapshot: Option<&api::Snapshot>,
    client: &api::Client,
) -> Result<(Vec<MediaInfo>, Option<api::Cursor>), ApiError> {
    let page = client
        .get_entity_images(id, IMAGES_PAGE_SIZE, cursor, snapshot)
        .await?;
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
    let date = date_display(moment.date);
    let facts = moment.date.map(|b| b.facts.clone()).unwrap_or_default();
    TimelineRow {
        role: moment.role,
        label: moment_label(moment.role, moment.collapsed).to_string(),
        date,
        facts,
        description: moment
            .carries_description
            .then(|| entry_description(&moment.event.detail))
            .flatten(),
    }
}

/// Read a moment's date slot into its inline cell, folding each slot's provenance
/// into the variant. A settled or pending slot cites each source against its one
/// value; a conflict carries each rival's own date and source.
fn date_display(bounded: Option<&Bounded<UncertainDate, ImageId>>) -> DateCell {
    let Some(b) = bounded else {
        return DateCell::Unknown;
    };
    // An inferred bound outranks the consensus read: its value reaches past what
    // any source stated, and the inferred marker owns the witnesses behind it, so
    // the row shows no separate citation bullet.
    if let Derivation::Inferred { rules } = &b.derivation {
        // The witness lines cite the attested year `W` the source states; the
        // bound itself rides on the cell's value, formatted at render.
        let witnessed = b
            .possible
            .latest_bound()
            .map(format_date_bound)
            .unwrap_or_else(|| format_uncertain_date(&b.possible));
        return DateCell::Inferred {
            value: b.possible.clone(),
            rules: rules.clone(),
            witnesses: cite_entries(&witnessed, &b.sources),
        };
    }
    match &b.consensus {
        Consensus::Absent => DateCell::Unknown,
        Consensus::Reached { .. } => DateCell::Settled {
            value: b.possible.clone(),
            sources: cite_entries(&format_uncertain_date(&b.possible), &b.sources),
        },
        Consensus::Pending { .. } => DateCell::Pending {
            value: b.possible.clone(),
            sources: cite_entries(&format_uncertain_date(&b.possible), &b.sources),
        },
        Consensus::Conflict { fighting } => {
            let rivals = distinct_rivals(fighting)
                .into_iter()
                .map(|rival| CiteEntry {
                    value: format_uncertain_date(&rival.value),
                    source: rival_source_label(&rival.sources),
                    url: rival.sources.first().and_then(citation_url),
                })
                .collect::<Vec<_>>();
            DateCell::Disputed {
                value: b.possible.clone(),
                rivals,
            }
        }
    }
}

/// The badge for a single-valued cited field — a name or a link. Each source
/// becomes one agreeing popover line attesting the field's own display `value`.
/// `None` when nothing cites the field.
fn cited_field(value: &str, sources: &[Citation<ImageId>]) -> Option<CitationLines> {
    let entries = cite_entries(value, sources);
    (!entries.is_empty()).then_some(CitationLines::Sources(entries))
}

/// One popover line per source, each attesting the given display `value`. The
/// shared body of [`cited_field`] and the inferred marker's witness list.
fn cite_entries(value: &str, sources: &[Citation<ImageId>]) -> Vec<CiteEntry> {
    sources
        .iter()
        .map(|source| CiteEntry {
            value: value.to_string(),
            source: citation_label(source),
            url: citation_url(source),
        })
        .collect()
}

/// A short source label for one rival's citations: each citation's source name,
/// de-duplicated and joined. `None` when the rival carries no citation.
fn rival_source_label(sources: &[Citation<ImageId>]) -> Option<String> {
    let mut labels: Vec<String> = Vec::new();
    for source in sources {
        if let Some(label) = citation_label(source)
            && !labels.contains(&label)
        {
            labels.push(label);
        }
    }
    (!labels.is_empty()).then(|| labels.join(", "))
}

/// The source name for one citation — a factual claim's external source, or a
/// judgment's warrant flavor.
fn citation_label(citation: &Citation<ImageId>) -> Option<String> {
    match citation {
        Citation::Factual { citation } => Some(external_source_label(&citation.source)),
        Citation::Judgment { source } => Some(judgment_source_label(source)),
    }
}

/// The clickable URL for a citation's source. A Wikidata source links to its
/// pinned revision, deep-linked to the cited property's statement group when the
/// claim came from one, so the reader lands on the exact state that was ingested.
/// A crawled page links to its URL. Books and archives have no stable link, so
/// their lines stay plain text.
fn citation_url(citation: &Citation<ImageId>) -> Option<String> {
    let source = match citation {
        Citation::Factual { citation } => &citation.source,
        Citation::Judgment {
            source: JudgmentSource::External { source },
        } => source,
        Citation::Judgment { .. } => return None,
    };
    match source {
        ExternalSource::Wikidata {
            entity_id,
            field,
            revision_id,
            ..
        } => {
            let page = format!("https://www.wikidata.org/wiki/{entity_id}?oldid={revision_id}");
            Some(match field {
                WikidataField::Statement { property_id } => format!("{page}#{property_id}"),
                WikidataField::Label { .. }
                | WikidataField::Sitelink { .. }
                | WikidataField::Item => page,
            })
        }
        ExternalSource::Url { url, .. } => Some(url.to_string()),
        ExternalSource::Dbpedia { .. }
        | ExternalSource::Book { .. }
        | ExternalSource::Archive { .. } => None,
    }
}

/// A short name for an external source. A Wikidata statement carries the
/// property id it was read from (e.g. "Wikidata \u{00b7} P571"); a URL shows its
/// host.
fn external_source_label(source: &ExternalSource) -> String {
    match source {
        ExternalSource::Wikidata { field, .. } => match field {
            WikidataField::Statement { property_id } => format!("Wikidata \u{00b7} {property_id}"),
            WikidataField::Label { .. } | WikidataField::Sitelink { .. } | WikidataField::Item => {
                "Wikidata".to_string()
            }
        },
        ExternalSource::Url { url, .. } => url
            .host_str()
            .map(|host| host.to_string())
            .unwrap_or_else(|| "Source".to_string()),
        ExternalSource::Dbpedia { .. } => "DBpedia".to_string(),
        ExternalSource::Book { title, .. } => title.as_str().to_owned(),
        ExternalSource::Archive { collection, .. } => collection.as_str().to_owned(),
    }
}

/// A short name for a judgment's warrant.
fn judgment_source_label(source: &JudgmentSource<ImageId>) -> String {
    match source {
        JudgmentSource::External { source } => external_source_label(source),
        JudgmentSource::PersonalKnowledge { .. } => "Researcher".to_string(),
        JudgmentSource::Analysis { .. } => "Analysis".to_string(),
        JudgmentSource::Derivation { .. } => "Derivation".to_string(),
        JudgmentSource::ImageObservation { .. } => "Image observation".to_string(),
    }
}

/// The label for a moment's role. The durational start roles read as the bare
/// verb when collapsed (both endpoints dateless); otherwise start and end roles
/// read as "… started" / "… completed", and point roles carry their own phrase.
fn moment_label(role: TransitionRole, collapsed: bool) -> &'static str {
    use TransitionRole::{
        Ambiguous, ConstructionEnd, ConstructionStart, DamagedEnd, DamagedStart, DemolitionEnd,
        DemolitionStart, Designated, KnownToExist, ModificationEnd, ModificationStart, MovedEnd,
        MovedStart, RepairEnd, RepairStart, UsageModified,
    };
    match (role, collapsed) {
        (KnownToExist, _) => "Known to exist",
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
        EventDetail::Constructed { .. }
        | EventDetail::Demolished { .. }
        | EventDetail::Existed { .. } => return None,
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
        DamageCause::Other { description } => description.as_str().to_owned(),
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
        Usage::Other { description } => description.as_str().to_owned(),
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
        UnresolvedLocation::Reference(LocationReference::NamedPlace { name }) => {
            Some(name.as_str().to_owned())
        }
        UnresolvedLocation::Reference(LocationReference::Address { address_text }) => {
            Some(address_text.as_str().to_owned())
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

// ==================== Row rendering ====================

/// Render one timeline row: the role label, its inline date, the citation
/// bullet, and any secondary description. A contested date reads its joined
/// value inline in the darker body tone; the rivals live in the bullet's
/// popover.
fn timeline_row_view(row: &TimelineRow, conflicts: Vec<ResolvedConflict>) -> AnyView {
    let label = row.label.clone();
    let description = row.description.clone();
    // Existence witnesses are evidence, not a lifecycle phase — muted, not bold,
    // with a fainter rule. An inferred bound is derived rather than asserted, so
    // it wears its own muted sage treatment.
    let existence = row.role == TransitionRole::KnownToExist;
    let inferred = matches!(&row.date, DateCell::Inferred { .. });
    // The bullet's meaning is the variant: agreeing sources (when any) read
    // neutral, contested rivals read disputed; an inferred bound wears the marker
    // instead, an unknown date carries nothing.
    let bullet = match &row.date {
        DateCell::Settled { sources, .. } | DateCell::Pending { sources, .. } => {
            (!sources.is_empty()).then(|| {
                let lines = CitationLines::Sources(sources.clone());
                view! { <CitationMarker lines=lines/> }
            })
        }
        DateCell::Disputed { rivals, .. } => {
            let lines = CitationLines::Rivals(rivals.clone());
            Some(view! { <CitationMarker lines=lines/> })
        }
        DateCell::Inferred { .. } | DateCell::Unknown => None,
    };
    let inferred_marker = match &row.date {
        DateCell::Inferred {
            value,
            rules,
            witnesses,
        } => Some(view! {
            <InferredMarker value=value.clone() rules=rules.clone() witnesses=witnesses.clone()/>
        }),
        _ => None,
    };
    let marker = (!conflicts.is_empty()).then(|| view! { <ConflictMarker conflicts=conflicts/> });
    let date_view = match &row.date {
        DateCell::Unknown => {
            view! { <span class="text-sepia/40 italic">" \u{2014} date unknown"</span> }.into_any()
        }
        DateCell::Settled { value: date, .. } => view! {
            <span class="text-sepia/70">{format!(" \u{2014} {}", format_uncertain_date(date))}</span>
        }
        .into_any(),
        DateCell::Pending { value: date, .. } => view! {
            <span class="text-sepia/70">
                {format!(" \u{2014} {}", format_uncertain_date(date))}
                <span class="text-sepia/40 italic text-xs">" (pending)"</span>
            </span>
        }
        .into_any(),
        DateCell::Disputed { value, .. } => view! {
            <span class="text-body">{format!(" \u{2014} {}", format_uncertain_date(value))}</span>
        }
        .into_any(),
        DateCell::Inferred { value, .. } => view! {
            <span class="text-verified italic">
                {format!(" \u{2014} {}", format_inferred_bound(value))}
            </span>
        }
        .into_any(),
    };
    let li_class = if inferred {
        "pl-2 border-l-2 border-verified/30"
    } else if existence {
        "pl-2 border-l-2 border-sepia/20"
    } else {
        "pl-2 border-l-2 border-copper/30"
    };
    let label_class = if inferred {
        "text-verified italic"
    } else if existence {
        "text-sepia/70 italic"
    } else {
        "font-semibold"
    };
    view! {
        <li class=li_class>
            <div>
                <span class=label_class>{label}</span>{date_view}{bullet}{inferred_marker}{marker}
            </div>
            {description.map(|desc| view! {
                <p class="text-xs text-sepia/70 mt-0.5">{desc}</p>
            })}
        </li>
    }
    .into_any()
}

/// The plottable point behind each fact id in the timeline, so a conflict marker
/// can resolve its `facts` to labeled points on a shared axis. A fact id backs the
/// one row whose date it dates.
fn conflict_points(timeline: &[TimelineRow]) -> BTreeMap<FactId, ConflictPoint> {
    let mut points = BTreeMap::new();
    for row in timeline {
        if let Some(point) = row.point() {
            for fact in &row.facts {
                points.entry(*fact).or_insert_with(|| point.clone());
            }
        }
    }
    points
}

/// The conflicts one row participates in, each resolved to its distinct
/// participant points. A row participates when it shares a fact id with the
/// conflict's own `facts`.
fn row_conflicts(
    row: &TimelineRow,
    conflicts: &[ConflictInfo],
    points: &BTreeMap<FactId, ConflictPoint>,
) -> Vec<ResolvedConflict> {
    conflicts
        .iter()
        .filter(|conflict| conflict.facts.iter().any(|fact| row.facts.contains(fact)))
        .map(|conflict| ResolvedConflict {
            summary: conflict_summary(&conflict.kind),
            points: resolve_points(&conflict.facts, points),
        })
        .collect()
}

/// The plain-language line for one temporal conflict. English for now, phrased
/// from the structured [`TemporalConflictKind`] so a future locale layer swaps
/// only this template. The witness and the bound it crossed render at a matched
/// precision ([`matched_precision`]).
fn conflict_summary(kind: &TemporalConflictKind) -> String {
    match kind {
        TemporalConflictKind::ExistedBeforeConstruction {
            witness,
            construction_started,
        } => {
            let (witness, started) = matched_precision(witness, *construction_started);
            format!("Existed at {witness} but construction started {started}")
        }
        TemporalConflictKind::ExistedAfterDemolition {
            witness,
            demolished,
        } => {
            let (witness, demolished) = matched_precision(witness, *demolished);
            format!("Existed at {witness} but demolished {demolished}")
        }
    }
}

/// Render a conflict's witness date and the lifetime bound it crossed so both stay
/// legible. The witness keeps its own precision via [`format_uncertain_date`]; the
/// bound reads as a bare year, or reveals its month and day when it shares the
/// witness's year, so a same-year clash shows the ordering instead of a
/// self-negating "1850 but 1850".
fn matched_precision(witness: &UncertainDate, bound: NaiveDate) -> (String, String) {
    let witness_text = format_uncertain_date(witness);
    let same_year = witness.earliest().map(|d| d.year()) == Some(bound.year());
    let bound_text = if same_year {
        bound.format("%Y-%m-%d").to_string()
    } else {
        bound.year().to_string()
    };
    (witness_text, bound_text)
}

/// The distinct participant points a conflict's facts resolve to, left-to-right.
/// Points sharing a label and a displayed year — the axis prints only the year —
/// collapse to one, so same-year facts don't smudge into an illegible duplicate.
fn resolve_points(
    facts: &[FactId],
    points: &BTreeMap<FactId, ConflictPoint>,
) -> Vec<ConflictPoint> {
    let mut out: Vec<ConflictPoint> = Vec::new();
    for fact in facts {
        if let Some(point) = points.get(fact)
            && !out.iter().any(|seen| {
                seen.label == point.label && seen.instant.year() == point.instant.year()
            })
        {
            out.push(point.clone());
        }
    }
    out.sort_by_key(|p| p.instant);
    out
}

// ==================== Popover shell ====================

/// The disposal-safe popover shared by the citation, conflict, and inferred
/// markers: a trigger glyph that toggles a toned body portaled to
/// `document.body`, closed on an outside click or Escape. The portal escapes the
/// panel's slide transform and `overflow-y-auto` clip; guarded signal access
/// keeps a teardown mid-handler safe.
#[component]
fn Popover(
    tone: PopoverTone,
    trigger_class: impl Fn(bool) -> String + Send + Sync + 'static,
    aria_label: String,
    #[prop(into)] glyph: ViewFn,
    #[prop(into)] body: ViewFn,
) -> impl IntoView {
    let (open, set_open) = signal(false);
    // The glyph's on-screen rect at the moment it was opened. The popover is
    // portaled to `document.body` to escape the panel's slide transform (which
    // would otherwise anchor its `position: fixed` to the panel) and the panel's
    // `overflow-y-auto` clip, so the rect gives it its viewport placement.
    let (anchor, set_anchor) = signal(None::<(f64, f64)>);
    let root_ref = NodeRef::<leptos::html::Span>::new();
    let popover_ref = NodeRef::<leptos::html::Div>::new();

    // Close when a click lands outside both the glyph and its portaled popover.
    // Leptos delegates the button's toggle below the window, so the toggle runs
    // first and the fresh open survives this handler.
    let click_handle =
        window_event_listener(leptos::ev::click, move |ev: leptos::ev::MouseEvent| {
            if !open.try_get_untracked().unwrap_or(false) {
                return;
            }
            let Some(node) = ev
                .target()
                .and_then(|target| target.dyn_into::<web_sys::Node>().ok())
            else {
                return;
            };
            let inside = root_ref
                .get_untracked()
                .is_some_and(|root| root.contains(Some(&node)))
                || popover_ref
                    .get_untracked()
                    .is_some_and(|popover| popover.contains(Some(&node)));
            if !inside {
                let _ = set_open.try_set(false);
            }
        });
    let key_handle =
        window_event_listener(leptos::ev::keydown, move |ev: leptos::ev::KeyboardEvent| {
            if ev.key() == "Escape" && open.try_get_untracked() == Some(true) {
                let _ = set_open.try_set(false);
            }
        });
    on_cleanup(move || {
        click_handle.remove();
        key_handle.remove();
    });

    let toggle = move |_: leptos::ev::MouseEvent| {
        let opening = !open.get_untracked();
        if opening && let Some(el) = root_ref.get_untracked() {
            let rect = el.get_bounding_client_rect();
            set_anchor.set(Some((rect.bottom(), rect.right())));
        }
        set_open.set(opening);
    };

    view! {
        <span node_ref=root_ref>
            <button
                type="button"
                class=move || trigger_class(open.get())
                aria-expanded=move || if open.get() { "true" } else { "false" }
                aria-label=aria_label
                on:click=toggle
            >
                {glyph.run()}
            </button>
        </span>
        <Show when=move || open.get()>
            {
                // `Show` and `Portal` both take a reactive `Fn` children, so
                // neither closure may move a captured value out. Clone the body
                // handle into this block local for the `Show` closure; `run`
                // borrows it inside the `Portal`.
                let body = body.clone();
                view! {
                    <Portal>
                        <div
                            node_ref=popover_ref
                            role="group"
                            class=popover_class(tone)
                            style=move || popover_style(anchor.get())
                        >
                            <div class=top_accent_class(tone)></div>
                            {body.run()}
                        </div>
                    </Portal>
                }
            }
        </Show>
    }
}

// ==================== Citation marker ====================

/// A citation marker: a small superscript mark showing a field's source count,
/// neutral normally and amber when the field is contested. Tapping it toggles a
/// popover listing the sources — each linked to its source when it has one — or,
/// for a contested field, the rival claims. An outside click or Escape closes it.
#[component]
fn CitationMarker(lines: CitationLines) -> impl IntoView {
    // The variant is the bullet's meaning: rival claims read as the disputed
    // badge, agreeing sources as the neutral one.
    let (disputed, entries) = match lines {
        CitationLines::Sources(entries) => (false, entries),
        CitationLines::Rivals(entries) => (true, entries),
    };
    let count = entries.len();

    let aria_label = if disputed {
        format!("{count} conflicting sources")
    } else if count == 1 {
        "1 source".to_string()
    } else {
        format!("{count} sources")
    };
    let heading = if disputed {
        format!("Disputed \u{00b7} {count} claims")
    } else if count == 1 {
        "Source".to_string()
    } else {
        "Sources".to_string()
    };
    let tone = if disputed {
        PopoverTone::Disputed
    } else {
        PopoverTone::Neutral
    };

    view! {
        <Popover
            tone=tone
            trigger_class=move |open| bullet_class(disputed, open)
            aria_label=aria_label
            glyph=move || {
                view! {
                    <span class="[text-box-trim:trim-both] [text-box-edge:cap_alphabetic]">
                        {count.to_string()}
                    </span>
                }
            }
            body=move || {
                // Clone the popover data into this reactive `Fn` body so it only
                // borrows the captured values, rebuilding on each portal mount.
                let heading = heading.clone();
                let entries = entries.clone();
                view! {
                    <div class=header_class(tone)>
                        <span>{heading}</span>
                    </div>
                    <ul class="py-1 max-h-64 overflow-y-auto">
                        {citation_entry_views(entries)}
                    </ul>
                }
            }
        />
    }
}

/// The popover's source lines: one `<li>` per citation, linked to its source
/// when the source has a stable URL. Takes the entries by value so the portaled
/// popover can rebuild them on each render.
fn citation_entry_views(entries: Vec<CiteEntry>) -> Vec<AnyView> {
    entries
        .into_iter()
        .map(|entry| {
            let CiteEntry { value, source, url } = entry;
            let source = source.map(|s| {
                view! {
                    <div class="font-mono text-[0.65rem] text-sepia mt-0.5 tracking-wide">{s}</div>
                }
            });
            let line = view! {
                <div class="font-serif text-body text-sm tabular-nums group-hover:text-copper group-hover:underline">
                    {value}
                </div>
                {source}
            }
            .into_any();
            let body = match url {
                Some(href) => view! {
                    <a
                        href=href
                        target="_blank"
                        rel="noopener noreferrer"
                        class="group block px-3 py-2 hover:bg-sepia/10 transition-colors"
                    >
                        {line}
                    </a>
                }
                .into_any(),
                None => view! { <div class="px-3 py-2">{line}</div> }.into_any(),
            };
            view! {
                <li class="border-b border-sepia/10 last:border-b-0">{body}</li>
            }
            .into_any()
        })
        .collect()
}

/// The glyph classes: a small superscript footnote mark, sized in `em` so it
/// scales with the host text. A neutral outline that darkens on hover/open, or
/// the amber contested tone.
fn bullet_class(disputed: bool, open: bool) -> String {
    let base = "inline-flex items-center justify-center align-[0.5em] \
                min-w-[1.3em] h-[1.3em] px-[0.25em] ml-[0.15em] \
                rounded-full border text-[0.6em] font-sans font-bold leading-none \
                cursor-pointer transition-colors";
    let tone = match (disputed, open) {
        (true, true) => "text-disputed-deep border-disputed-deep bg-disputed/20",
        (true, false) => {
            "text-disputed-deep border-disputed bg-disputed/10 \
             hover:border-disputed-deep hover:bg-disputed/20"
        }
        (false, true) => "text-body border-body bg-sepia/10",
        (false, false) => {
            "text-secondary border-secondary/50 hover:text-body hover:border-body hover:bg-sepia/10"
        }
    };
    format!("{base} {tone}")
}

/// The popover width in pixels — matched by the inline placement in
/// [`popover_style`].
const POPOVER_WIDTH_PX: f64 = 240.0;

/// The hue a citation/marker popover carries: neutral for a plain source list,
/// disputed (amber) for a contested field or a temporal conflict, inferred (sage)
/// for a bound the solver rules derived. Drives the border, accent, and header
/// color.
#[derive(Clone, Copy)]
enum PopoverTone {
    Neutral,
    Disputed,
    Inferred,
}

/// The popover panel classes. `position: fixed` (placed by [`popover_style`])
/// lets it escape the panel's `overflow-y-auto` clipping. The border picks up the
/// tone's hue. Mounted only while open (via `<Show>`), so it owns no visibility
/// toggle.
fn popover_class(tone: PopoverTone) -> String {
    let base = "fixed z-20 text-left rounded-md bg-parchment shadow-lg \
                overflow-hidden font-sans border";
    let border = match tone {
        PopoverTone::Neutral => "border-sepia/20",
        PopoverTone::Disputed => "border-disputed/40",
        PopoverTone::Inferred => "border-verified/40",
    };
    format!("{base} {border}")
}

/// The popover's fixed placement from the bullet's measured rect: dropped just
/// below the glyph with its right edge aligned to the glyph's, extending left
/// and clamped to stay within the viewport. Empty until the bullet is measured
/// on first open.
fn popover_style(anchor: Option<(f64, f64)>) -> String {
    let Some((bottom, right)) = anchor else {
        return String::new();
    };
    let top = bottom + 6.0;
    let left = (right - POPOVER_WIDTH_PX).max(8.0);
    format!("top: {top}px; left: {left}px; width: {POPOVER_WIDTH_PX}px;")
}

/// The 2px top accent, in the popover's tone: amber for a contested field, sage
/// for an inferred bound, muted otherwise.
fn top_accent_class(tone: PopoverTone) -> &'static str {
    match tone {
        PopoverTone::Neutral => "h-0.5 bg-sepia/50",
        PopoverTone::Disputed => "h-0.5 bg-disputed",
        PopoverTone::Inferred => "h-0.5 bg-verified",
    }
}

/// The popover header row classes, tinted to the popover's tone.
fn header_class(tone: PopoverTone) -> String {
    let base = "flex justify-between items-baseline px-3 py-2 border-b border-sepia/15 \
                text-[0.6rem] font-bold uppercase tracking-wider";
    let color = match tone {
        PopoverTone::Neutral => "text-secondary",
        PopoverTone::Disputed => "text-disputed-deep",
        PopoverTone::Inferred => "text-verified-deep",
    };
    format!("{base} {color}")
}

// ==================== Conflict marker ====================

/// A conflict marker: a small amber alert disc beside the citation bullet on a
/// timeline row whose date takes part in an entity-level temporal conflict.
/// Tapping it opens a [`Popover`] naming each clash in plain language and
/// plotting its participating facts on a small inline time-axis.
#[component]
fn ConflictMarker(conflicts: Vec<ResolvedConflict>) -> impl IntoView {
    let count = conflicts.len();
    let aria_label = if count == 1 {
        "1 date conflict".to_string()
    } else {
        format!("{count} date conflicts")
    };
    let heading = if count == 1 {
        "Date conflict".to_string()
    } else {
        format!("{count} date conflicts")
    };

    view! {
        <Popover
            tone=PopoverTone::Disputed
            trigger_class=conflict_glyph_class
            aria_label=aria_label
            glyph=move || {
                view! {
                    <span class="[text-box-trim:trim-both] [text-box-edge:cap_alphabetic]" aria-hidden="true">
                        "!"
                    </span>
                }
            }
            body=move || {
                // Clone the popover data into this reactive `Fn` body so it only
                // borrows the captured values, rebuilding on each portal mount.
                let heading = heading.clone();
                let conflicts = conflicts.clone();
                view! {
                    <div class=header_class(PopoverTone::Disputed)>
                        <span>{heading}</span>
                    </div>
                    <div class="p-3 space-y-4 max-h-80 overflow-y-auto">
                        {conflict_detail_views(conflicts)}
                    </div>
                }
            }
        />
    }
}

/// The popover body: one block per conflict — its plain-language summary and the
/// inline time-axis plotting its participating facts.
fn conflict_detail_views(conflicts: Vec<ResolvedConflict>) -> Vec<AnyView> {
    conflicts
        .into_iter()
        .map(|conflict| {
            let summary = conflict.summary.clone();
            let axis = conflict_axis(&conflict);
            view! {
                <div>
                    <p class="font-serif text-sm text-body mb-2">{summary}</p>
                    {axis}
                </div>
            }
            .into_any()
        })
        .collect()
}

/// One conflict's time-axis: each participant plotted at its date, the witness
/// (out-of-lifetime evidence) in the conflict color and bracketed to the bookend
/// it sits on the wrong side of. A self-contained inline SVG set via `inner_html`,
/// its colors drawn from the theme's CSS custom properties.
fn conflict_axis(conflict: &ResolvedConflict) -> AnyView {
    match build_conflict_svg(conflict) {
        Some(markup) => view! { <div class="w-full" inner_html=markup></div> }.into_any(),
        None => view! {
            <p class="text-xs text-sepia/50 italic">"No dated participants to plot"</p>
        }
        .into_any(),
    }
}

/// A point's scaling position: its instant as a day count, monotonic in date so
/// the axis lays participants out in order.
fn point_pos(point: &ConflictPoint) -> f64 {
    f64::from(point.instant.num_days_from_ce())
}

/// The index of the anchor nearest a witness in time — the bookend it brackets
/// to. `None` when the conflict names no anchor.
fn nearest_anchor_index(witness: &ConflictPoint, anchors: &[&ConflictPoint]) -> Option<usize> {
    anchors
        .iter()
        .enumerate()
        .min_by_key(|(_, anchor)| (witness.instant - anchor.instant).num_days().abs())
        .map(|(i, _)| i)
}

/// One bracket per anchor that owns witnesses: the anchor paired with the farthest
/// witness grouped under it (each witness grouped to its nearest anchor). A single
/// bracket then encloses every out-of-lifetime witness on that anchor's side, so
/// the bracket count follows the anchors, not the witnesses.
fn conflict_brackets(points: &[ConflictPoint]) -> Vec<(&ConflictPoint, &ConflictPoint)> {
    let anchors: Vec<&ConflictPoint> = points
        .iter()
        .filter(|p| p.role == ConflictRole::Anchor)
        .collect();
    anchors
        .iter()
        .enumerate()
        .filter_map(|(gi, anchor)| {
            let farthest = points
                .iter()
                .filter(|p| p.role == ConflictRole::Witness)
                .filter(|w| nearest_anchor_index(w, &anchors) == Some(gi))
                .max_by_key(|w| (w.instant - anchor.instant).num_days().abs())?;
            Some((*anchor, farthest))
        })
        .collect()
}

/// Build the inline-SVG markup for a conflict's time-axis, or `None` when no
/// participant carries a plottable date. Positions scale to the participants'
/// date range with padding; each witness point and its "before"/"after" bracket
/// to the nearest anchor use the amber conflict tones.
fn build_conflict_svg(conflict: &ResolvedConflict) -> Option<String> {
    const VB_W: f64 = 240.0;
    const VB_H: f64 = 88.0;
    const PAD_X: f64 = 30.0;
    const AXIS_Y: f64 = 54.0;

    if conflict.points.is_empty() {
        return None;
    }

    let inner = VB_W - 2.0 * PAD_X;
    let right = VB_W - PAD_X;
    let tick_top = AXIS_Y - 5.0;
    let tick_bot = AXIS_Y + 5.0;
    let label_y = AXIS_Y - 11.0;
    let year_y = AXIS_Y + 17.0;

    let min = conflict
        .points
        .iter()
        .map(point_pos)
        .fold(f64::INFINITY, f64::min);
    let max = conflict
        .points
        .iter()
        .map(point_pos)
        .fold(f64::NEG_INFINITY, f64::max);
    let range = if (max - min).abs() < 1.0 {
        1.0
    } else {
        max - min
    };
    let lo = min - 0.18 * range;
    let span = (max + 0.18 * range) - lo;
    let x_of = |pos: f64| PAD_X + (pos - lo) / span * inner;

    let mut body = String::new();
    body.push_str(&format!(
        r#"<line x1="{PAD_X:.1}" y1="{AXIS_Y:.1}" x2="{right:.1}" y2="{AXIS_Y:.1}" style="stroke:var(--color-sepia);stroke-opacity:0.3" stroke-width="1"/>"#
    ));

    // Draw each anchor's bracket, stepping successive ones up so they stay legible;
    // clamp the offset so a heavily-disputed bookend's many anchors can't climb off
    // the top of the viewBox.
    for (gi, (anchor, w)) in conflict_brackets(&conflict.points).into_iter().enumerate() {
        let wx = x_of(point_pos(w));
        let ax = x_of(point_pos(anchor));
        let (x1, x2) = if wx <= ax { (wx, ax) } else { (ax, wx) };
        let mid = (x1 + x2) / 2.0;
        let bracket_y = (AXIS_Y - 24.0 - 6.0 * gi as f64).max(12.0);
        let foot = AXIS_Y - 9.0;
        let label_pos = bracket_y - 3.0;
        let relation = if w.instant < anchor.instant {
            "before"
        } else {
            "after"
        };
        body.push_str(&format!(
            r#"<path d="M {x1:.1} {foot:.1} L {x1:.1} {bracket_y:.1} L {x2:.1} {bracket_y:.1} L {x2:.1} {foot:.1}" style="stroke:var(--color-disputed)" fill="none" stroke-width="1"/>"#
        ));
        body.push_str(&format!(
            r#"<text x="{mid:.1}" y="{label_pos:.1}" text-anchor="middle" font-size="8" font-style="italic" style="fill:var(--color-disputed-deep)">{relation}</text>"#
        ));
    }

    for point in &conflict.points {
        let cx = x_of(point_pos(point));
        let (dot, year_fill) = match point.role {
            ConflictRole::Witness => ("var(--color-disputed)", "var(--color-disputed-deep)"),
            ConflictRole::Anchor => ("var(--color-sepia)", "var(--color-sepia)"),
        };
        let label = svg_escape(&point.label);
        // Bare year, matching the conflict line's bare year; `%Y` would zero-pad
        // an ancient year ("0082") and read apart from it.
        let year_text = point.instant.year().to_string();
        let year = svg_escape(&year_text);
        body.push_str(&format!(
            r#"<line x1="{cx:.1}" y1="{tick_top:.1}" x2="{cx:.1}" y2="{tick_bot:.1}" style="stroke:var(--color-sepia);stroke-opacity:0.4" stroke-width="1"/>"#
        ));
        body.push_str(&format!(
            r#"<circle cx="{cx:.1}" cy="{AXIS_Y:.1}" r="4" style="fill:{dot}"/>"#
        ));
        body.push_str(&format!(
            r#"<text x="{cx:.1}" y="{label_y:.1}" text-anchor="middle" font-size="7.5" style="fill:var(--color-sepia);fill-opacity:0.85">{label}</text>"#
        ));
        body.push_str(&format!(
            r#"<text x="{cx:.1}" y="{year_y:.1}" text-anchor="middle" font-size="9" style="fill:{year_fill}">{year}</text>"#
        ));
    }

    Some(format!(
        r#"<svg viewBox="0 0 {VB_W:.0} {VB_H:.0}" class="w-full h-auto" role="img" aria-hidden="true">{body}</svg>"#
    ))
}

/// The disc geometry shared by the conflict and inferred markers: a
/// superscript-sized solid disc holding a parchment cut-out glyph, matching the
/// citation bullet's dimensions so all three ride the row alike. Only the
/// background tone differs between them.
fn disc_base() -> &'static str {
    "inline-flex items-center justify-center align-[0.5em] \
     min-w-[1.3em] h-[1.3em] px-[0.25em] ml-[0.15em] \
     rounded-full text-parchment text-[0.6em] font-sans font-bold leading-none \
     cursor-pointer transition-colors"
}

/// The conflict marker's tone over [`disc_base`]: a solid amber disc, its
/// parchment `!` a cut-out, deepening while its popover is open — solid alert
/// against the citation bullet's neutral outline.
fn conflict_glyph_class(open: bool) -> String {
    let tone = if open {
        "bg-disputed-deep"
    } else {
        "bg-disputed hover:bg-disputed-deep"
    };
    format!("{} {tone}", disc_base())
}

// ==================== Inferred marker ====================

/// An inferred marker: a solid sage disc beside the citation bullet on a row whose
/// date one or more solver rules reached past what the sources stated. Tapping it
/// opens a sage-toned [`Popover`] naming the claim those rules make, the reason
/// they give for it, and the citations of the facts they consumed.
#[component]
fn InferredMarker(
    value: UncertainDate,
    rules: NonEmptyVec<DerivationRule>,
    witnesses: Vec<CiteEntry>,
) -> impl IntoView {
    let (claim, reason) = inferred_text(&rules, &value);
    let aria_label = format!("inferred date, {claim}");
    let heading = format!("Inferred \u{00b7} {claim}");

    view! {
        <Popover
            tone=PopoverTone::Inferred
            trigger_class=inferred_glyph_class
            aria_label=aria_label
            glyph=move || {
                view! {
                    <span class="[text-box-trim:trim-both] [text-box-edge:cap_alphabetic]" aria-hidden="true">
                        "i"
                    </span>
                }
            }
            body=move || {
                // Clone the popover data into this reactive `Fn` body so it only
                // borrows the captured values, rebuilding on each portal mount.
                let heading = heading.clone();
                let reason = reason.clone();
                let witnesses = witnesses.clone();
                view! {
                    <div class=header_class(PopoverTone::Inferred)>
                        <span>{heading}</span>
                    </div>
                    <p class="px-3 py-2 font-serif text-sm text-body border-b border-sepia/15">
                        {reason}
                    </p>
                    <ul class="py-1 max-h-64 overflow-y-auto">
                        {citation_entry_views(witnesses)}
                    </ul>
                }
            }
        />
    }
}

/// The claim a derived bound asserts and the evidence behind it, both phrased
/// from the structured [`DerivationRule`]s and the bound's own value — a future
/// locale layer swaps only these templates, and a new rule brings its own words.
///
/// The lead rule names the claim; a chain of them wants a line each, which the
/// panel can grow into once more than one rule exists.
fn inferred_text(rules: &NonEmptyVec<DerivationRule>, value: &UncertainDate) -> (String, String) {
    match rules.first() {
        DerivationRule::ExistenceWitness => {
            let w = value
                .latest_bound()
                .map(format_date_bound)
                .unwrap_or_else(|| format_uncertain_date(value));
            (format!("built by {w}"), format!("Recorded existing in {w}"))
        }
    }
}

/// A one-sided derived bound as a compact inline value — "≤ W" when open below,
/// "≥ W" when open above — so the reader sees the boundary and its direction.
fn format_inferred_bound(value: &UncertainDate) -> String {
    match (value.earliest_bound(), value.latest_bound()) {
        (None, Some(hi)) => format!("\u{2264} {}", format_date_bound(hi)),
        (Some(lo), None) => format!("\u{2265} {}", format_date_bound(lo)),
        _ => format_uncertain_date(value),
    }
}

/// The inferred marker's tone over [`disc_base`]: a solid sage disc, its
/// parchment `i` a cut-out, deepening while its popover is open — a derived-value
/// tag in the sage tone.
fn inferred_glyph_class(open: bool) -> String {
    let tone = if open {
        "bg-verified-deep"
    } else {
        "bg-verified hover:bg-verified-deep"
    };
    format!("{} {tone}", disc_base())
}

/// Neutralize the ampersand and angle brackets before splicing generated text
/// into the raw inline-SVG markup.
fn svg_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
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

/// Format an `UncertainDate` for display. A single interval reads as one value
/// or range; a disjoint union — the shape a contested date takes — reads as its
/// intervals joined by " / " (e.g. "1160 / 1163"), preserving the disjunction.
fn format_uncertain_date(date: &UncertainDate) -> String {
    let intervals = date.intervals();
    if intervals.is_empty() {
        return "date unknown".to_string();
    }
    intervals
        .iter()
        .map(format_interval)
        .collect::<Vec<_>>()
        .join(" / ")
}

/// Format one interval, truncating each bound to its precision.
fn format_interval(range: &TimeRange) -> String {
    match (range.earliest(), range.latest()) {
        (Some(earliest), Some(latest)) if earliest == latest => format_date_bound(earliest),
        (Some(earliest), Some(latest)) => format!(
            "{} \u{2013} {}",
            format_date_bound(earliest),
            format_date_bound(latest)
        ),
        (Some(earliest), None) => format!("after {}", format_date_bound(earliest)),
        (None, Some(latest)) => format!("before {}", format_date_bound(latest)),
        (None, None) => "date unknown".to_string(),
    }
}

// ==================== External links ====================

/// Build a display link for one external reference: the source-name label the
/// panel shows, the canonical URL, and the badge for the sources behind the
/// reference. The URL is [`ExternalReference::to_url`]'s job — this only owns
/// the presentation label (which stays in the web). An `UnmodeledUrl` with no
/// extractable host renders no link.
fn link_info(reference: &Attributed<ExternalReference, ImageId>) -> Option<LinkInfo> {
    let label = source_label(&reference.value)?;
    let citations = cited_field(&label, &reference.sources);
    Some(LinkInfo {
        label,
        url: reference.value.to_url().to_string(),
        citations,
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
        ExternalReference::UnmodeledUrl { url } => url.host_str().map(str::to_string)?,
    })
}

#[cfg(test)]
mod tests {
    use chrono::Datelike;

    use super::{ConflictPoint, ConflictRole, NaiveDate, conflict_brackets};

    fn point(label: &str, ymd: (i32, u32, u32), role: ConflictRole) -> Option<ConflictPoint> {
        Some(ConflictPoint {
            label: label.to_string(),
            instant: NaiveDate::from_ymd_opt(ymd.0, ymd.1, ymd.2)?,
            role,
        })
    }

    #[test]
    fn witnesses_before_one_anchor_share_a_bracket_to_the_farthest() -> Result<(), String> {
        // Three witnesses all before a single construction anchor collapse to one
        // bracket, reaching the earliest — the most out-of-lifetime — witness.
        let built = point("Constructed", (1850, 1, 1), ConflictRole::Anchor).ok_or("bad date")?;
        let near =
            point("Known to exist", (1840, 1, 1), ConflictRole::Witness).ok_or("bad date")?;
        let far = point("Known to exist", (1820, 1, 1), ConflictRole::Witness).ok_or("bad date")?;
        let mid = point("Known to exist", (1830, 1, 1), ConflictRole::Witness).ok_or("bad date")?;
        let points = vec![built, near, far, mid];
        let brackets = conflict_brackets(&points);
        assert_eq!(brackets.len(), 1);
        let &(anchor, farthest) = brackets.first().ok_or("no bracket")?;
        assert_eq!(anchor.label.as_str(), "Constructed");
        assert_eq!(farthest.instant.year(), 1820);
        Ok(())
    }

    #[test]
    fn a_witness_by_each_anchor_gets_its_own_bracket() -> Result<(), String> {
        let built = point("Constructed", (1850, 1, 1), ConflictRole::Anchor).ok_or("bad date")?;
        let demolished =
            point("Demolished", (1950, 1, 1), ConflictRole::Anchor).ok_or("bad date")?;
        let early =
            point("Known to exist", (1845, 1, 1), ConflictRole::Witness).ok_or("bad date")?;
        let late =
            point("Known to exist", (1955, 1, 1), ConflictRole::Witness).ok_or("bad date")?;
        let points = vec![built, demolished, early, late];
        assert_eq!(conflict_brackets(&points).len(), 2);
        Ok(())
    }
}
