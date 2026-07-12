//! Entity API response types — read side of the fact store.
//!
//! The [`typed`]/[`listing`] DTOs and the wrappers here are generic over the id
//! scheme: the server instantiates them at a backend's concrete ids (which
//! serialize as opaque strings) and a client at [`EntityId`]/[`EventId`]/
//! [`ImageId`], both sides sharing one wire shape. The `Entity`/`EntitySummary`
//! aliases below are the client-facing instantiations. There's no separate
//! wire-format translation layer — the server projects a snapshot straight into
//! these shapes.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use url::Url;

use chronoscope_core::GeoPoint;
use chronoscope_core::conflicts::AnyConflictReport;
use chronoscope_core::grammar::depiction::Perspective;
use chronoscope_core::grammar::image::ImageMedium;
use chronoscope_core::{listing, typed};

use crate::client::ApiError;
use crate::ids::{EntityId, EventId, ImageId};

/// The client-facing entity projection: [`typed::Entity`] at the opaque wire ids.
/// Wrapped in [`EntityDetail`] by `GET /entities/{id}`; no infrastructure
/// envelope (no `created_at`/`updated_at` — the fact store has no row-level
/// timestamps, only per-fact provenance already carried inside the typed fields).
pub type Entity = typed::Entity<EntityId, EventId, ImageId>;

/// The `GET /entities/{id}` response: the typed entity, the display name the
/// server negotiated from the request's `Accept-Language`, and the entity's own
/// conflict reports. The depicting images are the paginated
/// `GET /entities/{id}/images` sub-resource ([`EntityImagesPage`]), fetched
/// separately.
///
/// `entity.names` still carries every localized name with its provenance;
/// `display_name` is just the one the panel heading shows, chosen server-side so
/// every client agrees on it. `None` only when the entity has no name at all.
///
/// `conflicts` are the over-determined date slots the detector found in this one
/// entity's projection — each a structured report the panel renders a disputed
/// indicator from. Empty when every date slot is consistent.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(bound(
    deserialize = "E: ::serde::Deserialize<'de> + Ord + std::fmt::Debug, V: ::serde::de::DeserializeOwned, I: ::serde::de::DeserializeOwned"
))]
pub struct EntityDetail<E: Ord, V, I> {
    pub entity: typed::Entity<E, V, I>,
    pub display_name: Option<String>,
    pub conflicts: Vec<AnyConflictReport<E, V>>,
    /// The read-consistency point this projection was served at. Thread it back
    /// as `?snapshot=` on the images sub-resource so the grid reads the same
    /// state as this detail.
    pub snapshot: Snapshot,
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
pub struct DetailImage<I> {
    pub id: I,
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

/// The client-facing viewport summary: [`listing::EntitySummary`] at the opaque
/// wire ids — id, names, current marker, and timeline date span. Returned by
/// `GET /entities`.
pub type EntitySummary = listing::EntitySummary<EntityId, ImageId>;

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

/// An opaque read-consistency handle a client can pin its reads to. Every read
/// DTO echoes the point it was served at as one of these; a client threads it
/// back as `?snapshot=` so a multi-request view (entity detail plus its images)
/// reads one stable state even as the store advances. Its contents — a position
/// in the append-only log — are server-internal and never inspected
/// client-side. Distinct from [`Cursor`]: a snapshot pins *where* to read, a
/// cursor pins where a paginated walk resumes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct Snapshot(String);

impl Snapshot {
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The paging affordance the images grid shows, when it shows one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImagesButton {
    /// A transient error offers a retry of the failed fetch.
    Retry,
    /// Another page is available to append.
    LoadMore,
}

/// The images grid's button decision (`None` = hidden). A retryable error offers
/// a retry; a terminal error (a 4xx) hides the button even if a cursor lingers,
/// so an unexpected error can't loop a dead fetch; an otherwise healthy grid
/// offers "Load more" while another page remains.
pub fn images_button(has_more: bool, error: Option<&ApiError>) -> Option<ImagesButton> {
    match error {
        Some(e) if e.is_retryable() => Some(ImagesButton::Retry),
        Some(_) => None, // terminal (a 4xx): hide the button so an unexpected error can't loop a dead action
        None => has_more.then_some(ImagesButton::LoadMore),
    }
}

/// The images grid's fetch decision for a given `page_req` (`None` = don't
/// fetch). A stored cursor makes a page bump a real "Load more" (the cursor pins
/// the snapshot); with no cursor it's a retry of a failed page 1, which re-pins
/// to the detail's snapshot. Page 1 waits for that snapshot too. Every path pins
/// a snapshot, so the grid never reads live head.
pub fn images_fetch_params(
    req: u32,
    next_cursor: Option<Cursor>,
    ready_snapshot: Option<Snapshot>,
) -> Option<(Option<Cursor>, Option<Snapshot>)> {
    if req > 0 {
        match next_cursor {
            Some(c) => Some((Some(c), None)), // load-more: the cursor pins the snapshot
            None => ready_snapshot.map(|s| (None, Some(s))), // retry page-1: re-pin (skip if detail not ready)
        }
    } else {
        ready_snapshot.map(|s| (None, Some(s))) // page 1: pin (skip if detail not ready)
    }
}

/// One page of a `GET /entities` viewport listing: the summaries gathered this
/// page and the opaque [`Cursor`] for the next, `None` once the viewport is
/// exhausted.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(bound(deserialize = "E: ::serde::Deserialize<'de>, I: ::serde::de::DeserializeOwned"))]
pub struct EntityListPage<E, I> {
    pub summaries: Vec<listing::EntitySummary<E, I>>,
    pub next: Option<Cursor>,
    /// The read-consistency point this page was served at. Thread it back as
    /// `?snapshot=` to pin a follow-up read to the same state.
    pub snapshot: Snapshot,
}

/// One page of an entity's depicting images: the resolved [`DetailImage`] tiles
/// gathered this page and the opaque [`Cursor`] for the next, `None` once the
/// entity's depictions are exhausted. Returned by `GET /entities/{id}/images`;
/// the client threads `next` back verbatim to page the grid.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(bound(deserialize = "I: ::serde::de::DeserializeOwned"))]
pub struct EntityImagesPage<I> {
    pub images: Vec<DetailImage<I>>,
    pub next: Option<Cursor>,
    /// The read-consistency point this page was served at, echoing the
    /// `?snapshot=` the client pinned to (or the live point, first page
    /// unpinned).
    pub snapshot: Snapshot,
}

// ==================== Unified Markers ====================

/// A map marker for one entity (or a co-located group of entities).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Marker<E> {
    /// The entity id — for a co-located group, the group's first member
    /// (sorted by earliest date). `click_action` carries every member.
    pub id: E,
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
    pub click_action: ClickAction<E>,
}

/// What happens when a marker is clicked.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum ClickAction<E> {
    /// Open the entity detail panel.
    #[serde(rename = "select")]
    Select { entity_id: E },
    /// Show a disambiguation picker (co-located entities at the same point).
    #[serde(rename = "disambiguate")]
    Disambiguate { entries: Vec<EntityPickerEntry<E>> },
}

/// One entry in a co-located entity disambiguation picker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EntityPickerEntry<E> {
    pub id: E,
    /// The entity's display name, negotiated server-side from the request's
    /// `Accept-Language`. `None` when the entity has no name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Response for the unified markers endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MarkersResponse<E> {
    pub markers: Vec<Marker<E>>,
    /// True if results were truncated at the server limit.
    pub truncated: bool,
    /// The read-consistency point these markers were served at. Thread it back
    /// as `?snapshot=` to pin a follow-up read to the same state.
    pub snapshot: Snapshot,
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

