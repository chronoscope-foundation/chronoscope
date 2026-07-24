use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use futures_util::future::{AbortHandle, Abortable, join_all};
use leptos::prelude::*;
use send_wrapper::SendWrapper;
use serde::Serialize;
use serde_json::json;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use chrono::NaiveDate;

use chronoscope_api_client::EntityId;
use chronoscope_core::geo::{GeoPoint, QuadLevel, Viewport};
use chronoscope_core::lifespan::ExistenceState;

use crate::api;
use crate::api::Snapshot;
use crate::maplibre;

/// Serialize a value to a JS object using JSON-compatible mode.
///
/// `serde_wasm_bindgen`'s default serializer produces JS `Map` objects for
/// Rust maps/structs. MapLibre (and most JS APIs) expect plain `Object`s
/// with dot-accessible properties. This helper always uses `json_compatible()`
/// to avoid that footgun.
pub(crate) fn to_js<T: Serialize>(value: &T) -> Result<JsValue, serde_wasm_bindgen::Error> {
    value.serialize(&serde_wasm_bindgen::Serializer::json_compatible())
}

// ==================== Constants ====================

/// `OpenFreeMap` Liberty style — a free, open-source map style.
/// <https://openfreemap.org/>
const MAP_STYLE_URL: &str = "https://tiles.openfreemap.org/styles/liberty";

/// Initial map center: Rome, Italy (roughly central in the Mediterranean).
const INITIAL_CENTER_LNG: f64 = 12.4964;
const INITIAL_CENTER_LAT: f64 = 41.9028;
const INITIAL_ZOOM: f64 = 3.0;

/// The map's max zoom, the GeoJSON source `clusterMaxZoom`, and the ceiling on
/// the zoom-tracking tile level, held equal. A badge still clustered at this
/// zoom is coincident points, resolved via the leaves picker.
const MAX_ZOOM: u8 = 24;
/// Map min zoom, floored above the world-wrap point so a full-globe view
/// doesn't collapse the viewport bounds to an antimeridian sliver.
const MIN_ZOOM: u8 = 1;
/// Proximity-cluster merge radius in CSS pixels — the client folds tile cells
/// closer than this into one badge (the near-overlap fix).
const CLUSTER_RADIUS: u32 = 50;
/// Ring of extra container tiles fetched beyond the viewport on each side, so a
/// small pan lands on already-cached cells instead of a blank strip.
const TILE_MARGIN: u32 = 1;
/// Upper bound on the tiles a single pass may enumerate. `level = floor(zoom)`
/// keeps a real view to a screenful of tiles, so this never trips in normal use;
/// it's the guardrail that stops a pathological zoom/viewport from firing a
/// request storm. A pass over the cap keeps the prior render and skips fetching.
const MAX_TILES_PER_PASS: usize = 512;
/// Latitude clamp for the substituted full-world viewport — the web-mercator
/// limit, past which the projection diverges.
const WORLD_VIEW_LAT: f64 = 85.0;
/// Leaf cap for a terminal cluster's disambiguation picker.
const CLUSTER_LEAVES_LIMIT: u32 = 50;
/// Symbol fade window (ms) — a touch above MapLibre's 300 ms default so
/// thumbnail and label symbols ease in as a cluster splits rather than snapping.
const SYMBOL_FADE_MS: f64 = 400.0;

/// Debounce delay (ms) for map pan/zoom → entity re-fetch.
const MOVEEND_DEBOUNCE_MS: i32 = 150;

/// Name of the GeoJSON source added to the map.
const ENTITY_SOURCE_ID: &str = "entities";

/// Name of the circle layer for entity markers.
pub(crate) const ENTITY_CIRCLES_LAYER: &str = "entity-circles";

/// Name of the badge layer — the only layer a cluster draws on, covering both
/// MapLibre proximity clusters and lone server `Expand` cells.
pub(crate) const ENTITY_BADGE_LAYER: &str = "entity-badge";

/// DOM event name signaling map mount completion (used by test hooks).
#[cfg(feature = "test-hooks")]
pub(crate) const MAP_READY_EVENT: &str = "chronoscope-map-ready";
/// DOM event name signaling entity fetch completion (used by test hooks).
#[cfg(feature = "test-hooks")]
pub(crate) const FETCH_COMPLETE_EVENT: &str = "chronoscope-fetch-complete";

/// Monotonic count of fetches that have *settled* (returned a result, success
/// or failure). Aborted fetches do not bump it because the future is dropped
/// before the settle point. Tests sample this counter before triggering an
/// action, then wait for it to advance — that's how "wait for the fetch
/// that follows my action" is expressed without a generation race.
#[cfg(feature = "test-hooks")]
thread_local! {
    static FETCH_SETTLED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(feature = "test-hooks")]
pub(crate) fn current_fetch_settled() -> u64 {
    FETCH_SETTLED.with(std::cell::Cell::get)
}

#[cfg(feature = "test-hooks")]
fn dispatch_window_event(name: &str) {
    if let Some(window) = web_sys::window()
        && let Ok(event) = web_sys::Event::new(name)
    {
        let _ = window.dispatch_event(&event);
    }
}

/// Bump a settled-counter and dispatch a `CustomEvent` carrying the post-bump
/// count as `event.detail`. Used for both the fetch-settled and
/// thumbnails-loaded counters — listeners use the detail to identify which
/// completion they observed.
#[cfg(feature = "test-hooks")]
fn bump_and_dispatch(
    counter: &'static std::thread::LocalKey<std::cell::Cell<u64>>,
    event_name: &str,
) {
    let count = counter.with(|c| {
        let next = c.get() + 1;
        c.set(next);
        next
    });
    if let Some(window) = web_sys::window() {
        let init = web_sys::CustomEventInit::new();
        init.set_detail(&JsValue::from_f64(count as f64));
        if let Ok(event) = web_sys::CustomEvent::new_with_event_init_dict(event_name, &init) {
            let _ = window.dispatch_event(&event);
        }
    }
}

#[cfg(feature = "test-hooks")]
fn record_fetch_settled() {
    bump_and_dispatch(&FETCH_SETTLED, FETCH_COMPLETE_EVENT);
}

/// Name of the symbol layer for thumbnail markers.
pub(crate) const ENTITY_THUMBNAILS_LAYER: &str = "entity-thumbnails";

/// DOM event name signaling thumbnail image loading completion (used by test hooks).
#[cfg(feature = "test-hooks")]
pub(crate) const THUMBNAILS_LOADED_EVENT: &str = "chronoscope-thumbnails-loaded";

/// Counter mirroring `FETCH_SETTLED` for thumbnails-loaded events. Lets tests
/// sample-then-await: `prev = currentThumbnailsLoaded(); pan(); waitForThumbnailsLoadedAfter(prev)`.
/// Closes the listener-attach race that any one-shot "wait for next event"
/// hook would have.
#[cfg(feature = "test-hooks")]
thread_local! {
    static THUMBNAILS_LOADED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(feature = "test-hooks")]
pub(crate) fn current_thumbnails_loaded() -> u64 {
    THUMBNAILS_LOADED.with(std::cell::Cell::get)
}

#[cfg(feature = "test-hooks")]
fn record_thumbnails_loaded() {
    bump_and_dispatch(&THUMBNAILS_LOADED, THUMBNAILS_LOADED_EVENT);
}

/// Size (CSS px) of circular thumbnail images on the map.
// TODO: Revisit for mobile — 96px may be too large on small screens.
// Consider scaling down to ~64px based on viewport width.
const THUMBNAIL_SIZE: u32 = 96;

// ==================== Public types ====================

/// What the user has selected on the map.
#[derive(Clone, Debug, PartialEq)]
pub enum EntitySelection {
    /// A single entity's detail.
    Single {
        /// The entity whose detail panel to show.
        detail: EntityId,
        /// The marker to highlight — its co-located group's representative.
        /// Equals `detail` for a plain marker click; differs when a
        /// non-representative member of a disambiguation group is picked,
        /// since map features are keyed on the representative's id.
        feature: EntityId,
        /// The co-located group to return to, when this came from a
        /// disambiguation pick.
        back: Option<Vec<EntityPickerEntry>>,
    },
    /// Multiple co-located entities — show disambiguation list.
    Multiple(Vec<EntityPickerEntry>),
}

/// Client-facing picker entry, at the opaque wire entity id.
pub type EntityPickerEntry = chronoscope_api_client::EntityPickerEntry<EntityId>;

/// A map marker — the map component's uniform view of every entity rendered
/// on the map. No region clustering — every marker is either a single entity
/// or a co-located disambiguation group, optionally carrying a thumbnail.
#[derive(Clone, Debug)]
struct MapMarker {
    /// The representative entity's opaque wire id as a string — the stable key
    /// MapLibre uses for the GeoJSON feature (`promoteId`) and for
    /// selection-state tracking.
    id: String,
    /// Geographic position, carried as a `GeoPoint` so the viewport clip can
    /// test membership without reconstructing (and re-validating) it.
    point: GeoPoint,
    label: Option<String>,
    click_action: api::ClickAction,
    /// The representative's thumbnail URL when the server reported a depicted
    /// image. Its presence drives whether the feature carries a `thumbnail`
    /// icon id (the placeholder→raster load path) and seeds the id→URL registry
    /// the `styleimagemissing` handler resolves against.
    thumbnail_url: Option<String>,
    /// What the sources say about this entity's existence at the slider's
    /// instant. `None` for a cluster pin, which stands for many entities and
    /// carries no single verdict.
    existence: Option<ExistenceState>,
}

/// Signal carrying the current map selection.
#[derive(Clone)]
pub struct SelectedEntity(
    pub ReadSignal<Option<EntitySelection>>,
    pub WriteSignal<Option<EntitySelection>>,
);

/// The map's time control, provided via context: the instant the map is rewound
/// to, and the setter the time slider drives it with.
///
/// The map owns the signal so the slider can live anywhere in the layout, and so
/// a pan and a scrub read one instant rather than two.
#[derive(Clone, Copy)]
pub struct TimeControl {
    pub as_of: ReadSignal<NaiveDate>,
    pub set_as_of: WriteSignal<NaiveDate>,
    /// The day the session opened, sampled once. Carried here so the slider's
    /// upper bound and its "is this the default view" test come from the same
    /// instant the map initialized with — a second `today()` call could land on
    /// the other side of midnight.
    pub now: NaiveDate,
}

/// Map loading/status signals provided via context.
#[derive(Clone)]
pub struct MapStatus {
    /// True while a viewport entity fetch is in flight.
    pub loading: ReadSignal<bool>,
    /// True briefly after a fetch returns zero results.
    pub empty: ReadSignal<bool>,
    /// Error message from the last failed fetch, if any.
    pub fetch_error: ReadSignal<Option<String>>,
    /// Set to true to trigger a retry of the last failed fetch.
    pub retry: WriteSignal<bool>,
}

// ==================== GeoJSON construction ====================

/// The browser's current date — the map's default instant, and what the "now"
/// reset returns to.
///
/// Read through `js_sys` rather than `chrono::Utc::now`, which needs chrono's
/// `wasmbind` feature to work in a browser at all.
pub(crate) fn today() -> NaiveDate {
    let now = js_sys::Date::new_0();
    // A `Date`'s components are always a real calendar day, so the fallback is
    // unreachable; `unwrap_or_default` keeps it total without a panic path.
    NaiveDate::from_ymd_opt(
        now.get_full_year() as i32,
        now.get_month() + 1,
        now.get_date(),
    )
    .unwrap_or_default()
}

/// The style-image id for a marker's thumbnail raster.
///
/// This id is the single point of coordination between three places: the
/// `thumbnail` GeoJSON property set in [`build_markers_geojson`] (which the
/// symbol layer's `icon-image` reads), the `styleimagemissing` lookup key, and
/// the eviction set. They must all derive it the same way, hence one function.
fn thumbnail_image_id(marker_id: &str) -> String {
    format!("thumb-{marker_id}")
}

