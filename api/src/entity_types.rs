//! Conversion functions from fact-store projections to API response types.
//!
//! The response types themselves live in `chronoscope_api_client::entities`.
//! This module holds the server-side logic for constructing them from the
//! fact store's `typed`/`listing` projections (orphan rule prevents
//! `impl From<CoreType> for ClientType`).

use std::cmp::Ordering;

use chronoscope_core::typed::{self, find_by_language};

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
pub fn parse_accept_language(accept_language: Option<&str>) -> Vec<String> {
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
/// `DEFAULT_LANGUAGE`, else the first name, `None` for an entity with no names.
pub fn negotiate_name<I>(
    names: &[typed::Name<I>],
    accept_language: Option<&str>,
) -> Option<String> {
    negotiate_name_for_prefixes(names, &parse_accept_language(accept_language))
}

/// [`negotiate_name`] over pre-parsed language prefixes — the tiles path parses
/// the header once and negotiates every cell entity's name against the shared
/// list. The English `DEFAULT_LANGUAGE` rides after the viewer's prefixes, so a
/// header-less request still lands on the English name before the first-listed
/// fallback.
pub fn negotiate_name_for_prefixes<I>(
    names: &[typed::Name<I>],
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
