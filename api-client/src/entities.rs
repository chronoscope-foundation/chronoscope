//! Entity API response types — read side of the fact store.
//!
//! These are thin aliases over the fact store's own `typed`/`listing` DTOs
//! (`chronoscope_core::facts::*`), concretized to the in-memory backend's id
//! scheme. The server projects a `MemoryFactStore` snapshot straight into
//! these shapes; there's no separate wire-format translation layer.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use chronoscope_core::facts::ids::FactId;
use chronoscope_core::facts::memory::{MemoryEntityId, MemoryEventId, MemoryImageId};
use chronoscope_core::facts::{listing, typed};

/// Full entity detail — the fact store's typed projection, concretized to the
/// in-memory backend's id scheme. Wrapped in [`EntityDetail`] by
/// `GET /entities/{id}`; no infrastructure envelope (no `created_at`/`updated_at`
/// — the fact store has no row-level timestamps, only per-fact provenance
/// already carried inside the typed fields).
pub type Entity = typed::Entity<MemoryEntityId, MemoryEventId, MemoryImageId>;

/// The `GET /entities/{id}` response: the typed entity plus the resolved image
/// grid the detail panel renders. The entity's `depictions` name the images by
/// id; `images` carries each depicted image's resolved URLs and label so the
/// client renders the grid without a second round-trip per image.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EntityDetail {
    pub entity: Entity,
    pub images: Vec<DetailImage>,
}

/// One image in an entity's detail grid: the id it is keyed by, the URL the
/// client actually loads (`display_url`), the real provenance URL for the
/// lightbox's "open original" link (`source_url`), and a short human label.
///
/// `display_url` always loads from our own `/media/{key}` host — same-origin,
/// so the canvas thumbnail draw stays CORS-safe — while `source_url` keeps the
/// upstream provenance URL for the lightbox's "open original". In placeholder
/// mode (dev/test) `display_url` is one shared local placeholder; otherwise
/// it's the resolver's stored copy of the source.
///
/// `label` is a short grid caption built from the depiction's perspective and
/// the image's medium (e.g. "Exterior picture", "Map"). It is always non-empty
/// and never contains the word "view"; the web appends " view" to form the
/// image's aria-label.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DetailImage {
    pub id: MemoryImageId,
    pub display_url: String,
    pub source_url: String,
    pub label: String,
}

/// One placeable entity in a viewport listing: id, names, current marker, and
/// timeline date span. Returned by `GET /entities`.
pub type EntitySummary = listing::EntitySummary<MemoryEntityId, MemoryImageId>;

/// The resume cursor threaded through `GET /entities` pagination: the walk
/// position `summaries_in_bbox` hands back, JSON-encoded into the `cursor`
/// query parameter for the next request.
pub type EntityListCursor = (MemoryEntityId, FactId);

/// One page of a `GET /entities` viewport listing.
pub type EntityListPage = listing::EntityListPage<MemoryEntityId, MemoryImageId, EntityListCursor>;

// ==================== Unified Markers ====================

/// A map marker for one entity (or a co-located group of entities).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Marker {
    /// The entity id — for a co-located group, the group's first member
    /// (sorted by earliest date). `click_action` carries every member.
    pub id: MemoryEntityId,
    pub latitude: f64,
    pub longitude: f64,
    /// Display label (best-language entity name).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The representative entity's thumbnail URL, when it has a depicted image.
    /// Same `display_url` semantics as [`DetailImage`]: served from our own
    /// `/media/{key}` host, or the shared placeholder in dev/test.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail_url: Option<String>,
    /// What happens when the user clicks this marker.
    pub click_action: ClickAction,
}

/// What happens when a marker is clicked.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum ClickAction {
    /// Open the entity detail panel.
    #[serde(rename = "select")]
    Select { entity_id: MemoryEntityId },
    /// Show a disambiguation picker (co-located entities at the same point).
    #[serde(rename = "disambiguate")]
    Disambiguate { entries: Vec<EntityPickerEntry> },
}

/// One entry in a co-located entity disambiguation picker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EntityPickerEntry {
    pub id: MemoryEntityId,
    pub name: Option<String>,
}

/// Response for the unified markers endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MarkersResponse {
    pub markers: Vec<Marker>,
    /// True if results were truncated at the server limit.
    pub truncated: bool,
}