/// The GeoJSON property value each existence verdict renders under. The layer
/// paint expressions match on these strings, so the mapping lives here alone.
fn existence_property(state: ExistenceState) -> &'static str {
    match state {
        ExistenceState::Uncontested => "uncontested",
        ExistenceState::Contested => "contested",
        ExistenceState::Presumed => "presumed",
        ExistenceState::Unknown => "unknown",
        ExistenceState::Absent => "absent",
    }
}

/// Whether a marker is drawn at the slider's instant. An entity the sources
/// place as gone (or not yet built) is dropped entirely rather than styled away.
///
/// Applied when the in-view set is *assembled*, not when it is serialized, so
/// that one list is the drawn set: the empty-state signal, the thumbnail
/// registry, and the GeoJSON all read the same markers. Filtering at
/// serialization instead left every other consumer counting entities nobody can
/// see.
///
/// A cluster pin has no verdict and always draws.
fn is_drawn(marker: &MapMarker) -> bool {
    marker.existence != Some(ExistenceState::Absent)
}

/// Build a GeoJSON `FeatureCollection` from a list of map markers.
///
/// One marker = one feature. The server handles co-location grouping, so
/// there's no client-side coordinate dedup; each feature's `kind` (entity vs
/// cluster) is derived from its click action. Markers absent at the slider's
/// instant were already dropped when the in-view set was assembled
/// ([`is_drawn`]).
fn build_markers_geojson(markers: &[MapMarker]) -> Option<JsValue> {
    use geojson::{Feature, FeatureCollection, Geometry, Value, feature};

    let features: Vec<Feature> = markers
        .iter()
        .map(|marker| {
            // GeoJSON Point is [lon, lat]
            let geometry =
                Geometry::new(Value::Point(vec![marker.point.lon(), marker.point.lat()]));
            let mut props = serde_json::Map::new();
            props.insert("feature_id".into(), marker.id.clone().into());
            if let Some(ref label) = marker.label {
                props.insert("name".into(), label.clone().into());
            }

            // Derive `kind` from the click action: an Expand cluster the map
            // zooms into versus an entity marker (a lone Select or a
            // Disambiguate group).
            let kind = match &marker.click_action {
                api::ClickAction::Select { entity_id } => {
                    props.insert("id".into(), entity_id.as_str().into());
                    "entity"
                }
                api::ClickAction::Expand { split_level } => {
                    // A lone server cluster: its click zooms to the level where
                    // the cell subdivides, so the refetch breaks it apart.
                    props.insert("split_level".into(), serde_json::Value::from(*split_level));
                    "cluster"
                }
                api::ClickAction::Disambiguate { entries } => {
                    if let Ok(json) = serde_json::to_string(entries) {
                        props.insert("group".into(), json.into());
                    }
                    "entity"
                }
            };
            props.insert("kind".into(), kind.into());
            if let Some(state) = marker.existence {
                props.insert("existence".into(), existence_property(state).into());
            }

            // A thumbnailed marker carries its style-image id upfront, so it
            // lands in the symbol layer from the first `setData`. The id points
            // at an image not yet registered; MapLibre fires `styleimagemissing`,
            // which draws a placeholder then loads the real raster in place.
            if marker.thumbnail_url.is_some() {
                props.insert("thumbnail".into(), thumbnail_image_id(&marker.id).into());
            }

            Feature {
                // Mirror `promoteId: "feature_id"` at the GeoJSON id so
                // selection feature-state and query dedup resolve against one
                // stable id.
                id: Some(feature::Id::String(marker.id.clone())),
                geometry: Some(geometry),
                properties: Some(props),
                ..Feature::default()
            }
        })
        .collect();

    let collection = FeatureCollection {
        features,
        bbox: None,
        foreign_members: None,
    };

    to_js(&collection).ok()
}

// ==================== MapLibre source/layer setup ====================

/// Typed GeoJSON source specification for `map.addSource`.
///
/// `promote_id: "feature_id"` tells MapLibre to use the `feature_id`
/// property as each feature's stable ID. This is required for
/// `setFeatureState` (selection highlighting) and for deduplicating
/// `queryRenderedFeatures` across layers.
#[derive(Serialize)]
struct GeoJsonSourceSpec {
    r#type: &'static str,
    data: serde_json::Value,
    #[serde(rename = "promoteId")]
    promote_id: &'static str,
    /// Turn on MapLibre's built-in supercluster: near cells fold into cluster
    /// features carrying `point_count` (and shedding leaf props).
    cluster: bool,
    /// Cluster merge radius in CSS pixels.
    #[serde(rename = "clusterRadius")]
    cluster_radius: u32,
    /// Zoom past which cells no longer cluster. Held at the map's max zoom so a
    /// badge that survives to the terminal zoom is genuinely coincident points.
    #[serde(rename = "clusterMaxZoom")]
    cluster_max_zoom: u8,
}

/// Initialize empty GeoJSON source and layers on the map.
///
/// Must be called synchronously in the map load callback (before any async
/// work). Use `update_source_data` to populate with real data.
fn init_source_and_layers(map: &maplibre::Map) {
    let source = GeoJsonSourceSpec {
        r#type: "geojson",
        data: json!({"type": "FeatureCollection", "features": []}),
        promote_id: "feature_id",
        cluster: true,
        cluster_radius: CLUSTER_RADIUS,
        cluster_max_zoom: MAX_ZOOM,
    };
    let Ok(source_js) = to_js(&source) else {
        web_sys::console::error_1(&"Failed to serialize GeoJSON source spec".into());
        return;
    };
    if let Err(e) = map.add_source(ENTITY_SOURCE_ID, &source_js) {
        web_sys::console::error_1(&format!("Failed to add GeoJSON source: {e:?}").into());
        return;
    }

    // Coalesce expressions: MapLibre evaluates paint expressions even
    // during source initialization when properties/feature-state may be
    // null. Wrap in ["coalesce", ..., default] to avoid type errors.
    let get_selected = json!(["coalesce", ["feature-state", "selected"], false]);

    // A bare pin excludes clustered features: a MapLibre proximity cluster
    // (`point_count`) and a lone server `Expand` cell (`kind == "cluster"`)
    // both belong to the badge layer, not here.
    let not_cluster = json!([
        "all",
        ["!", ["has", "point_count"]],
        ["!=", ["get", "kind"], "cluster"]
    ]);

    // How the drawn existence states read (absent markers are dropped upstream,
    // in `build_markers_geojson`). A cluster carries no verdict and falls through
    // to the default arm.
    //
    // `presumed` deliberately renders exactly like `uncontested`. We never hold
    // positive evidence that something stood at a given instant — only events we
    // infer it from — so presumption is the ordinary way a standing building
    // reads, not a degraded one. A palace sighted once in 1343 and never since is
    // presumed every day after, and fading that would fade most of the map.
    //
    // What the render does distinguish is having *no* evidence (`unknown`, washed
    // out) and sources *disagreeing* (`contested`, ringed).
    // Coalesced like the other property reads: a marker whose representative
    // failed to project carries no verdict, and a bare `get` would feed `match` a
    // null. No evidence is exactly `unknown`, so that is the honest default.
    let existence = json!(["coalesce", ["get", "existence"], "unknown"]);
    let fill_opacity = json!(["match", existence.clone(), "unknown", 0.25, 0.85]);
    let fill_color = json!(["match", existence.clone(), "unknown", "#9A9A9A", "#8B5E3C"]);
    // A contested marker gets a heavier ring in a colour nothing else uses, so a
    // source disagreement is visible without opening the entity.
    let stroke_color = json!([
        "case",
        get_selected,
        "#FFFFFF",
        ["==", existence.clone(), "contested"],
        "#C2410C",
        ["==", existence.clone(), "unknown"],
        "#B8B8B8",
        "#F5F0E8"
    ]);
    let stroke_width = json!([
        "case",
        get_selected,
        4,
        ["==", existence.clone(), "contested"],
        3,
        2
    ]);

    // Circle layer for the bare marker dots. A thumbnailed marker carries a
    // `thumbnail` id from t=0 and so renders in the symbol layer instead —
    // this filter excludes it, and the symbol layer's `styleimagemissing`
    // path shows a placeholder pin until the raster loads.
    let circle = json!({
        "id": ENTITY_CIRCLES_LAYER,
        "type": "circle",
        "source": ENTITY_SOURCE_ID,
        "filter": ["all", ["!", ["has", "thumbnail"]], not_cluster.clone()],
        "paint": {
            "circle-radius": 10,
            "circle-color": fill_color,
            "circle-stroke-width": stroke_width,
            "circle-stroke-color": stroke_color,
            "circle-opacity": fill_opacity
        }
    });
    let Ok(circle_js) = to_js(&circle) else {
        web_sys::console::error_1(&"Failed to serialize circle layer spec".into());
        return;
    };
    if let Err(e) = map.add_layer(&circle_js) {
        web_sys::console::error_1(&format!("Failed to add circle layer: {e:?}").into());
        return;
    }

    // Badge layer for clusters — a larger, darker disc, visually distinct from
    // a bare pin. It matches a MapLibre proximity cluster (`point_count`) or a
    // lone server `Expand` cell (`kind == "cluster"`). No count text (the
    // undercount of an approximate rollup stays invisible by design).
    let badge = json!({
        "id": ENTITY_BADGE_LAYER,
        "type": "circle",
        "source": ENTITY_SOURCE_ID,
        "filter": ["any", ["has", "point_count"], ["==", ["get", "kind"], "cluster"]],
        "paint": {
            "circle-radius": 18,
            "circle-color": "#6B4A2F",
            "circle-stroke-width": 3,
            "circle-stroke-color": "#F5F0E8",
            "circle-opacity": 0.9
        }
    });
    let Ok(badge_js) = to_js(&badge) else {
        web_sys::console::error_1(&"Failed to serialize badge layer spec".into());
        return;
    };
    if let Err(e) = map.add_layer(&badge_js) {
        web_sys::console::error_1(&format!("Failed to add badge layer: {e:?}").into());
        return;
    }

    // Symbol layer for marker labels — added BEFORE thumbnails so thumbnails
    // render on top and aren't occluded by neighboring labels.
    // Read label styling from the basemap's city label layer so our markers
    // match the basemap visually, regardless of which style is loaded.
    let basemap = "label_city";
    let labels = json!({
        "id": "entity-labels",
        "type": "symbol",
        "source": ENTITY_SOURCE_ID,
        "filter": ["all", ["has", "name"], not_cluster.clone()],
        "layout": {
            "text-field": ["get", "name"],
            "text-size": 12,
            "text-anchor": "top",
            "text-offset": [0, 0.8],
            "text-allow-overlap": false,
            "text-ignore-placement": false,
            "text-max-width": 8
        }
    });
    let Ok(labels_js) = to_js(&labels) else {
        web_sys::console::error_1(&"Failed to serialize labels layer spec".into());
        return;
    };
    // Copy font and paint properties from the basemap label layer.
    let Ok(layout) = js_sys::Reflect::get(&labels_js, &"layout".into()) else {
        web_sys::console::error_1(&"Failed to read layout from labels spec".into());
        return;
    };
    let font = map.get_layout_property(basemap, "text-font");
    if !font.is_undefined() {
        let _ = js_sys::Reflect::set(&layout, &"text-font".into(), &font);
    }
    let paint = js_sys::Object::new();
    for prop in &[
        "text-color",
        "text-halo-color",
        "text-halo-width",
        "text-halo-blur",
    ] {
        let val = map.get_paint_property(basemap, prop);
        if !val.is_undefined() {
            let _ = js_sys::Reflect::set(&paint, &(*prop).into(), &val);
        }
    }
    let _ = js_sys::Reflect::set(&labels_js, &"paint".into(), &paint);
    if let Err(e) = map.add_layer(&labels_js) {
        web_sys::console::error_1(&format!("Failed to add labels layer: {e:?}").into());
    }

    // Symbol layer for thumbnail images — added LAST so thumbnails render on
    // top of all other marker layers (circles and labels).
    let thumbnails = json!({
        "id": ENTITY_THUMBNAILS_LAYER,
        "type": "symbol",
        "source": ENTITY_SOURCE_ID,
        "filter": ["all", ["has", "thumbnail"], not_cluster],
        "layout": {
            "icon-image": ["get", "thumbnail"],
            "icon-size": 1.0,
            "icon-allow-overlap": true,
            "icon-ignore-placement": true,
            "icon-anchor": "bottom"
        },
        "paint": {
            // A thumbnailed marker fades on the same scale as a bare dot, so a
            // photographed entity and an unphotographed one read the same
            // confidence at a given instant.
            "icon-opacity": [
                "match", ["coalesce", ["get", "existence"], "unknown"],
                "unknown", 0.35,
                0.95
            ]
        }
    });
    let Ok(thumbnails_js) = to_js(&thumbnails) else {
        web_sys::console::error_1(&"Failed to serialize thumbnails layer spec".into());
        return;
    };
    if let Err(e) = map.add_layer(&thumbnails_js) {
        web_sys::console::error_1(&format!("Failed to add thumbnails layer: {e:?}").into());
    }
}

