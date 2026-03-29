use std::cell::RefCell;
use std::rc::Rc;

use leptos::prelude::*;
use send_wrapper::SendWrapper;
use wasm_bindgen::JsCast;

use crate::api;
use crate::components::dismiss_button::DismissButton;
use crate::components::map::{EntityPickerEntry, EntitySelection, SelectedEntity};

/// Side panel showing entity details or a disambiguation picker.
///
/// The panel is always rendered (for CSS transitions) but translated off-screen
/// when nothing is selected. On desktop it slides in from the right; on mobile
/// it slides up as a bottom sheet.
#[component]
pub fn EntityDetailPanel(api_client: Rc<RefCell<Option<api::ApiClient>>>) -> impl IntoView {
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
        {
            if let Ok(el) = main.dyn_into::<web_sys::HtmlElement>() {
                let _ = el.focus();
            }
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
                let aria = format!("{} ({})", entry.name, entry.entity_type);
                view! {
                    <li>
                        <button
                            class="w-full text-left p-3 rounded-lg bg-parchment hover:bg-copper/10 \
                                   border border-copper/20 cursor-pointer transition-colors"
                            on:click=select
                            aria-label=aria
                        >
                            <span class="text-sm font-semibold text-ink">{entry.name.clone()}</span>
                            <span class="text-xs text-sepia/70 ml-2 font-sans uppercase">{entry.entity_type.clone()}</span>
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
    id: String,
    api_client: Rc<RefCell<Option<api::ApiClient>>>,
) -> impl IntoView {
    let id_clone = id.clone();
    let (retry_count, set_retry_count) = signal(0u32);
    let detail = LocalResource::new(move || {
        // Include retry_count in the dependency so incrementing it re-fetches.
        let _retry = retry_count.get();
        let id = id_clone.clone();
        let api = api_client.clone();
        async move {
            let client = crate::api::get_or_init_api_client(&api)
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
                                <h3 class="text-base font-semibold text-ink mb-1">
                                    {entity.name.unwrap_or_else(|| "Unnamed entity".to_string())}
                                </h3>
                                <p class="text-xs text-sepia/70 mb-3 font-sans uppercase tracking-wide">
                                    {entity.entity_type}
                                </p>

                                // Timeline
                                {(!entity.transitions.is_empty()).then(|| view! {
                                    <div class="mb-3">
                                        <p class="text-xs font-sans text-copper font-semibold mb-1">"Timeline"</p>
                                        <ul class="text-sm text-sepia space-y-1.5">
                                            {entity.transitions.iter().map(|t| view! {
                                                <li class="pl-2 border-l-2 border-copper/30">
                                                    <span class="font-semibold capitalize">{t.transition_type.clone()}</span>
                                                    {t.date.as_ref().map(|d| view! {
                                                        <span class="text-sepia/70">{format!(" \u{2014} {d}")}</span>
                                                    })}
                                                </li>
                                            }).collect::<Vec<_>>()}
                                        </ul>
                                    </div>
                                })}

                                // Links
                                {(!entity.links.is_empty()).then(|| view! {
                                    <div class="mb-3">
                                        <p class="text-xs font-sans text-copper font-semibold mb-1">"Links"</p>
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

#[derive(Debug, Clone)]
struct TransitionSummary {
    transition_type: String,
    date: Option<String>,
}

#[derive(Debug, Clone)]
struct LinkInfo {
    label: String,
    url: String,
}

#[derive(Debug, Clone)]
struct EntityDetailView {
    name: Option<String>,
    entity_type: String,
    transitions: Vec<TransitionSummary>,
    links: Vec<LinkInfo>,
}

/// Return the browser's preferred language prefix (e.g. "en" from "en-US"),
/// falling back to "en" if the navigator API is unavailable.
fn browser_language_prefix() -> String {
    web_sys::window()
        .map(|w| w.navigator().language().unwrap_or_default())
        .and_then(|lang| lang.split('-').next().map(String::from))
        .unwrap_or_else(|| "en".to_string())
}

// TODO: Replace this hand-rolled JSON parsing with proper Deserialize structs
// once the API crate's types are shared with the web crate (feature-gate work).
// The current approach silently degrades if the API response shape changes.
async fn fetch_entity_detail(id: &str, client: &api::ApiClient) -> Result<EntityDetailView, String> {
    let json = client.fetch_entity(id).await.map_err(|e| e.to_string())?;

    let entity = json.get("entity").ok_or("missing entity field")?;

    let lang_prefix = browser_language_prefix();
    let name = entity
        .get("names")
        .and_then(|n| n.as_array())
        .and_then(|names| {
            names
                .iter()
                .find(|n| {
                    n.get("value")
                        .and_then(|v| v.get("language"))
                        .and_then(|l| l.as_str())
                        .is_some_and(|l| l.starts_with(&lang_prefix))
                })
                .or_else(|| names.first())
        })
        .and_then(|n| n.get("value"))
        .and_then(|v| v.get("name"))
        .and_then(|n| n.as_str())
        .map(String::from);

    let entity_type = entity
        .get("entity_type")
        .and_then(|t| t.as_str())
        .unwrap_or("unknown")
        .to_string();

    let transitions = entity
        .get("transitions")
        .and_then(|t| t.as_array())
        .map(|arr| arr.iter().map(parse_transition).collect())
        .unwrap_or_default();

    let links = json
        .get("links")
        .and_then(|l| l.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|link| {
                    let target = link.get("target")?;
                    let link_type = link
                        .get("link_type")
                        .and_then(|t| t.as_str())
                        .unwrap_or("link");
                    let (url, label) = parse_link(link_type, target)?;
                    Some(LinkInfo { label, url })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(EntityDetailView {
        name,
        entity_type,
        transitions,
        links,
    })
}

fn parse_transition(t: &serde_json::Value) -> TransitionSummary {
    let transition_type = t
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    TransitionSummary {
        transition_type,
        date: extract_transition_date(t),
    }
}

fn extract_transition_date(t: &serde_json::Value) -> Option<String> {
    for field in ["completed_at", "occurred_at", "started_at"] {
        if let Some(date_str) = t.get(field).and_then(extract_uncertain_date) {
            return Some(date_str);
        }
    }
    None
}

/// The `UncertainDate` is a tagged enum. The `precise` variant has:
/// `{ "type": "precise", "datetime": "...", "precision": "year"|"month"|"day" }`
fn extract_uncertain_date(cited: &serde_json::Value) -> Option<String> {
    let value = cited.get("value")?;
    let datetime = value.get("datetime").and_then(|d| d.as_str())?;
    let precision = value
        .get("precision")
        .and_then(|p| p.as_str())
        .unwrap_or("day");
    Some(match precision {
        "year" => datetime.get(..4)?.to_string(),
        "month" => datetime.get(..7)?.to_string(),
        _ => datetime.get(..10)?.to_string(),
    })
}

use chronoscope_core::links::LinkTarget;

/// Parse a link target from JSON and return its URL and a human-readable label.
fn parse_link(link_type: &str, target_json: &serde_json::Value) -> Option<(String, String)> {
    let target: LinkTarget = serde_json::from_value(target_json.clone()).ok()?;
    let url = target.to_url().to_string();

    // Filter out non-HTTP URLs (e.g., javascript:)
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return None;
    }

    let target_label = match &target {
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
        "same_as" => target_label,
        _ => {
            let rel = link_type.replace('_', " ");
            format!("{rel}: {target_label}")
        }
    };

    Some((url, label))
}

/// Extract the domain from a URL string (e.g., `"https://example.com/path"` → `"example.com"`).
fn extract_domain(url: &str) -> Option<String> {
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    Some(after_scheme.split('/').next()?.to_string())
}