    #[test]
    fn terminal_error_hides_button_even_with_more_pages() {
        // A terminal 4xx suppresses the button even when a stale cursor still
        // reports more pages — the reader isn't handed a fetch that can only fail.
        assert_eq!(
            images_button(
                true,
                Some(&ApiError::Api {
                    status: 404,
                    message: String::new()
                })
            ),
            None
        );
    }

    #[test]
    fn transient_error_offers_retry() {
        assert_eq!(
            images_button(
                false,
                Some(&ApiError::Api {
                    status: 503,
                    message: String::new()
                })
            ),
            Some(ImagesButton::Retry)
        );
    }

    #[test]
    fn more_pages_offers_load_more() {
        assert_eq!(images_button(true, None), Some(ImagesButton::LoadMore));
    }

    #[test]
    fn healthy_and_exhausted_shows_no_button() {
        assert_eq!(images_button(false, None), None);
    }

    #[test]
    fn retry_page_one_repins_to_snapshot() {
        // A page bump with no stored cursor is a retry of a failed page 1; it
        // re-pins to the detail's snapshot rather than reading live head.
        let snapshot = Snapshot::new("snap");
        assert_eq!(
            images_fetch_params(1, None, Some(snapshot.clone())),
            Some((None, Some(snapshot)))
        );
    }

    #[test]
    fn load_more_rides_cursor_without_snapshot() {
        // A stored cursor already pins the snapshot, so load-more passes none.
        let cursor = Cursor::new("cur");
        let snapshot = Snapshot::new("snap");
        assert_eq!(
            images_fetch_params(1, Some(cursor.clone()), Some(snapshot)),
            Some((Some(cursor), None))
        );
    }

    #[test]
    fn page_one_pins_to_snapshot() {
        let snapshot = Snapshot::new("snap");
        assert_eq!(
            images_fetch_params(0, None, Some(snapshot.clone())),
            Some((None, Some(snapshot)))
        );
    }

    #[test]
    fn page_one_before_ready_does_not_fetch() {
        assert_eq!(images_fetch_params(0, None, None), None);
    }

    #[test]
    fn retry_before_ready_never_reads_live_head() {
        // A page bump before the detail resolves has neither cursor nor snapshot;
        // it must not fetch (which would read live head), it waits.
        assert_eq!(images_fetch_params(1, None, None), None);
    }
}