/// Update an existing GeoJSON source with new data.
fn update_source_data(map: &maplibre::Map, source_id: &str, geojson: &JsValue) {
    match map.get_source(source_id) {
        Some(source) => source.set_data(geojson),
        None => {
            web_sys::console::warn_1(
                &format!("GeoJSON source '{source_id}' not found on map — was init_source_and_layers called?").into(),
            );
        }
    }
}

/// Clear a GeoJSON source by setting it to an empty feature collection.
fn clear_source_data(map: &maplibre::Map, source_id: &str) {
    let empty = serde_json::json!({
        "type": "FeatureCollection",
        "features": []
    });
    if let Ok(js) = to_js(&empty) {
        update_source_data(map, source_id, &js);
    }
}

// ==================== Entity fetching ====================

/// Track whether the GeoJSON source has been added to the map.
///
/// This is a Leptos signal so that effects (`effect_rebuild_geojson`,
/// `effect_selection`) track it reactively rather than reading an untracked
/// `Rc<Cell<bool>>` that wouldn't trigger re-runs.
type SourceInitialized = RwSignal<bool>;

/// Signals needed by viewport entity fetching and GeoJSON rebuilds.
///
/// All fields are `Copy` (Leptos signals), so this struct is `Copy` too —
/// it can be captured by closures without cloning.
#[derive(Clone, Copy)]
struct ViewportSignals {
    set_loading: WriteSignal<bool>,
    set_empty: WriteSignal<bool>,
    set_fetch_error: WriteSignal<Option<String>>,
    set_map_error: WriteSignal<Option<String>>,
    selected: ReadSignal<Option<EntitySelection>>,
    /// All markers currently rendered on the map.
    cached_markers: ReadSignal<Vec<MapMarker>>,
    set_cached_markers: WriteSignal<Vec<MapMarker>>,
}

/// Cache key for a fetched container tile. Snapshot-first so a snapshot change
/// can never serve a stale cell: keying on `(snapshot, level, x, y)` isolates
/// each read-consistency point's tiles.
///
/// `as_of` is part of the key because the server reports each marker's existence
/// at that instant — the same tile at a different slider position is a different
/// answer, not a cache hit.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct TileKey {
    snapshot: Snapshot,
    as_of: NaiveDate,
    level: u8,
    x: u32,
    y: u32,
}

/// The container tiles a `viewport` covers at `level`, widened by a `margin`
/// ring on every side.
///
/// The seam-aware covering enumeration is core's
/// [`viewport_tile_xys`](chronoscope_core::geo::viewport_tile_xys) — it
/// interprets the antimeridian wrap once, so the client no longer re-derives it.
/// On top the client adds only its own concern: the margin ring, whose x wraps
/// modulo `n = 2^level` at the seam and whose y clamps to `[0, n)`. Mercator y
/// grows southward, so `max_lat` is the smaller (northern) row — handled inside
/// core's projection.
fn tiles_for_bounds(viewport: &Viewport, level: QuadLevel, margin: u32) -> Vec<(u32, u32)> {
    use chronoscope_core::geo::viewport_tile_xys;

    // One `QuadLevel` drives both the covering indices and the wrap modulus, so
    // the margin ring wraps `x` modulo the very count the tiles were enumerated
    // against — the two can't desync as the pyramid depth changes.
    let n = i64::from(1u32 << u32::from(level.get()));
    let m = i64::from(margin);

    let mut tiles = BTreeSet::new();
    for (x, y) in viewport_tile_xys(viewport, level) {
        for dx in -m..=m {
            let wx = (i64::from(x) + dx).rem_euclid(n) as u32;
            for dy in -m..=m {
                let ny = i64::from(y) + dy;
                if (0..n).contains(&ny) {
                    tiles.insert((wx, ny as u32));
                }
            }
        }
    }
    tiles.into_iter().collect()
}

/// Convert a wire [`Marker`](api::Marker) into the map's own [`MapMarker`]. The
/// server negotiated the display name from the request's `Accept-Language`, so
/// the label is used as-is.
fn marker_from_wire(m: api::Marker) -> MapMarker {
    MapMarker {
        id: m.id.as_str().to_string(),
        point: m.point,
        label: m.name,
        click_action: m.click_action,
        thumbnail_url: m.thumbnail_url.map(|url| url.as_str().to_string()),
        existence: m.existence,
    }
}

/// Fetch the container tiles overlapping the viewport (plus a margin ring) at a
/// zoom-tracking level, clip their cells to the visible box, and hand the
/// survivors to the `cluster: true` source for on-screen decluttering.
///
/// The clip is the correctness core: supercluster only ever sees in-view cells,
/// so every badge centroid lands in-view and no off-screen cell stands in for
/// an empty region (design invariant B — no hidden entity).
///
/// A monotonic pass generation is the last-write-wins guard: a pass that finds
/// its generation superseded while awaiting fetches drops its result rather
/// than overwrite a newer assembly.
async fn refresh_tiles_for_viewport(
    map: &maplibre::Map,
    client: &api::Client,
    signals: ViewportSignals,
    state: &MapState,
) {
    // The component may have unmounted at the `get_or_init_client` await that
    // precedes this pass, outside its abort. `on_cleanup` then ran, removing the
    // map and disposing the signals. Bail before any map read or signal write;
    // nothing awaits a settled bump once the component is gone.
    if state.disposed.get() {
        return;
    }

    // Seam-aware display bounds. When the whole globe is on screen the wrap
    // folds the box to a narrow antimeridian sliver, so substitute a full,
    // non-wrapping world box — used identically for enumeration and the clip.
    let (west, south, east, north) = maplibre::get_viewport_bounds(map);
    let (raw_west, _, raw_east, _) = maplibre::get_raw_viewport_bounds(map);
    let (min_lat, max_lat, min_lon, max_lon) = if raw_east - raw_west >= 360.0 {
        (-WORLD_VIEW_LAT, WORLD_VIEW_LAT, -180.0, 180.0)
    } else {
        (south, north, west, east)
    };

    // Compute the pass fully before touching the generation or the abort handle:
    // a bail-out here (invalid viewport, over-cap) must not supersede a valid
    // pass already in flight, which would leave the map staler than before while
    // rendering nothing itself. The bail paths still set their status and bump
    // the settled counter — they just don't disturb the last-write-wins guard.
    let viewport = match Viewport::from_coords(min_lat, max_lat, min_lon, max_lon) {
        Ok(v) => v,
        Err(e) => {
            signals.set_loading.set(false);
            signals
                .set_fetch_error
                .set(Some(format!("Invalid viewport bounds: {e}")));
            #[cfg(feature = "test-hooks")]
            record_fetch_settled();
            return;
        }
    };

    // Level tracks the map zoom, clamped into `[1, MAX_ZOOM]`: never level 0
    // (the server accepts it, but a whole-world request is pointless here) and
    // never past the terminal cluster level. One `QuadLevel` is the single
    // source of the level, so the tile modulus, the covering indices, and the
    // request `z` can't drift apart even if `MAX_ZOOM` outgrows the pyramid.
    let level =
        QuadLevel::saturating(map.get_zoom_raw().floor().clamp(1.0, f64::from(MAX_ZOOM)) as u8);
    let level_z = level.get();
    let tiles = tiles_for_bounds(&viewport, level, TILE_MARGIN);

    // One read for the whole pass: the slider can move mid-fetch, and a pass
    // that probed one instant must not cache or render under another.
    let as_of = state.as_of.get();

    // Guardrail: never fan out a request storm. The prior render (and cache)
    // stay, so the view degrades to slightly-stale rather than blank.
    if tiles.len() > MAX_TILES_PER_PASS {
        web_sys::console::warn_1(
            &format!(
                "tile pass at level {level_z} enumerated {} tiles (cap {MAX_TILES_PER_PASS}); skipping fetch",
                tiles.len()
            )
            .into(),
        );
        signals.set_loading.set(false);
        #[cfg(feature = "test-hooks")]
        record_fetch_settled();
        return;
    }

    // The pass has committed to fetching. Bump the generation as the
    // last-write-wins guard, then install this pass's abort handle and abort the
    // prior one, so a superseded pass's in-flight fetches are dropped — dropping
    // a wasm `fetch` future aborts the browser request rather than running it to
    // completion only to discard the result.
    let my_gen = state.pass_generation.get().wrapping_add(1);
    state.pass_generation.set(my_gen);
    let (abort_handle, abort_reg) = AbortHandle::new_pair();
    if let Some(prev) = state.pass_abort.borrow_mut().replace(abort_handle) {
        prev.abort();
    }

    signals.set_loading.set(true);
    signals.set_fetch_error.set(None);
    signals.set_map_error.set(None);

    // A session pins to the first snapshot it sees, threading it through every
    // later fetch and the cache key so one session stays read-coherent.
    let pass_snapshot = state.pinned_snapshot.borrow().clone();

    // Tiles not already cached under the pinned snapshot. The first pass has no
    // pinned snapshot yet, so nothing is cached — fetch them all. Every tile in
    // the pass shares one snapshot, so probe with a single reused key rather than
    // cloning the snapshot string per tile.
    let missing: Vec<(u32, u32)> = match &pass_snapshot {
        Some(snapshot) => {
            let cache = state.tile_cache.borrow();
            let mut probe = TileKey {
                snapshot: snapshot.clone(),
                as_of,
                level: level_z,
                x: 0,
                y: 0,
            };
            tiles
                .iter()
                .copied()
                .filter(|&(x, y)| {
                    probe.x = x;
                    probe.y = y;
                    !cache.contains_key(&probe)
                })
                .collect()
        }
        None => tiles.clone(),
    };

    // Every fetch shares the pinned snapshot, so borrow it rather than clone the
    // string per tile.
    let client_ref = client;
    let snapshot_ref = pass_snapshot.as_ref();
    let fetches = missing.into_iter().map(move |(x, y)| async move {
        let result = client_ref
            .fetch_tile(level_z, x, y, snapshot_ref, Some(as_of))
            .await;
        (x, y, result)
    });
    // Cancellation: a newer pass aborts this future, dropping the in-flight
    // fetches. An aborted pass returns without rendering or bumping the settle
    // counter — the same outcome as the generation guard below.
    let results = match Abortable::new(join_all(fetches), abort_reg).await {
        Ok(results) => results,
        Err(_aborted) => return,
    };

    // A newer pass superseded this one mid-fetch without aborting it (its fetches
    // finished first): leave every shared cell — the cache, the render, the
    // status signals — for that newer pass to own.
    if state.pass_generation.get() != my_gen {
        return;
    }

    let mut fetch_error: Option<String> = None;
    {
        let mut cache = state.tile_cache.borrow_mut();
        for (x, y, result) in results {
            match result {
                Ok(response) => {
                    if state.pinned_snapshot.borrow().is_none() {
                        *state.pinned_snapshot.borrow_mut() = Some(response.snapshot.clone());
                    }
                    let cells = response.markers.into_iter().map(marker_from_wire).collect();
                    cache.insert(
                        TileKey {
                            snapshot: response.snapshot,
                            as_of,
                            level: level_z,
                            x,
                            y,
                        },
                        cells,
                    );
                }
                Err(e) => {
                    if fetch_error.is_none() {
                        fetch_error = Some(format!("{e}"));
                    }
                }
            }
        }
    }

    // Assemble every enumerated tile's cells from the cache under the pinned
    // snapshot, dropping any whose point falls outside the seam-aware viewport.
    // Read the pinned snapshot once and reuse it for the retain sweep below —
    // nothing mutates it in between. The tiles share one snapshot, so probe with
    // a single reused key rather than cloning it per tile.
    let effective_snapshot = state.pinned_snapshot.borrow().clone();
    let mut in_view: Vec<MapMarker> = Vec::new();
    if let Some(snapshot) = &effective_snapshot {
        let cache = state.tile_cache.borrow();
        let mut probe = TileKey {
            snapshot: snapshot.clone(),
            as_of,
            level: level_z,
            x: 0,
            y: 0,
        };
        for &(x, y) in &tiles {
            probe.x = x;
            probe.y = y;
            if let Some(cells) = cache.get(&probe) {
                in_view.extend(
                    cells
                        .iter()
                        .filter(|c| viewport.contains(&c.point) && is_drawn(c))
                        .cloned(),
                );
            }
        }
    }

    // A pan whose newly-uncovered tiles all failed to fetch assembles an empty
    // in-view set. Rendering it would blank the map under the error banner and
    // the retain sweep below would evict the prior area's still-good cache. Keep
    // the last-good render and cache instead, surfacing only the error. Gated on
    // the error so a genuinely empty area (no error) still renders empty below.
    // Only sound while the instant is unchanged: keeping the prior render then
    // means "slightly old viewport". After a scrub it would mean markers from
    // another year sitting under a slider that reads the new one, so a failed
    // scrub clears instead.
    if fetch_error.is_some() && in_view.is_empty() {
        let same_instant = state.rendered_as_of.get() == Some(as_of);
        signals.set_loading.set(false);
        signals.set_empty.set(false);
        signals.set_fetch_error.set(fetch_error);
        if !same_instant {
            state.rendered_as_of.set(Some(as_of));
            signals.set_cached_markers.set(Vec::new());
        }
        #[cfg(feature = "test-hooks")]
        record_fetch_settled();
        return;
    }

    // Bound the cache to the window this pass assembled: keep only cells at the
    // pinned snapshot, current level, and current instant whose tile the
    // viewport+margin enumeration covered. A pan, zoom, snapshot change, or
    // slider move keys past everything else, so without this sweep the cache
    // grows unbounded as the map moves — and `as_of` belongs in the predicate for
    // the same reason as the rest: a scrub across a century would otherwise
    // retain a full tile set per year it passed through.
    if let Some(snapshot) = &effective_snapshot {
        let window: BTreeSet<(u32, u32)> = tiles.iter().copied().collect();
        state.tile_cache.borrow_mut().retain(|key, _| {
            key.snapshot == *snapshot
                && key.as_of == as_of
                && key.level == level_z
                && window.contains(&(key.x, key.y))
        });
    }

    signals.set_loading.set(false);
    signals.set_fetch_error.set(fetch_error.clone());
    signals
        .set_empty
        .set(fetch_error.is_none() && in_view.is_empty());

    // Desired thumbnail set for this assembly, keyed by style-image id; the
    // `styleimagemissing` handler resolves a missing id here and eviction diffs
    // the keys. Built before `in_view` is moved into the signal.
    let new_urls: BTreeMap<String, String> = in_view
        .iter()
        .filter_map(|m| {
            m.thumbnail_url
                .as_ref()
                .map(|url| (thumbnail_image_id(&m.id), url.clone()))
        })
        .collect();

    // No thumbnails to load: signal readiness now so test hooks don't wait on
    // an event that will never fire.
    #[cfg(feature = "test-hooks")]
    if new_urls.is_empty() {
        record_thumbnails_loaded();
    }

    // Bound the registered-image pool: diff the prior desired set against this
    // one and removeImage the ids that fell out. An evicted id can't re-fire
    // `styleimagemissing` — its feature leaves the source in the setData below,
    // so the handler's URL lookup no longer finds it.
    {
        let mut cur = state.thumbnail_urls.borrow_mut();
        let stale: Vec<(String, String)> = cur
            .iter()
            .filter(|(id, _)| !new_urls.contains_key(*id))
            .map(|(id, url)| (id.clone(), url.clone()))
            .collect();
        let mut next = new_urls;
        for (image_name, url) in stale {
            if map.has_image(&image_name)
                && let Err(e) = map.remove_image(&image_name)
            {
                web_sys::console::warn_1(
                    &format!("remove_image failed for {image_name}: {e:?}").into(),
                );
                // Keep an image we failed to remove tracked so a later pass
                // retries the removal.
                next.insert(image_name, url);
            }
        }
        *cur = next;
    }

    state.rendered_as_of.set(Some(as_of));
    signals.set_cached_markers.set(in_view);

    #[cfg(feature = "test-hooks")]
    record_fetch_settled();
}

