//! Conversion functions from fact-store projections to API response types.
//!
//! The response types themselves live in `chronoscope_api_client::entities`.
//! This module holds the server-side logic for constructing them from the
//! fact store's `typed`/`listing` projections (orphan rule prevents
//! `impl From<CoreType> for ClientType`).

use std::cmp::Ordering;
use std::collections::HashMap;
use std::num::NonZeroUsize;

use chronoscope_api_client::{ClickAction, EntityPickerEntry, Marker};
use chronoscope_core::listing::EntitySummary;
use chronoscope_core::store::memory::{MemoryEntityId, MemoryImageId};
use chronoscope_core::typed::{self, find_by_language};
use dropshot::HttpError;

use crate::limits;

/// The page-size cap shared by `/entities` and `/markers`, as a `NonZeroUsize`.
///
/// # Errors
/// Returns an internal error if `limits::ENTITY_LIST_MAX_PAGE_SIZE` is ever
/// misconfigured to zero.
pub fn max_page_limit() -> Result<NonZeroUsize, HttpError> {
    NonZeroUsize::new(limits::ENTITY_LIST_MAX_PAGE_SIZE as usize)
        .ok_or_else(|| HttpError::for_internal_error("page limit must be nonzero".to_string()))
}

/// English is the negotiation's final fallback before the first-listed name, so
/// a header-less client (curl, a shared cache, a server-to-server call) gets the
/// English name rather than whichever name happens to sort first.
const DEFAULT_LANGUAGE: &str = "en";

/// Parse an `Accept-Language` header into primary language subtags in descending
/// preference order. Splits on `,`; each entry's primary subtag is the part
/// before `-` (so `en-US` becomes `en`), lowercased to match the
/// lowercase-canonical stored tags. Entries sort by their `;q=` weight (an
/// absent, unparseable, or non-finite weight reads as `1.0`), highest first,
/// ties keeping header order. `None` or an empty/blank header yields an empty
/// list.
fn parse_accept_language(accept_language: Option<&str>) -> Vec<String> {
    let Some(header) = accept_language else {
        return Vec::new();
    };

    /// One parsed entry: its lowercased primary subtag, its weight, and its
    /// header position for a stable tiebreak.
    struct Ranked {
        primary: String,
        weight: f64,
        position: usize,
    }

    let mut ranked: Vec<Ranked> = header
        .split(',')
        .enumerate()
        .filter_map(|(position, part)| {
            let mut segments = part.split(';');
            let primary = segments.next()?.trim().split('-').next()?;
            if primary.is_empty() {
                return None;
            }
            let weight = segments.find_map(quality_value).unwrap_or(1.0);
            Some(Ranked {
                primary: primary.to_ascii_lowercase(),
                weight,
                position,
            })
        })
        .collect();

    ranked.sort_by(|a, b| {
        b.weight
            .partial_cmp(&a.weight)
            .unwrap_or(Ordering::Equal)
            .then(a.position.cmp(&b.position))
    });
    ranked.into_iter().map(|r| r.primary).collect()
}

/// The finite `q=` weight of one `;`-delimited `Accept-Language` parameter, or
/// `None` when the parameter isn't a `q=`. A present-but-unparseable or
/// non-finite weight reads as `1.0`, matching an absent one.
fn quality_value(segment: &str) -> Option<f64> {
    let param = segment.trim();
    let value = param
        .strip_prefix("q=")
        .or_else(|| param.strip_prefix("Q="))?;
    Some(
        value
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|q| q.is_finite())
            .unwrap_or(1.0),
    )
}

/// The one display name to show for a viewer's `Accept-Language`: the first name
/// matching the highest-priority language the viewer requested, else the English
/// [`DEFAULT_LANGUAGE`], else the first name, `None` for an entity with no names.
pub fn negotiate_name(
    names: &[typed::Name<MemoryImageId>],
    accept_language: Option<&str>,
) -> Option<String> {
    negotiate_name_for_prefixes(names, &parse_accept_language(accept_language))
}

/// [`negotiate_name`] over pre-parsed language prefixes — the markers path parses
/// the header once and negotiates every entity's name against the shared list.
/// The English [`DEFAULT_LANGUAGE`] rides after the viewer's prefixes, so a
/// header-less request still lands on the English name before the first-listed
/// fallback.
fn negotiate_name_for_prefixes(
    names: &[typed::Name<MemoryImageId>],
    prefixes: &[String],
) -> Option<String> {
    prefixes
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(DEFAULT_LANGUAGE))
        .find_map(|prefix| find_by_language(names, prefix, |n| n.language.as_str()))
        .or_else(|| names.first())
        .map(|n| n.text.clone())
}

/// Group viewport summaries into markers, collapsing co-located entities (same
/// point) into one disambiguation marker. Mirrors the coordinate-bucketing the
/// SQLite-backed marker assembly used (`f64::to_bits` as the group key).
///
/// Each entity's display name is negotiated against `accept_language`, parsed
/// once here and shared across the whole viewport. Each marker also pairs with
/// its representative entity's thumbnail image id (or `None`); the handler
/// resolves that id to a URL — kept out of this step because the resolution
/// reads the fact store.
///
/// Assembling markers per-summary in memory is a stopgap for the in-memory
/// backend; a real backend paginates and indexes the viewport instead.
pub fn markers_from_summaries(
    summaries: Vec<EntitySummary<MemoryEntityId, MemoryImageId>>,
    accept_language: Option<&str>,
) -> Vec<(Marker, Option<MemoryImageId>)> {
    let prefixes = parse_accept_language(accept_language);
    let mut coord_groups: EntityGroups = HashMap::new();
    for summary in summaries {
        let key = (summary.point.lat().to_bits(), summary.point.lon().to_bits());
        coord_groups.entry(key).or_default().push(summary);
    }
    coord_groups
        .into_values()
        .map(|group| marker_from_group(group, &prefixes))
        .collect()
}

type EntityGroups = HashMap<(u64, u64), Vec<EntitySummary<MemoryEntityId, MemoryImageId>>>;

/// One coordinate group's marker: a lone entity selects directly; several
/// co-located entities disambiguate, sorted by earliest date (undated last).
/// The sorted group's first entry stands in for the marker's own position,
/// name, and thumbnail either way. Names are negotiated against `prefixes`.
/// Returns the representative's thumbnail image id beside the marker for the
/// handler to resolve.
fn marker_from_group(
    mut group: Vec<EntitySummary<MemoryEntityId, MemoryImageId>>,
    prefixes: &[String],
) -> (Marker, Option<MemoryImageId>) {
    group.sort_by_key(|e| (e.earliest.is_none(), e.earliest));

    let click_action = if let [only] = group.as_slice() {
        ClickAction::Select { entity_id: only.id }
    } else {
        let entries = group
            .iter()
            .map(|e| EntityPickerEntry {
                id: e.id,
                name: negotiate_name_for_prefixes(&e.names, prefixes),
            })
            .collect();
        ClickAction::Disambiguate { entries }
    };

    // `coord_groups` only ever holds non-empty groups — each is seeded by the
    // push that creates it — so the sorted group's first entry always exists.
    let representative = &group[0];
    let marker = Marker {
        id: representative.id,
        point: representative.point,
        name: negotiate_name_for_prefixes(&representative.names, prefixes),
        thumbnail_url: None,
        click_action,
    };
    (marker, representative.thumbnail)
}
