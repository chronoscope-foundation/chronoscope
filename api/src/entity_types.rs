//! Conversion functions from fact-store projections to API response types.
//!
//! The response types themselves live in `chronoscope_api_client::entities`.
//! This module holds the server-side logic for constructing them from the
//! fact store's `typed`/`listing` projections (orphan rule prevents
//! `impl From<CoreType> for ClientType`).

use std::collections::HashMap;
use std::num::NonZeroUsize;

use chronoscope_api_client::{ClickAction, EntityPickerEntry, Marker};
use chronoscope_core::claimed::Claimed;
use chronoscope_core::facts::depiction::Perspective;
use chronoscope_core::facts::image::ImageMedium;
use chronoscope_core::facts::listing::EntitySummary;
use chronoscope_core::facts::memory::{MemoryEntityId, MemoryImageId};
use chronoscope_core::facts::typed;
use chronoscope_core::geo::{self, GeoPoint};
use dropshot::HttpError;

use crate::limits;

/// Convert a request-supplied bbox to the fact store's own `Bbox`.
///
/// `chronoscope_api_client::Bbox` already validated coordinate ranges and
/// finiteness (and admits an antimeridian-crossing `min_lon > max_lon` box),
/// and `chronoscope_core::geo::Bbox` is itself antimeridian-aware — it rejects
/// only inverted latitude, which the client bbox already precludes. Both steps
/// keep their fallible signatures, threaded through via `?` rather than assumed
/// away; any failure surfaces to the caller as a 400.
pub fn to_core_bbox(bbox: &chronoscope_api_client::Bbox) -> Result<geo::Bbox, String> {
    let sw =
        GeoPoint::new(bbox.min_lat(), bbox.min_lon()).map_err(|e| format!("sw corner: {e}"))?;
    let ne =
        GeoPoint::new(bbox.max_lat(), bbox.max_lon()).map_err(|e| format!("ne corner: {e}"))?;
    geo::Bbox::new(sw, ne).map_err(|e| e.to_string())
}

/// The page-size cap shared by `/entities` and `/markers`, as a `NonZeroUsize`.
///
/// # Errors
/// Returns an internal error if `limits::ENTITY_LIST_MAX_PAGE_SIZE` is ever
/// misconfigured to zero.
pub fn max_page_limit() -> Result<NonZeroUsize, HttpError> {
    NonZeroUsize::new(limits::ENTITY_LIST_MAX_PAGE_SIZE as usize)
        .ok_or_else(|| HttpError::for_internal_error("page limit must be nonzero".to_string()))
}

/// A short grid caption for a detail image, from the depiction's perspective
/// and the image's medium — e.g. "Exterior picture", "Map". Always non-empty
/// and free of the word "view"; the web appends " view" for the aria-label.
pub fn image_label(
    perspective: &typed::Bounded<Claimed<Perspective>, MemoryImageId>,
    medium: &typed::Bounded<Claimed<ImageMedium>, MemoryImageId>,
) -> String {
    // (lowercase, capitalized) forms of the medium noun: the lowercase reads as
    // the tail of "Exterior picture", the capitalized stands alone.
    let noun = match medium.settled() {
        Some(&ImageMedium::Picture) => ("picture", "Picture"),
        Some(&ImageMedium::Map) => ("map", "Map"),
        Some(&ImageMedium::PictorialMap) => ("pictorial map", "Pictorial map"),
        None => ("image", "Image"),
    };
    match perspective.settled() {
        Some(&Perspective::Exterior) => format!("Exterior {}", noun.0),
        Some(&Perspective::Interior) => format!("Interior {}", noun.0),
        None => noun.1.to_string(),
    }
}

/// Group viewport summaries into markers, collapsing co-located entities (same
/// point) into one disambiguation marker. Mirrors the coordinate-bucketing the
/// SQLite-backed marker assembly used (`f64::to_bits` as the group key).
///
/// Each marker pairs with its representative entity's thumbnail image id (or
/// `None`); the handler resolves that id to a URL — kept out of this pure
/// grouping step because the resolution reads the fact store.
///
/// Assembling markers per-summary in memory is a stopgap for the in-memory
/// backend; a real backend paginates and indexes the viewport instead.
pub fn markers_from_summaries(
    summaries: Vec<EntitySummary<MemoryEntityId, MemoryImageId>>,
) -> Vec<(Marker, Option<MemoryImageId>)> {
    let mut coord_groups: EntityGroups = HashMap::new();
    for summary in summaries {
        let key = (summary.point.lat().to_bits(), summary.point.lon().to_bits());
        coord_groups.entry(key).or_default().push(summary);
    }
    coord_groups.into_values().map(marker_from_group).collect()
}

type EntityGroups = HashMap<(u64, u64), Vec<EntitySummary<MemoryEntityId, MemoryImageId>>>;

/// One coordinate group's marker: a lone entity selects directly; several
/// co-located entities disambiguate, sorted by earliest date (undated last).
/// The sorted group's first entry stands in for the marker's own position,
/// label, and thumbnail either way. Returns the representative's thumbnail
/// image id beside the marker for the handler to resolve.
fn marker_from_group(
    mut group: Vec<EntitySummary<MemoryEntityId, MemoryImageId>>,
) -> (Marker, Option<MemoryImageId>) {
    group.sort_by_key(|e| (e.earliest.is_none(), e.earliest));

    let click_action = if let [only] = group.as_slice() {
        ClickAction::Select { entity_id: only.id }
    } else {
        let entries = group
            .iter()
            .map(|e| EntityPickerEntry {
                id: e.id,
                name: typed::best_name(&e.names, "en").map(|n| n.text.clone()),
            })
            .collect();
        ClickAction::Disambiguate { entries }
    };

    // `coord_groups` only ever holds non-empty groups — each is seeded by the
    // push that creates it — so the sorted group's first entry always exists.
    let representative = &group[0];
    let marker = Marker {
        id: representative.id,
        latitude: representative.point.lat(),
        longitude: representative.point.lon(),
        label: typed::best_name(&representative.names, "en").map(|n| n.text.clone()),
        thumbnail_url: None,
        click_action,
    };
    (marker, representative.thumbnail)
}