// ==================== Thumbnail registration ====================
//
// Thumbnails use MapLibre's `addImage` + symbol layer, which is the intended
// way to do custom icons — once registered, they're rendered by MapLibre's
// WebGL pipeline alongside the circle and label layers, all driven by the
// same GeoJSON source and filtered by properties.
//
// The canvas drawing below prepares the image data (circular photo + stem +
// location dot) as a single raster icon. This is analogous to a sprite sheet
// entry. An alternative would be to register just the circular photo, use
// `icon-offset` to shift it up, and let the existing circle layer render the
// location dot underneath. That would reduce canvas drawing and lean more on
// MapLibre's compositing, but requires coordinating multiple layers and
// doesn't give us the connecting stem.
//
// TODO: Consider minimizing canvas drawing by splitting the pin into
// composited MapLibre layers (offset icon + circle + line) instead of
// baking the full pin shape into one raster image. This would improve
// zoom scaling behavior and reduce per-image canvas work.

/// Radius of the small location dot at the bottom of a thumbnail pin.
/// Matches the circle layer's base radius (7px at count=1) for visual continuity.
const DOT_RADIUS: f64 = 7.0;
/// Vertical gap between the thumbnail circle and the location dot.
const STEM_LENGTH: f64 = 8.0;

/// Get the device pixel ratio, defaulting to 1.0 if unavailable.
fn device_pixel_ratio() -> f64 {
    web_sys::window()
        .map(|w| w.device_pixel_ratio())
        .unwrap_or(1.0)
}

/// Device-pixel geometry of a thumbnail pin at a given DPR.
///
/// `draw_circular_thumbnail` and `draw_thumbnail_placeholder` both derive their
/// canvas from this, so a placeholder and the raster that later replaces it via
/// `map.updateImage` are byte-for-byte the same size — `updateImage` throws on a
/// dimension mismatch, and this single source of the dimensions is what keeps
/// the two from drifting apart.
struct ThumbnailGeometry {
    /// Canvas width in device pixels.
    canvas_w: u32,
    /// Canvas height in device pixels — taller than wide to fit the stem + dot.
    canvas_h: u32,
    /// Thumbnail circle radius.
    thumb_r: f64,
    /// Border stroke width, shared by the circle and dot outlines.
    border: f64,
    /// Horizontal center of every element.
    cx: f64,
    /// Vertical center of the thumbnail circle.
    thumb_cy: f64,
    /// Gap between the circle bottom and the location dot.
    stem_len: f64,
    /// Location dot radius.
    dot_r: f64,
}

fn thumbnail_geometry(dpr: f64) -> ThumbnailGeometry {
    let thumb_r = f64::from(THUMBNAIL_SIZE) / 2.0 * dpr;
    let border = 2.0 * dpr;
    let stem_len = STEM_LENGTH * dpr;
    let dot_r = DOT_RADIUS * dpr;
    // Use ceil() so non-integer device pixel ratios (1.5/1.75/2.625 on some
    // Windows and Android devices) don't clip the bottom row of the drop shadow.
    let canvas_w = (f64::from(THUMBNAIL_SIZE) * dpr).ceil() as u32;
    // Extra space below the dot for the drop shadow (blur + offset can extend
    // further than `dot_r` alone, especially on Retina displays).
    let shadow_blur = 4.0 * dpr;
    let shadow_offset_y = 2.0 * dpr;
    let bottom_pad = dot_r.max(shadow_blur + shadow_offset_y);
    let canvas_h =
        (f64::from(THUMBNAIL_SIZE) * dpr + stem_len + dot_r * 2.0 + bottom_pad).ceil() as u32;
    ThumbnailGeometry {
        canvas_w,
        canvas_h,
        thumb_r,
        border,
        cx: f64::from(canvas_w) / 2.0,
        thumb_cy: thumb_r,
        stem_len,
        dot_r,
    }
}

/// Create an offscreen canvas of the given geometry and return its 2D context.
///
/// The `<canvas>` DOM node is kept alive by the returned context, so callers
/// only need the context to draw and read back pixels.
fn thumbnail_canvas(g: &ThumbnailGeometry) -> Result<web_sys::CanvasRenderingContext2d, String> {
    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or("no document")?;
    let canvas = document
        .create_element("canvas")
        .map_err(|e| format!("create_element failed: {e:?}"))?
        .dyn_into::<web_sys::HtmlCanvasElement>()
        .map_err(|_| "cast to HtmlCanvasElement failed")?;
    canvas.set_width(g.canvas_w);
    canvas.set_height(g.canvas_h);
    canvas
        .get_context("2d")
        .map_err(|e| format!("getContext failed: {e:?}"))?
        .ok_or("getContext returned None")?
        .dyn_into::<web_sys::CanvasRenderingContext2d>()
        .map_err(|_| "cast to CanvasRenderingContext2d failed".to_string())
}

/// Draw the copper "pin" placeholder shown while a thumbnail loads.
///
/// Sized via [`thumbnail_geometry`] so `map.updateImage` can swap in the real
/// raster in place without a dimension-mismatch throw. Drawn as the pin's stem
/// and location dot with a solid copper disc where the photo will land, so the
/// swap to the loaded raster doesn't shift the pin.
fn draw_thumbnail_placeholder(dpr: f64) -> Result<web_sys::ImageData, String> {
    let g = thumbnail_geometry(dpr);
    let ctx = thumbnail_canvas(&g)?;

    // Copper disc where the photo will land, with the parchment border.
    ctx.set_fill_style_str("#8B5E3C");
    ctx.begin_path();
    ctx.arc(
        g.cx,
        g.thumb_cy,
        g.thumb_r - g.border,
        0.0,
        std::f64::consts::TAU,
    )
    .map_err(|e| format!("arc failed: {e:?}"))?;
    ctx.fill();
    ctx.set_stroke_style_str("#F5F0E8");
    ctx.set_line_width(g.border);
    ctx.begin_path();
    ctx.arc(
        g.cx,
        g.thumb_cy,
        g.thumb_r - g.border / 2.0,
        0.0,
        std::f64::consts::TAU,
    )
    .map_err(|e| format!("arc failed: {e:?}"))?;
    ctx.stroke();

    // Stem line down to the location dot.
    let stem_top = g.thumb_cy + g.thumb_r;
    let stem_bottom = stem_top + g.stem_len;
    ctx.set_stroke_style_str("#8B5E3C");
    ctx.set_line_width(g.border);
    ctx.begin_path();
    ctx.move_to(g.cx, stem_top);
    ctx.line_to(g.cx, stem_bottom);
    ctx.stroke();

    // Location dot marking the geographic point.
    let dot_cy = stem_bottom + g.dot_r;
    ctx.set_fill_style_str("#8B5E3C");
    ctx.begin_path();
    ctx.arc(g.cx, dot_cy, g.dot_r, 0.0, std::f64::consts::TAU)
        .map_err(|e| format!("arc failed: {e:?}"))?;
    ctx.fill();
    ctx.set_stroke_style_str("#F5F0E8");
    ctx.set_line_width(g.border);
    ctx.stroke();

    ctx.get_image_data(0.0, 0.0, f64::from(g.canvas_w), f64::from(g.canvas_h))
        .map_err(|e| format!("getImageData failed: {e:?}"))
}

