//! Backend-shared viewport membership: the gate-decode-refine step both the
//! SQLite and Postgres `InViewport` reads run over their candidate rows.
//!
//! Each backend's SQL pre-filter is a conservative superset over the stored
//! covering rects, so whether a candidate is really in the viewport is decided
//! here, on core's [`known_geometry_intersects`]. Every spatial fixture clears
//! its viewport by kilometers, so a divergence between the backends would live
//! at the rim where conformance is thinnest; one copy of the decision is what
//! keeps that from happening. No `sqlx` and no connection: backends hand over
//! the rows and the batched retractor closure.
//!
//! [`known_geometry_intersects`]: chronoscope_core::location::UnresolvedLocation::known_geometry_intersects

use std::collections::BTreeMap;

use chronoscope_core::geo::Viewport;
use chronoscope_core::grammar::ids::FactId;
use chronoscope_core::store::retraction::{RetractionEdges, effective_retractor};
use chronoscope_core::submit::StoredFact;

use super::convert::{IdConvertError, i64_to_u64};
use super::error::CodecError;
use super::ids::SqlIds;
use super::storage::fact_from_json;

/// The fetched rows as one ascending candidate per fact. A fact arrives more
/// than once whenever its region stores several covering rects (one per `OneOf`
/// member, a pair across the ±180 seam) or a seam-crossing viewport queries both
/// of its halves. `context` names the batch for the id-conversion error.
pub(crate) fn dedup_by_fact(
    rows: impl IntoIterator<Item = (i64, String)>,
    context: &'static str,
) -> Result<Vec<(FactId, String)>, IdConvertError> {
    rows.into_iter()
        .map(|(fact_id, fact_json)| Ok((FactId::new(i64_to_u64(fact_id, context)?), fact_json)))
        .collect::<Result<BTreeMap<_, _>, IdConvertError>>()
        .map(|by_fact| by_fact.into_iter().collect())
}

/// The candidates that survive as viewport members at `snapshot`: live under
/// the batched retractor closure, location-bearing once decoded, and meeting the
/// viewport on core's predicate.
///
/// Each row carries its own fact id, so the liveness gate can never slip onto a
/// neighbour's json.
pub(crate) fn refine_in_viewport(
    candidates: &[(FactId, String)],
    snapshot: FactId,
    retraction: &RetractionEdges,
    viewport: &Viewport,
) -> Result<Vec<(FactId, StoredFact<SqlIds>)>, CodecError> {
    let mut located = Vec::new();
    for (fact_id, fact_json) in candidates {
        if effective_retractor(*fact_id, snapshot, retraction).is_some() {
            continue;
        }
        let fact = fact_from_json(fact_json)?;
        let Some((location, _)) = fact.located_subject() else {
            continue;
        };
        if !location.known_geometry_intersects(viewport) {
            continue;
        }
        located.push((*fact_id, fact));
    }
    Ok(located)
}
