//! Entity API response types — read side of the fact store.
//!
//! These are thin aliases over the fact store's own `typed`/`listing` DTOs
//! (`chronoscope_core::facts::*`), concretized to the in-memory backend's id
//! scheme. The server projects a `MemoryFactStore` snapshot straight into
//! these shapes; there's no separate wire-format translation layer.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use chronoscope_core::facts::depiction::Perspective;
use chronoscope_core::facts::ids::FactId;
use chronoscope_core::facts::image::ImageMedium;
use chronoscope_core::facts::memory::{MemoryEntityId, MemoryEventId, MemoryImageId};
use chronoscope_core::facts::{listing, typed};

/// Full entity detail — the fact store's typed projection, concretized to the
/// in-memory backend's id scheme. Wrapped in [`EntityDetail`] by
/// `GET /entities/{id}`; no infrastructure envelope (no `created_at`/`updated_at`
/// — the fact store has no row-level timestamps, only per-fact provenance
/// already carried inside the typed fields).
pub type Entity = typed::Entity<MemoryEntityId, MemoryEventId, MemoryImageId>;

/// The `GET /entities/{id}` response: the typed entity, the display name the
/// server negotiated from the request's `Accept-Language`, and the resolved
/// image grid the detail panel renders. The entity's `depictions` name the
/// images by id; `images` carries each depicted image's resolved URLs plus its
/// structured perspective/medium so the client renders the grid without a
/// second round-trip per image.
///
/// `entity.names` still carries every localized name with its provenance;
/// `display_name` is just the one the panel heading shows, chosen server-side so
/// every client agrees on it. `None` only when the entity has no name at all.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EntityDetail {
    pub entity: Entity,
    pub display_name: Option<String>,
    pub images: Vec<DetailImage>,
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
    pub display_url: String,
    pub source_url: String,
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
    /// The representative entity's display name, negotiated server-side from the
    /// request's `Accept-Language`. `None` when the entity has no name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
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