/// Draw a thumbnail "pin": a circular photo on top, a short stem, and a small
/// dot at the bottom marking the actual geographic location.
///
/// The canvas is sized so the dot sits at the bottom center — use
/// `icon-anchor: "bottom"` in MapLibre so the dot aligns with the coordinate.
fn draw_circular_thumbnail(
    img: &web_sys::HtmlImageElement,
    dpr: f64,
) -> Result<web_sys::ImageData, String> {
    let g = thumbnail_geometry(dpr);
    let ctx = thumbnail_canvas(&g)?;

    let ThumbnailGeometry {
        canvas_w,
        canvas_h,
        thumb_r,
        border,
        cx,
        thumb_cy,
        stem_len,
        dot_r,
    } = g;

    // --- Thumbnail circle (center-cropped for non-square sources) ---
    ctx.save();
    ctx.begin_path();
    ctx.arc(cx, thumb_cy, thumb_r - border, 0.0, std::f64::consts::TAU)
        .map_err(|e| format!("arc failed: {e:?}"))?;
    ctx.clip();

    let nw = f64::from(img.natural_width());
    let nh = f64::from(img.natural_height());
    let side = nw.min(nh);
    let sx = (nw - side) / 2.0;
    let sy = (nh - side) / 2.0;
    ctx.draw_image_with_html_image_element_and_sw_and_sh_and_dx_and_dy_and_dw_and_dh(
        img,
        sx,
        sy,
        side,
        side, // source: center square crop
        0.0,
        0.0,
        thumb_r * 2.0,
        thumb_r * 2.0, // dest: fill circle
    )
    .map_err(|e| format!("drawImage failed: {e:?}"))?;
    ctx.restore();

    // Thumbnail border
    ctx.set_stroke_style_str("#F5F0E8");
    ctx.set_line_width(border);
    ctx.begin_path();
    ctx.arc(
        cx,
        thumb_cy,
        thumb_r - border / 2.0,
        0.0,
        std::f64::consts::TAU,
    )
    .map_err(|e| format!("arc failed: {e:?}"))?;
    ctx.stroke();

    // --- Stem line ---
    let stem_top = thumb_cy + thumb_r;
    let stem_bottom = stem_top + stem_len;
    ctx.set_stroke_style_str("#8B5E3C"); // copper
    ctx.set_line_width(2.0 * dpr);
    ctx.begin_path();
    ctx.move_to(cx, stem_top);
    ctx.line_to(cx, stem_bottom);
    ctx.stroke();

    // --- Drop shadow (soft circle behind the dot, offset down) ---
    let dot_cy = stem_bottom + dot_r;
    ctx.save();
    ctx.set_shadow_color("rgba(0, 0, 0, 0.4)");
    ctx.set_shadow_blur(4.0 * dpr);
    ctx.set_shadow_offset_y(2.0 * dpr);

    // --- Location dot (matches circle marker style) ---
    ctx.set_fill_style_str("#8B5E3C"); // copper fill
    ctx.begin_path();
    ctx.arc(cx, dot_cy, dot_r, 0.0, std::f64::consts::TAU)
        .map_err(|e| format!("arc failed: {e:?}"))?;
    ctx.fill();
    // Restore before stroke so the shadow only applies to the fill, not the border
    ctx.restore();
    ctx.set_stroke_style_str("#F5F0E8"); // parchment stroke
    ctx.set_line_width(2.0 * dpr);
    ctx.stroke();

    ctx.get_image_data(0.0, 0.0, f64::from(canvas_w), f64::from(canvas_h))
        .map_err(|e| format!("getImageData failed: {e:?}"))
}

/// Load an image from a URL, returning the `HtmlImageElement` when loaded.
///
/// `crossOrigin = "anonymous"` is required so the resulting image can be
/// drawn to a canvas and read back via `getImageData` without tainting it.
/// The thumbnail server must respond with `Access-Control-Allow-Origin`
/// — without it, every image silently fails to load.
async fn load_image(url: &str) -> Result<web_sys::HtmlImageElement, String> {
    let img = web_sys::HtmlImageElement::new()
        .map_err(|e| format!("HtmlImageElement::new failed: {e:?}"))?;
    img.set_cross_origin(Some("anonymous"));

    let img_for_handlers = img.clone();
    let promise = js_sys::Promise::new(&mut move |resolve, reject| {
        // TODO: on abort, the <img> may outlive the future, leaking
        // these closures until the orphaned element is GC'd. Bounded leak.
        let resolve_cb = Closure::once_into_js(move || {
            let _ = resolve.call0(&JsValue::NULL);
        });
        let reject_cb = Closure::once_into_js(move || {
            let _ = reject.call0(&JsValue::NULL);
        });
        img_for_handlers.set_onload(Some(resolve_cb.unchecked_ref()));
        img_for_handlers.set_onerror(Some(reject_cb.unchecked_ref()));
    });

    img.set_src(url);

    wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map_err(|_| format!("image load failed for {url}"))?;
    Ok(img)
}

// ==================== Click handler helpers ====================

/// What a click on a marker or badge resolves to. Parsed from the flat GeoJSON
/// feature properties (set in `build_markers_geojson`, plus the `cluster_id` /
/// `point_count` MapLibre injects on a proximity cluster) into a proper
/// discriminated type.
enum ClickTarget {
    /// Select a single entity.
    Select(EntityId),
    /// Disambiguate co-located entities.
    Disambiguate(Vec<EntityPickerEntry>),
    /// Expand a cluster badge. A MapLibre proximity cluster carries a
    /// `cluster_id` (resolved through the source's supercluster index); a lone
    /// server `Expand` cell carries a `split_level` instead. `point` is the
    /// badge's `[lng, lat]`, the ease-to center.
    Expand {
        cluster_id: Option<u64>,
        point: [f64; 2],
        split_level: Option<u8>,
    },
}

/// Raw deserialization target for GeoJSON feature properties.
/// Immediately converted to a [`ClickTarget`] — never used directly.
/// `id` deserializes from the string `id` property written in
/// `build_markers_geojson` (`EntityId` is a transparent string newtype);
/// `cluster_id`/`point_count` are the fields MapLibre injects on a proximity
/// cluster (which drops the leaf props). No `deny_unknown_fields`, so the extra
/// MapLibre cluster fields (`cluster`, `point_count_abbreviated`) are ignored.
#[derive(serde::Deserialize)]
struct RawMarkerProps {
    id: Option<EntityId>,
    name: Option<String>,
    group: Option<String>,
    /// Present only on a MapLibre proximity cluster. `u64` holds supercluster's
    /// ids (they exceed `u32`); the FFI call converts it to a JS number.
    cluster_id: Option<u64>,
    /// Present only on a MapLibre proximity cluster; its presence marks the
    /// feature as one — the count itself is unused.
    point_count: Option<u32>,
    /// `"cluster"` on a lone server `Expand` cell (proximity clusters shed it).
    kind: Option<String>,
    /// The server-computed split level on a lone `Expand` cell.
    split_level: Option<u8>,
}

impl RawMarkerProps {
    fn into_click_target(self) -> Option<ClickTarget> {
        match self {
            Self {
                group: Some(json), ..
            } => serde_json::from_str::<Vec<EntityPickerEntry>>(&json)
                .ok()
                .map(ClickTarget::Disambiguate),
            Self { id: Some(id), .. } => Some(ClickTarget::Select(id)),
            _ => None,
        }
    }

    /// Resolve a badge click into a [`ClickTarget::Expand`]. `point` is the
    /// clicked feature's geometry, threaded in because the properties don't
    /// carry it.
    fn into_expand_target(self, point: [f64; 2]) -> Option<ClickTarget> {
        let Self {
            cluster_id,
            point_count,
            kind,
            split_level,
            ..
        } = self;
        match (cluster_id, point_count) {
            // MapLibre proximity cluster: both fields set, leaf props dropped.
            (Some(cluster_id), Some(_)) => Some(ClickTarget::Expand {
                cluster_id: Some(cluster_id),
                point,
                split_level: None,
            }),
            // Lone server `Expand` cell: not proximity-clustered; zoom to its
            // server-computed split level.
            _ if kind.as_deref() == Some("cluster") => Some(ClickTarget::Expand {
                cluster_id: None,
                point,
                split_level,
            }),
            _ => None,
        }
    }
}

/// The first feature of a MapLibre layer-click event, if any.
fn first_feature(event: &JsValue) -> Option<JsValue> {
    let features = js_sys::Reflect::get(event, &"features".into())
        .ok()?
        .dyn_into::<js_sys::Array>()
        .ok()?;
    if features.length() == 0 {
        return None;
    }
    Some(features.get(0))
}

/// A feature's `[lng, lat]` from its GeoJSON point geometry.
fn feature_point(feature: &JsValue) -> Option<[f64; 2]> {
    let coords = js_sys::Reflect::get(feature, &"geometry".into())
        .ok()
        .and_then(|g| js_sys::Reflect::get(&g, &"coordinates".into()).ok())?
        .dyn_into::<js_sys::Array>()
        .ok()?;
    let lng = coords.get(0).as_f64()?;
    let lat = coords.get(1).as_f64()?;
    Some([lng, lat])
}

/// Cluster-expansion animation timing: a base duration plus growth per zoom
/// level jumped, capped. A deep expansion (a tightly-packed cluster whose split
/// level is many levels down) flies over a longer path; a shallow one stays
/// quick.
const EXPAND_BASE_MS: f64 = 300.0;
const EXPAND_PER_LEVEL_MS: f64 = 120.0;
const EXPAND_MAX_MS: f64 = 1400.0;

/// Fly the map to `[lng, lat]` at `zoom` for a cluster expansion. `flyTo`
/// couples pan and zoom into one flight path, so an off-center badge recenters
/// as it zooms in — a single smooth motion — with a duration scaled to the zoom
/// delta so a deep jump stays smooth.
fn fly_to_point(map: &maplibre::Map, point: [f64; 2], zoom: f64) {
    // A cluster expand always zooms in. Behind the debounced refetch a stale
    // badge can name a target below the live zoom; clamp to the live zoom so the
    // fly stays inward, holding zoom and recentering when the target is stale.
    let current = map.get_zoom_raw();
    let target = zoom.max(current);
    let duration = (EXPAND_BASE_MS + EXPAND_PER_LEVEL_MS * (target - current)).min(EXPAND_MAX_MS);
    let opts = js_sys::Object::new();
    let center = js_sys::Array::of2(&point[0].into(), &point[1].into());
    let _ = js_sys::Reflect::set(&opts, &"center".into(), &center);
    let _ = js_sys::Reflect::set(&opts, &"zoom".into(), &target.into());
    let _ = js_sys::Reflect::set(&opts, &"duration".into(), &duration.into());
    map.fly_to(&opts);
}

/// Handle a click on an entity marker, dispatching on whether it resolves
/// to a single entity or a co-located disambiguation group.
fn handle_marker_click(event: JsValue, set_selected: WriteSignal<Option<EntitySelection>>) {
    let Some(feature) = first_feature(&event) else {
        return;
    };
    let Some(props_js) = js_sys::Reflect::get(&feature, &"properties".into()).ok() else {
        return;
    };

    let raw: RawMarkerProps = match serde_wasm_bindgen::from_value(props_js) {
        Ok(p) => p,
        Err(e) => {
            web_sys::console::warn_1(&format!("Failed to deserialize marker props: {e}").into());
            return;
        }
    };

    let Some(target) = raw.into_click_target() else {
        return;
    };

    match target {
        ClickTarget::Select(id) => {
            set_selected.set(Some(EntitySelection::Single {
                detail: id.clone(),
                feature: id,
                back: None,
            }));
        }
        ClickTarget::Disambiguate(entries) => {
            set_selected.set(Some(EntitySelection::Multiple(entries)));
        }
        // The individual layers filter clusters out, so a marker click never
        // resolves to an expansion — that path is the badge layer's.
        ClickTarget::Expand { .. } => {}
    }
}

