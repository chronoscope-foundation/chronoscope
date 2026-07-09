//! Entity API response types — read side of the fact store.
//!
//! These are thin aliases over the fact store's own `typed`/`listing` DTOs
//! (`chronoscope_core::{typed, listing}`), concretized to the in-memory backend's id
//! scheme. The server projects a `MemoryFactStore` snapshot straight into
//! these shapes; there's no separate wire-format translation layer.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use url::Url;

use chronoscope_core::GeoPoint;
use chronoscope_core::conflicts::AnyConflictReport;
use chronoscope_core::grammar::depiction::Perspective;
use chronoscope_core::grammar::image::ImageMedium;
use chronoscope_core::store::memory::{MemoryEntityId, MemoryEventId, MemoryImageId};
use chronoscope_core::{listing, typed};

/// Full entity detail — the fact store's typed projection, concretized to the
/// in-memory backend's id scheme. Wrapped in [`EntityDetail`] by
/// `GET /entities/{id}`; no infrastructure envelope (no `created_at`/`updated_at`
/// — the fact store has no row-level timestamps, only per-fact provenance
/// already carried inside the typed fields).
pub type Entity = typed::Entity<MemoryEntityId, MemoryEventId, MemoryImageId>;

/// The `GET /entities/{id}` response: the typed entity, the display name the
/// server negotiated from the request's `Accept-Language`, the resolved image
/// grid the detail panel renders, and the entity's own conflict reports. The
/// entity's `depictions` name the images by id; `images` carries each depicted
/// image's resolved URLs plus its structured perspective/medium so the client
/// renders the grid without a second round-trip per image.
///
/// `entity.names` still carries every localized name with its provenance;
/// `display_name` is just the one the panel heading shows, chosen server-side so
/// every client agrees on it. `None` only when the entity has no name at all.
///
/// `conflicts` are the over-determined date slots the detector found in this one
/// entity's projection — each a structured report the panel renders a disputed
/// indicator from. Empty when every date slot is consistent.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EntityDetail {
    pub entity: Entity,
    pub display_name: Option<String>,
    pub images: Vec<DetailImage>,
    pub conflicts: Vec<AnyConflictReport<MemoryEntityId, MemoryEventId>>,
}

/// One image in an entity's detail grid: the id it is keyed by, the URL the
/// client actually loads (`display_url`), the real provenance URL for the
/// lightbox's "open original" link (`source_url`), and the structured view
/// classification the client renders a caption from.
///
/// `display_url` always loads from our own `/media/{key}` host — same-origin,
/// so the canvas thumbnail draw stays CORS-safe — while `source_url` keeps the
/// upstream provenance URL for the lightbox's "open original". In placeholder
/// mode (dev/test) `display_url` is one shared local placeholder; otherwise
/// it's the resolver's stored copy of the source.
///
/// `perspective` and `medium` are the depiction's settled perspective and the
/// image's settled medium, each `None` when the underlying claim is absent or
/// unsettled. The client composes its own localized caption from them; the wire
/// carries the structured values, not a pre-rendered English string.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DetailImage {
    pub id: MemoryImageId,
    #[schemars(with = "String")]
    pub display_url: Url,
    #[schemars(with = "String")]
    pub source_url: Url,
    pub perspective: Option<Perspective>,
    pub medium: Option<ImageMedium>,
}

/// The grid caption for a detail image, composed from the depiction's settled
/// perspective and the image's settled medium — e.g. "Exterior picture", "Map".
/// A client-side helper over [`DetailImage`]'s structured fields; the wire still
/// carries the structured values, not this rendered string.
///
/// Always non-empty and free of the word "view", so a caller that appends
/// " view" for the image's aria-label never doubles the word.
pub fn image_caption(perspective: Option<Perspective>, medium: Option<ImageMedium>) -> String {
    // (lowercase, capitalized) forms of the medium noun: the lowercase reads as
    // the tail of "Exterior picture", the capitalized stands alone.
    let noun = match medium {
        Some(ImageMedium::Picture) => ("picture", "Picture"),
        Some(ImageMedium::Map) => ("map", "Map"),
        Some(ImageMedium::PictorialMap) => ("pictorial map", "Pictorial map"),
        None => ("image", "Image"),
    };
    match perspective {
        Some(Perspective::Exterior) => format!("Exterior {}", noun.0),
        Some(Perspective::Interior) => format!("Interior {}", noun.0),
        None => noun.1.to_string(),
    }
}

/// One placeable entity in a viewport listing: id, names, current marker, and
/// timeline date span. Returned by `GET /entities`.
pub type EntitySummary = listing::EntitySummary<MemoryEntityId, MemoryImageId>;

/// An opaque resume token for `GET /entities` pagination. The server mints one
/// per page; the client threads it back verbatim. Its contents — the pinned
/// snapshot and walk position — are server-internal and never inspected
/// client-side.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct Cursor(String);

impl Cursor {
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Cursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One page of a `GET /entities` viewport listing: the summaries gathered this
/// page and the opaque [`Cursor`] for the next, `None` once the viewport is
/// exhausted.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EntityListPage {
    pub summaries: Vec<EntitySummary>,
    pub next: Option<Cursor>,
}

// ==================== Unified Markers ====================

/// A map marker for one entity (or a co-located group of entities).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Marker {
    /// The entity id — for a co-located group, the group's first member
    /// (sorted by earliest date). `click_action` carries every member.
    pub id: MemoryEntityId,
    pub point: GeoPoint,
    /// The representative entity's display name, negotiated server-side from the
    /// request's `Accept-Language`. `None` when the entity has no name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The representative entity's thumbnail URL, when it has a depicted image.
    /// Same `display_url` semantics as [`DetailImage`]: served from our own
    /// `/media/{key}` host, or the shared placeholder in dev/test.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub thumbnail_url: Option<Url>,
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
    /// The entity's display name, negotiated server-side from the request's
    /// `Accept-Language`. `None` when the entity has no name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Response for the unified markers endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MarkersResponse {
    pub markers: Vec<Marker>,
    /// True if results were truncated at the server limit.
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_caption_is_non_empty_and_never_contains_view() {
        // The panel builds each image's aria-label as `"{caption} view"`, so a
        // caption already containing "view" would read "… view view". Pin the
        // invariant across every perspective × medium the wire can carry.
        let perspectives = [
            None,
            Some(Perspective::Exterior),
            Some(Perspective::Interior),
        ];
        let media = [
            None,
            Some(ImageMedium::Picture),
            Some(ImageMedium::Map),
            Some(ImageMedium::PictorialMap),
        ];
        for perspective in perspectives {
            for medium in media {
                let caption = image_caption(perspective, medium);
                assert!(
                    !caption.is_empty(),
                    "caption for {perspective:?}/{medium:?} must be non-empty"
                );
                assert!(
                    !caption.contains("view"),
                    "caption {caption:?} must not contain \"view\" — the aria-label appends \" view\""
                );
            }
        }
    }
}