/// Handle a click on the cluster-badge layer. A MapLibre proximity cluster
/// expands via the source's supercluster index (async); a lone server `Expand`
/// cell zooms straight to its server-computed split level.
fn handle_badge_click(
    event: JsValue,
    map: &maplibre::Map,
    set_selected: WriteSignal<Option<EntitySelection>>,
    source_generation: &Rc<Cell<u64>>,
) {
    let Some(feature) = first_feature(&event) else {
        return;
    };
    let Some(point) = feature_point(&feature) else {
        return;
    };
    let Some(props_js) = js_sys::Reflect::get(&feature, &"properties".into()).ok() else {
        return;
    };
    let raw: RawMarkerProps = match serde_wasm_bindgen::from_value(props_js) {
        Ok(p) => p,
        Err(e) => {
            web_sys::console::warn_1(&format!("Failed to deserialize badge props: {e}").into());
            return;
        }
    };
    let Some(ClickTarget::Expand {
        cluster_id,
        point,
        split_level,
    }) = raw.into_expand_target(point)
    else {
        return;
    };

    match cluster_id {
        Some(cluster_id) => {
            expand_proximity_cluster(map, cluster_id, point, set_selected, source_generation);
        }
        None => {
            if let Some(level) = split_level {
                fly_to_point(map, point, f64::from(level));
            }
        }
    }
}

/// Expand a MapLibre proximity cluster: await the source's expansion zoom, then
/// ease there — unless the cluster survives to the terminal zoom (coincident
/// points), where it resolves to a leaves picker instead.
fn expand_proximity_cluster(
    map: &maplibre::Map,
    cluster_id: u64,
    point: [f64; 2],
    set_selected: WriteSignal<Option<EntitySelection>>,
    source_generation: &Rc<Cell<u64>>,
) {
    let Some(source) = map.get_source(ENTITY_SOURCE_ID) else {
        return;
    };
    let map = map.clone();
    let source_generation = Rc::clone(source_generation);
    let generation_at_click = source_generation.get();

    wasm_bindgen_futures::spawn_local(async move {
        let expansion = wasm_bindgen_futures::JsFuture::from(
            source.get_cluster_expansion_zoom(cluster_id as f64),
        )
        .await;

        // A fetch since the click re-clustered the source, so this `cluster_id`
        // is stale — and a stale id can silently name a *different* live
        // cluster, which no promise rejection would catch. Discard.
        if source_generation.get() != generation_at_click {
            return;
        }

        // A rejected promise is a stale `cluster_id` — no-op.
        let Ok(zoom_js) = expansion else {
            return;
        };
        let Some(zoom) = zoom_js.as_f64() else {
            return;
        };
        if zoom > f64::from(MAX_ZOOM) {
            // Still clustered past the terminal zoom → coincident points.
            // Resolve to a disambiguation picker over the leaves.
            show_cluster_leaves_picker(
                &source,
                cluster_id,
                set_selected,
                &source_generation,
                generation_at_click,
            )
            .await;
        } else {
            fly_to_point(&map, point, zoom);
        }
    });
}

/// Build a disambiguation picker from a terminal cluster's leaves and select
/// it. A rejected promise (stale `cluster_id`), a re-cluster during the await
/// (`source_generation` moved on), or an empty result is a no-op.
async fn show_cluster_leaves_picker(
    source: &maplibre::GeoJsonSource,
    cluster_id: u64,
    set_selected: WriteSignal<Option<EntitySelection>>,
    source_generation: &Rc<Cell<u64>>,
    generation_at_click: u64,
) {
    let leaves = match wasm_bindgen_futures::JsFuture::from(source.get_cluster_leaves(
        cluster_id as f64,
        CLUSTER_LEAVES_LIMIT,
        0,
    ))
    .await
    {
        Ok(v) => v,
        Err(_) => return,
    };

    // A fetch during this round-trip re-clustered the source: `cluster_id` now
    // names a different cluster, so a picker built here would list entities the
    // user never clicked near. The expansion-zoom await guards the same staleness.
    if source_generation.get() != generation_at_click {
        return;
    }

    let Ok(leaves) = leaves.dyn_into::<js_sys::Array>() else {
        return;
    };

    let mut entries: Vec<EntityPickerEntry> = Vec::new();
    for i in 0..leaves.length() {
        let leaf = leaves.get(i);
        let Some(props_js) = js_sys::Reflect::get(&leaf, &"properties".into()).ok() else {
            continue;
        };
        if let Ok(raw) = serde_wasm_bindgen::from_value::<RawMarkerProps>(props_js) {
            append_leaf_entries(raw, &mut entries);
        }
    }
    if !entries.is_empty() {
        set_selected.set(Some(EntitySelection::Multiple(entries)));
    }
}

/// Fold one leaf feature's props into picker entries — a co-located group
/// contributes all its members, a lone entity contributes itself, and a nested
/// server `Expand` cell (no entity id) contributes nothing.
fn append_leaf_entries(raw: RawMarkerProps, entries: &mut Vec<EntityPickerEntry>) {
    if let Some(group) = raw.group {
        if let Ok(group) = serde_json::from_str::<Vec<EntityPickerEntry>>(&group) {
            entries.extend(group);
        }
    } else if let Some(id) = raw.id {
        entries.push(EntityPickerEntry { id, name: raw.name });
    }
}

/// Handle a click on the map background (not on an entity marker).
fn handle_background_click(
    event: JsValue,
    map: &maplibre::Map,
    set_selected: WriteSignal<Option<EntitySelection>>,
) {
    let point = js_sys::Reflect::get(&event, &"point".into()).ok();
    if let Some(point) = point {
        let opts = js_sys::Object::new();
        let layers = js_sys::Array::of3(
            &JsValue::from_str(ENTITY_CIRCLES_LAYER),
            &JsValue::from_str(ENTITY_THUMBNAILS_LAYER),
            &JsValue::from_str(ENTITY_BADGE_LAYER),
        );
        let _ = js_sys::Reflect::set(&opts, &"layers".into(), &layers);
        let features = map.query_rendered_features(&point, &opts);
        if features.length() > 0 {
            // Click hit a marker or a badge — its layer handler already fired.
            return;
        }
    }
    // Nothing hit — clear selection
    set_selected.set(None);
}

// ==================== Map lifecycle helpers ====================

/// Register click, hover, and background-click handlers on the entity layer.
///
/// Returns the closures that must be kept alive for the handlers to work.
/// (`wasm_bindgen::Closure` is invalidated when dropped — the Vec keeps
/// them alive for the map's lifetime, and they're cleared on cleanup/remount.)
/// Register click + hover handlers for a single marker layer.
///
/// Each layer needs its own `Closure` instances (MapLibre takes ownership),
/// so this is called once per interactive layer.
fn register_marker_layer(
    map: &maplibre::Map,
    layer: &str,
    set_selected: WriteSignal<Option<EntitySelection>>,
    closures: &mut Vec<Box<dyn std::any::Any>>,
) {
    let click_cb = Closure::<dyn Fn(JsValue)>::new(move |event: JsValue| {
        handle_marker_click(event, set_selected);
    });
    map.on_layer("click", layer, click_cb.as_ref());
    closures.push(Box::new(click_cb));

    let map_for_enter = map.clone();
    let enter_cb = Closure::<dyn Fn()>::new(move || {
        maplibre::set_cursor(&map_for_enter, "pointer");
    });
    map.on_layer("mouseenter", layer, enter_cb.as_ref());
    closures.push(Box::new(enter_cb));

    let map_for_leave = map.clone();
    let leave_cb = Closure::<dyn Fn()>::new(move || {
        maplibre::set_cursor(&map_for_leave, "");
    });
    map.on_layer("mouseleave", layer, leave_cb.as_ref());
    closures.push(Box::new(leave_cb));
}

/// Register the badge layer's click + hover handlers. Distinct from
/// [`register_marker_layer`] because a badge click needs the [`Map`] and the
/// source to run cluster expansion, and the fetch generation to reject a
/// `cluster_id` a re-cluster has invalidated.
///
/// [`Map`]: maplibre::Map
fn register_badge_layer(
    map: &maplibre::Map,
    set_selected: WriteSignal<Option<EntitySelection>>,
    source_generation: Rc<Cell<u64>>,
    closures: &mut Vec<Box<dyn std::any::Any>>,
) {
    let map_for_click = map.clone();
    let click_cb = Closure::<dyn Fn(JsValue)>::new(move |event: JsValue| {
        handle_badge_click(event, &map_for_click, set_selected, &source_generation);
    });
    map.on_layer("click", ENTITY_BADGE_LAYER, click_cb.as_ref());
    closures.push(Box::new(click_cb));

    let map_for_enter = map.clone();
    let enter_cb = Closure::<dyn Fn()>::new(move || {
        maplibre::set_cursor(&map_for_enter, "pointer");
    });
    map.on_layer("mouseenter", ENTITY_BADGE_LAYER, enter_cb.as_ref());
    closures.push(Box::new(enter_cb));

    let map_for_leave = map.clone();
    let leave_cb = Closure::<dyn Fn()>::new(move || {
        maplibre::set_cursor(&map_for_leave, "");
    });
    map.on_layer("mouseleave", ENTITY_BADGE_LAYER, leave_cb.as_ref());
    closures.push(Box::new(leave_cb));
}

fn register_layer_handlers(
    map: &maplibre::Map,
    set_selected: WriteSignal<Option<EntitySelection>>,
    source_generation: Rc<Cell<u64>>,
) -> Vec<Box<dyn std::any::Any>> {
    let mut closures: Vec<Box<dyn std::any::Any>> = Vec::new();

    register_marker_layer(map, ENTITY_CIRCLES_LAYER, set_selected, &mut closures);
    register_marker_layer(map, ENTITY_THUMBNAILS_LAYER, set_selected, &mut closures);
    register_badge_layer(map, set_selected, source_generation, &mut closures);

    // --- Map background click: dismiss panel when clicking empty area ---
    let map_for_bg = map.clone();
    let bg_click_cb = Closure::<dyn Fn(JsValue)>::new(move |event: JsValue| {
        handle_background_click(event, &map_for_bg, set_selected);
    });
    map.on("click", bg_click_cb.as_ref());
    closures.push(Box::new(bg_click_cb));

    closures
}

/// Register the `styleimagemissing` handler that lazily supplies thumbnail
/// rasters for the symbol layer.
///
/// A thumbnailed feature carries its `thumbnail` icon id from the first
/// `setData`, so MapLibre fires `styleimagemissing` for that id during the same
/// layout pass. The handler:
///
/// 1. Looks the id up in `thumbnail_urls`; ignores ids it doesn't own (basemap
///    sprites, stale ids).
/// 2. **Synchronously** registers a placeholder pin at the exact dimensions the
///    real raster will have. Synchronous registration is required: MapLibre
///    retries `getImage` within the same layout pass, so the placeholder shows
///    with no empty-frame flash, and matched dimensions let the later
///    `updateImage` swap succeed (it throws on a dimension mismatch).
/// 3. Spawns the async fetch + draw, then `updateImage`s the raster in place —
///    a render-only op that never touches source data, so it can't re-cluster.
///
/// Returns the closure that must be kept alive for the handler to fire.
fn register_styleimagemissing_handler(
    map: &maplibre::Map,
    thumbnail_urls: &Rc<RefCell<std::collections::BTreeMap<String, String>>>,
) -> Box<dyn std::any::Any> {
    let map_for_cb = map.clone();
    let thumbnail_urls = Rc::clone(thumbnail_urls);
    let cb = Closure::<dyn Fn(JsValue)>::new(move |event: JsValue| {
        let Some(id) = js_sys::Reflect::get(&event, &"id".into())
            .ok()
            .and_then(|v| v.as_string())
        else {
            return;
        };

        // Only our thumbnail ids; everything else (basemap sprites) is left to
        // MapLibre.
        let Some(url) = thumbnail_urls.borrow().get(&id).cloned() else {
            return;
        };

        // Already registered (a retry in this same pass, or a prior placeholder
        // that survived): nothing to do.
        if map_for_cb.has_image(&id) {
            return;
        }

        // Draw + register the placeholder synchronously so the pin renders this
        // pass with no empty-frame flash.
        let dpr = device_pixel_ratio();
        let placeholder = match draw_thumbnail_placeholder(dpr) {
            Ok(data) => data,
            Err(e) => {
                web_sys::console::warn_1(
                    &format!("thumbnail placeholder draw failed for {id}: {e}").into(),
                );
                return;
            }
        };
        let opts = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&opts, &"pixelRatio".into(), &dpr.into());
        if let Err(e) = map_for_cb.add_image_with_options(&id, placeholder.as_ref(), &opts) {
            web_sys::console::warn_1(
                &format!("placeholder add_image failed for {id}: {e:?}").into(),
            );
            return;
        }

        // Load the real raster and swap it in place.
        let map = map_for_cb.clone();
        let thumbnail_urls = Rc::clone(&thumbnail_urls);
        wasm_bindgen_futures::spawn_local(async move {
            let img = match load_image(&url).await {
                Ok(img) => img,
                Err(e) => {
                    web_sys::console::warn_1(
                        &format!("thumbnail load failed for {id}: {e}").into(),
                    );
                    return;
                }
            };
            // The marker may have panned away while loading: if eviction removed
            // the placeholder, or the id is no longer desired, drop the raster.
            if !map.has_image(&id) || !thumbnail_urls.borrow().contains_key(&id) {
                return;
            }
            let raster = match draw_circular_thumbnail(&img, dpr) {
                Ok(data) => data,
                Err(e) => {
                    web_sys::console::warn_1(
                        &format!("thumbnail draw failed for {id}: {e}").into(),
                    );
                    return;
                }
            };
            match map.update_image(&id, raster.as_ref()) {
                Ok(()) => {
                    // Signal thumbnail readiness for test hooks.
                    #[cfg(feature = "test-hooks")]
                    record_thumbnails_loaded();
                }
                Err(e) => {
                    web_sys::console::warn_1(
                        &format!("update_image failed for {id}: {e:?}").into(),
                    );
                }
            }
        });
    });
    map.on("styleimagemissing", cb.as_ref());
    Box::new(cb)
}

/// Set up the debounced `moveend` handler that re-fetches entities on pan/zoom.
///
/// Returns the closure that must be kept alive.
fn register_moveend_handler(
    map: &maplibre::Map,
    state: &MapState,
    signals: ViewportSignals,
) -> Box<dyn std::any::Any> {
    let map_for_move = map.clone();
    let state_for_move = state.clone();

    let move_cb = Closure::<dyn Fn()>::new(move || {
        // Clear any pending debounce timer
        if let Some(timer_id) = state_for_move.debounce_timer.get()
            && let Some(w) = web_sys::window()
        {
            w.clear_timeout_with_handle(timer_id);
        }

        let map_ref = map_for_move.clone();
        let st = state_for_move.clone();

        // `once_into_js` transfers ownership to JS: the closure is freed by
        // the JS garbage collector after the timer fires.
        let timeout_cb = Closure::once_into_js(move || {
            st.debounce_timer.set(None);
            let st_pass = st.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let Some(client) = api::get_or_init_client(&st_pass.api_client).await else {
                    signals
                        .set_fetch_error
                        .set(Some("Failed to load API configuration".to_string()));
                    return;
                };
                refresh_tiles_for_viewport(&map_ref, &client, signals, &st_pass).await;
            });
        });

        if let Some(w) = web_sys::window()
            && let Ok(id) = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                timeout_cb.unchecked_ref(),
                MOVEEND_DEBOUNCE_MS,
            )
        {
            state_for_move.debounce_timer.set(Some(id));
        }
    });
    map.on("moveend", move_cb.as_ref());
    Box::new(move_cb)
}

// ==================== Shared map state ====================

/// State shared between the `MapView` component, its Effects, and JS closures.
///
/// All fields are `Rc` (cheap to clone) so we can hand out copies to closures
/// without threading dozens of individual `Rc::clone` calls.
#[derive(Clone)]
struct MapState {
    source_initialized: SourceInitialized,
    debounce_timer: Rc<Cell<Option<i32>>>,
    closures: Rc<RefCell<Vec<Box<dyn std::any::Any>>>>,
    api_client: Rc<RefCell<Option<api::Client>>>,
    /// `thumb-<id>` style-image id → source URL for every thumbnail in the
    /// current assembly. The `styleimagemissing` handler resolves a missing
    /// id's URL here; each pass diffs the keys and `removeImage`s the ids that
    /// fell out, bounding the pool to on-screen thumbnails.
    thumbnail_urls: Rc<RefCell<std::collections::BTreeMap<String, String>>>,
    /// Bumped on every source `setData` (each pass re-clusters). A badge click
    /// captures this before awaiting cluster expansion and re-checks after,
    /// discarding a `cluster_id` a re-cluster has invalidated.
    source_generation: Rc<Cell<u64>>,
    /// Fetched container tiles keyed by `(snapshot, level, x, y)`. Populated by
    /// `refresh_tiles_for_viewport`; a snapshot or level change simply keys
    /// past the stale entries.
    tile_cache: Rc<RefCell<BTreeMap<TileKey, Vec<MapMarker>>>>,
    /// The snapshot the session pinned to on its first tile response. Threaded
    /// into every later fetch and cache key so one session stays read-coherent.
    pinned_snapshot: Rc<RefCell<Option<Snapshot>>>,
    /// Monotonic per-pass counter — the last-write-wins guard. A pass captures
    /// it before awaiting and drops its result if a newer pass has since bumped
    /// it, so a slow fetch can't overwrite a fresher assembly.
    pass_generation: Rc<Cell<u64>>,
    /// The current pass's fetch abort handle. Each pass installs its own and
    /// aborts the prior one, so a superseded pass's in-flight fetches are
    /// dropped (which cancels the browser requests) rather than run to
    /// completion. `on_cleanup` aborts it too, so unmount cancels any pass.
    pass_abort: Rc<RefCell<Option<AbortHandle>>>,
    /// Set true by `on_cleanup` before the map is removed. A tile pass parked on
    /// the `get_or_init_client` await — which no pass abort covers — checks it at
    /// entry when it resumes, so a pass that outlives unmount never drives the
    /// removed map or writes a disposed signal.
    disposed: Rc<Cell<bool>>,
    /// The instant the map is rewound to — the time slider's position, and the
    /// `as_of` every tile fetch and cache key carries. Shared state rather than a
    /// pass argument so a pan and a scrub read the same value and cannot race
    /// two different instants into one assembly.
    as_of: Rc<Cell<NaiveDate>>,
    /// The instant the markers currently on screen were read at. Lets a failed
    /// pass tell "stale viewport, same year" (keep the render) from "stale year"
    /// (drop it — markers labelled with a date they don't belong to are worse
    /// than none).
    rendered_as_of: Rc<Cell<Option<NaiveDate>>>,
}

/// Create the map, register the style-load callback (which adds source/layers,
/// click/hover handlers, and kicks off the initial entity fetch), and the
/// debounced moveend handler.
///
/// Returns the closures that must be kept alive and the map instance.
fn initialize_map(
    el: &web_sys::HtmlDivElement,
    state: &MapState,
    signals: ViewportSignals,
    set_selected: WriteSignal<Option<EntitySelection>>,
    set_map_error: WriteSignal<Option<String>>,
) -> Option<maplibre::Map> {
    let map = maplibre::create_map(
        el,
        &maplibre::MapOptions {
            style: MAP_STYLE_URL,
            center: [INITIAL_CENTER_LNG, INITIAL_CENTER_LAT],
            zoom: INITIAL_ZOOM,
            max_zoom: f64::from(MAX_ZOOM),
            min_zoom: f64::from(MIN_ZOOM),
            fade_duration: SYMBOL_FADE_MS,
        },
    )?;

    // --- On style load: add source/layers, register handlers, initial fetch ---
    //
    // We register via `map.once("load", ...)` unconditionally rather than
    // branching on `is_style_loaded()`. MapLibre's `once` fires immediately
    // (on the next tick) if the style is already loaded, and `is_style_loaded`
    // is a known footgun: it can return true while glyph fetches are still
    // outstanding, causing the initial fetch to race with style finalization.
    let st = state.clone();
    let map_for_load = map.clone();
    let load_cb = Closure::<dyn Fn()>::new(move || {
        let map_ref = map_for_load.clone();
        let st2 = st.clone();

        init_source_and_layers(&map_ref);
        st.source_initialized.set(true);

        let handler_closures =
            register_layer_handlers(&map_ref, set_selected, Rc::clone(&st.source_generation));
        st.closures.borrow_mut().extend(handler_closures);

        let missing_image_closure =
            register_styleimagemissing_handler(&map_ref, &st.thumbnail_urls);
        st.closures.borrow_mut().push(missing_image_closure);

        wasm_bindgen_futures::spawn_local(async move {
            let Some(client) = api::get_or_init_client(&st2.api_client).await else {
                signals
                    .set_fetch_error
                    .set(Some("Failed to load API configuration".to_string()));
                return;
            };
            refresh_tiles_for_viewport(&map_ref, &client, signals, &st2).await;
        });
    });

    map.once("load", load_cb.as_ref());
    state.closures.borrow_mut().push(Box::new(load_cb));

    let moveend_closure = register_moveend_handler(&map, state, signals);
    state.closures.borrow_mut().push(moveend_closure);

    // Surface MapLibre errors from our entity source in the UI. Errors
    // from the basemap (tile loading, style evaluation) are logged but
    // not surfaced since they're outside our control.
    let error_cb = Closure::<dyn Fn(JsValue)>::new(move |event: JsValue| {
        let msg = js_sys::Reflect::get(&event, &"message".into())
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_else(|| "Map style error".to_string());

        let source_id = js_sys::Reflect::get(&event, &"sourceId".into())
            .ok()
            .and_then(|v| v.as_string());

        if source_id.as_deref() == Some(ENTITY_SOURCE_ID) {
            set_map_error.set(Some(msg));
        }
    });
    map.on("error", error_cb.as_ref());
    state.closures.borrow_mut().push(Box::new(error_cb));

    Some(map)
}

// ==================== Effects ====================

/// Mount/remount the MapLibre map when the container DOM node becomes available.
///
/// On first run, `container.get()` is `None` (DOM not yet rendered); the
/// effect re-runs automatically when the `NodeRef` resolves. On subsequent
/// runs (e.g., hot-reload), the old map is destroyed before creating a new one.
fn effect_mount_map(
    container: NodeRef<leptos::html::Div>,
    map_handle: Rc<RefCell<Option<maplibre::Map>>>,
    state: MapState,
    signals: ViewportSignals,
    set_selected: WriteSignal<Option<EntitySelection>>,
    set_map_error: WriteSignal<Option<String>>,
) {
    Effect::new(move || {
        let Some(el) = container.get() else { return };

        if let Some(old_map) = map_handle.borrow().as_ref() {
            old_map.remove();
        }
        state.closures.borrow_mut().clear();
        state.thumbnail_urls.borrow_mut().clear();
        state.source_initialized.set(false);

        if let Some(map) = initialize_map(&el, &state, signals, set_selected, set_map_error) {
            *map_handle.borrow_mut() = Some(map);

            // Signal that the map has mounted (used by test hooks).
            #[cfg(feature = "test-hooks")]
            dispatch_window_event(MAP_READY_EVENT);
        }
    });
}

/// Rebuilds the GeoJSON source when the marker list changes.
///
/// The source is built once per fetch: a thumbnailed feature carries its
/// `thumbnail` icon id from this `setData`, and the raster is supplied out of
/// band via `styleimagemissing` + `updateImage` (style-image ops that never
/// touch source data). So this effect reacts only to the marker list signal,
/// not to individual thumbnail load completions.
///
/// Selection highlighting is handled separately via `setFeatureState` in
/// `effect_selection` — this effect does not read `signals.selected`, so
/// clicking an entity doesn't trigger a full GeoJSON rebuild+reserialize.
fn effect_rebuild_geojson(
    signals: ViewportSignals,
    map_handle: Rc<RefCell<Option<maplibre::Map>>>,
    source_initialized: SourceInitialized,
    source_generation: Rc<Cell<u64>>,
) {
    Effect::new(move || {
        // Tracked read: re-runs when marker list changes.
        let markers = signals.cached_markers.get();

        if !source_initialized.get() {
            return;
        }
        let Some(map) = map_handle.borrow().as_ref().cloned() else {
            return;
        };

        if markers.is_empty() {
            clear_source_data(&map, ENTITY_SOURCE_ID);
        } else if let Some(geojson) = build_markers_geojson(&markers) {
            update_source_data(&map, ENTITY_SOURCE_ID, &geojson);
        }

        // The new dataset re-clusters, invalidating any `cluster_id` a
        // badge-click expansion in flight captured before this point.
        source_generation.set(source_generation.get().wrapping_add(1));
    });
}

/// Apply selection highlighting via `setFeatureState` instead of rebuilding
/// the entire GeoJSON. Tracks `signals.selected` — when the selection changes,
/// clears the old feature state and sets the new one. Only entities (not
/// clusters) can be selected.
fn effect_selection(
    signals: ViewportSignals,
    map_handle: Rc<RefCell<Option<maplibre::Map>>>,
    source_initialized: SourceInitialized,
) {
    // Track the previously-selected feature ID so we can clear its state.
    let prev_id: Rc<Cell<Option<String>>> = Rc::new(Cell::new(None));

    Effect::new(move || {
        // Feature ids are the marker's opaque wire id as a string (see
        // `MapMarker::id` / `build_markers_geojson`). Highlight the marker the
        // selection belongs to — its representative `feature` id — which for a
        // disambiguation pick differs from the picked member's detail id.
        let new_id = match signals.selected.get() {
            Some(EntitySelection::Single { feature, .. }) => Some(feature.as_str().to_string()),
            _ => None,
        };

        if !source_initialized.get() {
            return;
        }
        let Some(map) = map_handle.borrow().as_ref().cloned() else {
            return;
        };

        // Clear previous selection
        if let Some(old_id) = prev_id.take() {
            let target = serde_json::json!({
                "source": ENTITY_SOURCE_ID,
                "id": old_id,
            });
            if let Ok(target_js) = to_js(&target) {
                let _ = map.remove_feature_state(&target_js);
            }
        }

        // Set new selection
        if let Some(ref id) = new_id {
            let target = serde_json::json!({
                "source": ENTITY_SOURCE_ID,
                "id": id,
            });
            let state = serde_json::json!({"selected": true});
            if let Ok(target_js) = to_js(&target)
                && let Ok(state_js) = to_js(&state)
            {
                let _ = map.set_feature_state(&target_js, &state_js);
            }
        }

        prev_id.set(new_id);
    });
}

/// Re-fetch entities when the retry signal fires (after a failed fetch).
fn effect_retry_on_signal(
    retry_signal: ReadSignal<bool>,
    set_retry: WriteSignal<bool>,
    map_handle: Rc<RefCell<Option<maplibre::Map>>>,
    state: MapState,
    signals: ViewportSignals,
) {
    Effect::new(move || {
        if retry_signal.get() {
            set_retry.set(false);
            let map_ref = map_handle.borrow();
            if let Some(map) = map_ref.as_ref() {
                let map = map.clone();
                let st = state.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let Some(client) = api::get_or_init_client(&st.api_client).await else {
                        signals
                            .set_fetch_error
                            .set(Some("Failed to load API configuration".to_string()));
                        return;
                    };
                    refresh_tiles_for_viewport(&map, &client, signals, &st).await;
                });
            }
        }
    });
}

/// Re-read the visible tiles whenever the time slider moves.
///
/// The instant lives in `MapState` so the fetch pass reads one value; this
/// effect writes it there, then fires the pass through the *same* debounce timer
/// the pan path uses, so a scrub and a pan can never drive two passes at once.
/// Verdicts come from the server, so a new instant is a new read — the tile
/// cache keys on `as_of` and simply misses.
fn effect_refetch_on_as_of(
    as_of: ReadSignal<NaiveDate>,
    map_handle: Rc<RefCell<Option<maplibre::Map>>>,
    state: MapState,
    signals: ViewportSignals,
) {
    // One reusable `Fn` closure for every scrub, built once and kept alive here.
    // A dragged range input fires `input` per pixel, and a `Closure::once_into_js`
    // is only freed when it runs — so a per-event closure would leak its captured
    // state on every cancelled debounce, which is most of them.
    let timer_state = state.clone();
    let timer_map = Rc::clone(&map_handle);
    let timeout_cb: Rc<Closure<dyn Fn()>> = Rc::new(Closure::new(move || {
        timer_state.debounce_timer.set(None);
        let map_ref = timer_map.borrow();
        let Some(map) = map_ref.as_ref() else { return };
        let map = map.clone();
        let st_pass = timer_state.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let Some(client) = api::get_or_init_client(&st_pass.api_client).await else {
                signals
                    .set_fetch_error
                    .set(Some("Failed to load API configuration".to_string()));
                return;
            };
            refresh_tiles_for_viewport(&map, &client, signals, &st_pass).await;
        });
    }));

    Effect::new(move || {
        let instant = as_of.get();
        if state.as_of.get() == instant {
            return;
        }
        state.as_of.set(instant);

        if let Some(timer_id) = state.debounce_timer.get()
            && let Some(w) = web_sys::window()
        {
            w.clear_timeout_with_handle(timer_id);
        }

        if let Some(w) = web_sys::window()
            && let Ok(id) = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                timeout_cb.as_ref().as_ref().unchecked_ref(),
                MOVEEND_DEBOUNCE_MS,
            )
        {
            state.debounce_timer.set(Some(id));
        }
    });
}

// ==================== Component ====================

#[component]
pub fn MapView(
    api_client: Rc<RefCell<Option<api::Client>>>,
    /// Shared map handle — populated when the map mounts. Allows external
    /// code (e.g., test hooks) to access the live map instance.
    #[prop(optional)]
    map_handle: Option<Rc<RefCell<Option<maplibre::Map>>>>,
) -> impl IntoView {
    let container = NodeRef::<leptos::html::Div>::new();
    let map_handle = map_handle.unwrap_or_else(|| Rc::new(RefCell::new(None)));

    // Selected entity signal — provided to parent via context
    let (selected, set_selected) = signal(None::<EntitySelection>);
    provide_context(SelectedEntity(selected, set_selected));

    // Map status signals — provided to parent via context
    let (loading, set_loading) = signal(false);
    let (empty, set_empty) = signal(false);
    let (fetch_error, set_fetch_error) = signal(None::<String>);
    let (retry_signal, set_retry) = signal(false);
    provide_context(MapStatus {
        loading,
        empty,
        fetch_error,
        retry: set_retry,
    });

    // The instant the map reads existence at. Defaults to the present, so the
    // first view is the world as it stands; the slider rewinds from there.
    // Sampled once and shared with `MapState` below: two `today()` calls could
    // straddle midnight and start the signal and the fetch state a day apart.
    let initial_as_of = today();
    let (as_of, set_as_of) = signal(initial_as_of);
    provide_context(TimeControl {
        as_of,
        set_as_of,
        now: initial_as_of,
    });

    let (map_error, set_map_error) = signal(None::<String>);

    let (cached_markers, set_cached_markers) = signal(Vec::<MapMarker>::new());

    let signals = ViewportSignals {
        set_loading,
        set_empty,
        set_fetch_error,
        set_map_error,
        selected,
        cached_markers,
        set_cached_markers,
    };

    // Shared state for JS closures. Grouped into a struct so we clone once
    // instead of threading a dozen individual Rc::clone calls.
    let state = MapState {
        source_initialized: RwSignal::new(false),
        debounce_timer: Rc::new(Cell::new(None)),
        // wasm_bindgen::Closure must be kept alive as long as JS holds a reference
        // to the callback. Dropping a Closure invalidates the JS-side function
        // reference. This vec keeps them alive for the map's lifetime; it's
        // cleared on cleanup or remount.
        closures: Rc::new(RefCell::new(Vec::new())),
        api_client,
        thumbnail_urls: Rc::new(RefCell::new(std::collections::BTreeMap::new())),
        source_generation: Rc::new(Cell::new(0)),
        tile_cache: Rc::new(RefCell::new(BTreeMap::new())),
        pinned_snapshot: Rc::new(RefCell::new(None)),
        pass_generation: Rc::new(Cell::new(0)),
        pass_abort: Rc::new(RefCell::new(None)),
        disposed: Rc::new(Cell::new(false)),
        as_of: Rc::new(Cell::new(initial_as_of)),
        rendered_as_of: Rc::new(Cell::new(None)),
    };
    effect_mount_map(
        container,
        Rc::clone(&map_handle),
        state.clone(),
        signals,
        set_selected,
        set_map_error,
    );
    effect_rebuild_geojson(
        signals,
        Rc::clone(&map_handle),
        state.source_initialized,
        Rc::clone(&state.source_generation),
    );
    effect_selection(signals, Rc::clone(&map_handle), state.source_initialized);
    effect_refetch_on_as_of(as_of, Rc::clone(&map_handle), state.clone(), signals);
    effect_retry_on_signal(
        retry_signal,
        set_retry,
        Rc::clone(&map_handle),
        state.clone(),
        signals,
    );

    // Clean up on unmount: destroy the map and release JS closure references.
    //
    // `SendWrapper` is required because Leptos's `on_cleanup` wants `Send`
    // closures even on single-threaded WASM targets. There are no other
    // threads in browser WASM, so the wrapper's runtime panic-on-cross-
    // thread-access can never fire here — it's purely a type-bound shim.
    let cleanup_handle = SendWrapper::new(Rc::clone(&map_handle));
    let cleanup_closures = SendWrapper::new(Rc::clone(&state.closures));
    let cleanup_debounce = SendWrapper::new(Rc::clone(&state.debounce_timer));
    let cleanup_pass_generation = SendWrapper::new(Rc::clone(&state.pass_generation));
    let cleanup_pass_abort = SendWrapper::new(Rc::clone(&state.pass_abort));
    let cleanup_disposed = SendWrapper::new(Rc::clone(&state.disposed));
    on_cleanup(move || {
        // Mark the component disposed before tearing anything down, so a tile
        // pass parked on the `get_or_init_client` await (outside the pass abort)
        // sees the flag when it resumes and bails before touching the removed
        // map — this is set before `map.remove()` below for exactly that reason.
        cleanup_disposed.set(true);
        // Cancel any pending debounce timer so it doesn't fire after unmount.
        if let Some(timer_id) = cleanup_debounce.get()
            && let Some(w) = web_sys::window()
        {
            w.clear_timeout_with_handle(timer_id);
        }
        // Bump the pass generation so any in-flight tile pass sees itself
        // superseded and returns before touching disposed signals or the
        // removed map, and abort its in-flight fetches so they're cancelled
        // rather than left running against a disposed component.
        cleanup_pass_generation.set(cleanup_pass_generation.get().wrapping_add(1));
        if let Some(handle) = cleanup_pass_abort.borrow_mut().take() {
            handle.abort();
        }
        if let Some(map) = cleanup_handle.borrow().as_ref() {
            map.remove();
        }
        cleanup_closures.borrow_mut().clear();
    });

    view! {
        <div class="relative w-full h-full">
            <div
                node_ref=container
                class="w-full h-full"
                role="region"
                aria-label="Interactive map"
            />
            {move || {
                map_error.get().map(|msg| view! {
                    <div
                        class="absolute bottom-2 left-2 right-2 bg-red-900/90 text-white text-xs px-3 py-2 rounded shadow"
                        role="alert"
                    >
                        {msg}
                    </div>
                })
            }}
        </div>
    }
}
