//! Lifecycle extraction from Wikidata claims.
//!
//! Extracts construction/demolition bookends and interior lifetime events from
//! Wikidata properties as fact-shaped [`Contribution`]s, each date and
//! location paired with its [`FactualCitation`] at the point of extraction.
//! Handles entity splitting when demolish→rebuild patterns indicate a new
//! entity.
//!
//! Date and location fields are `Vec`s because parallel claims (multiple
//! non-deprecated statements about one slot) each contribute a competing
//! cited bound; the conflict machinery reconciles them downstream.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use chronoscope_core::date::UncertainDate;
use chronoscope_core::external_ids::WikidataPropertyId;
use chronoscope_core::grammar::citations::FactualCitation;
use chronoscope_core::grammar::event;
use chronoscope_core::grammar::lifecycle::{
    DamageCause, DurationalKind, LifetimeEventKind, PointKind, Usage,
};
use chronoscope_core::location::{Location, UnresolvedLocation};
use chronoscope_integrations::wikidata::{Claim, PropertyId};

use crate::wikidata::{ItemContext, asserted_claims};

// =============================================================================
// CONTRIBUTION SHAPES
// =============================================================================

/// A date bound with the citation naming where it was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitedDate {
    pub bound: UncertainDate,
    pub citation: FactualCitation,
}

/// A location with the citation naming where it was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitedLocation {
    pub location: UnresolvedLocation,
    pub citation: FactualCitation,
}

/// One lifecycle contribution to a split entity: a construction bookend, a
/// demolition bookend, or an interior lifetime event. Each maps one-for-one
/// onto the facts the commit builder emits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Contribution {
    /// Construction bookend claims: start bounds, completion bounds, and
    /// build locations.
    Construction {
        started: Vec<CitedDate>,
        completed: Vec<CitedDate>,
        location: Vec<CitedLocation>,
    },
    /// Demolition bookend claims: start and completion bounds.
    Demolition {
        started: Vec<CitedDate>,
        completed: Vec<CitedDate>,
    },
    /// An interior lifetime event.
    Event(Box<InteriorEvent>),
}

/// An interior lifetime event: its kind and date bounds, an optional payload,
/// and the citation backing its `HasEvent` and payload facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteriorEvent {
    pub shape: EventShape,
    pub payload: Option<InteriorPayload>,
    /// Backs `HasEvent` and the payload: the primary date's citation, or the
    /// item citation when the event carries no date.
    pub citation: FactualCitation,
}

/// The kind and date bounds of an interior event, split along the grammar's
/// durational/point boundary so a point event can't carry a completion bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventShape {
    Durational {
        kind: DurationalKind,
        started: Vec<CitedDate>,
        completed: Vec<CitedDate>,
    },
    Point {
        kind: PointKind,
        at: Vec<CitedDate>,
    },
}

impl EventShape {
    /// The declared lifetime-event kind.
    pub fn kind(&self) -> LifetimeEventKind {
        match self {
            Self::Durational { kind, .. } => LifetimeEventKind::Durational { kind: *kind },
            Self::Point { kind, .. } => LifetimeEventKind::Point { kind: *kind },
        }
    }
}

/// A payload fact for an interior event, minus the event id minted at commit
/// build time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InteriorPayload {
    DamageCause { cause: DamageCause },
    UsageChange { new_usages: BTreeSet<Usage> },
}

impl InteriorPayload {
    /// The event-cluster fact this payload asserts about `event`.
    pub fn fact<EntId: Ord, EvtId: Ord>(&self, event: EvtId) -> event::Fact<EntId, EvtId> {
        match self {
            Self::DamageCause { cause } => event::Fact::DamageCause {
                event,
                cause: cause.clone(),
            },
            Self::UsageChange { new_usages } => event::Fact::UsageChange {
                event,
                new_usages: new_usages.clone(),
            },
        }
    }
}

// =============================================================================
// EXTRACTION COMBINATORS
// =============================================================================

mod extract {
    use chronoscope_core::date::UncertainDate;
    use chronoscope_core::geo::{GeoPoint, Meters};
    use chronoscope_core::location::{Location, UnresolvedLocation};
    use chronoscope_integrations::wikidata::{Claim, DataValue, Snak};

    use crate::wikidata::parsing::parse_wikidata_time;

    /// Extract time from claim's mainsnak. Returns (value, warnings).
    pub fn mainsnak_time(claim: &Claim) -> (Option<(UncertainDate, String)>, Vec<String>) {
        let mut warnings = Vec::new();

        let time_val = match &claim.mainsnak {
            Snak::Value(DataValue::Time(tv)) => tv,
            Snak::NoValue | Snak::SomeValue => return (None, warnings),
            Snak::Value(_) => {
                warnings.push("expected time value but got different type".to_string());
                return (None, warnings);
            }
        };

        match parse_wikidata_time(time_val.time.as_str(), time_val.precision) {
            Some(date) => (Some((date, time_val.time.to_string())), warnings),
            None => {
                warnings.push(format!("failed to parse time: {}", time_val.time));
                (None, warnings)
            }
        }
    }

    /// Extract coordinates from claim's mainsnak.
    pub fn mainsnak_coordinates(claim: &Claim) -> (Option<UnresolvedLocation>, Vec<String>) {
        let mut warnings = Vec::new();

        let coord = match &claim.mainsnak {
            Snak::Value(DataValue::GlobeCoordinate(c)) => c,
            Snak::NoValue | Snak::SomeValue => return (None, warnings),
            Snak::Value(_) => {
                warnings.push("expected coordinate value but got different type".to_string());
                return (None, warnings);
            }
        };

        // Convert coordinate precision from degrees to meters using Haversine distance.
        // This accounts for longitude convergence at higher latitudes, unlike the
        // equator-only approximation (deg * 111_000).
        let radius_m = coord.precision.map(|deg| {
            use geo::{Distance, Haversine};
            let center = geo::Point::new(coord.longitude, coord.latitude);
            let offset = geo::Point::new(coord.longitude + deg.abs(), coord.latitude);
            Haversine::distance(center, offset)
        });

        let result = GeoPoint::new(coord.latitude, coord.longitude).map(|center| match radius_m {
            Some(r) => Location::circle(center, Meters(r)),
            None => Ok(Location::point(center)),
        });

        match result {
            // Outer `Ok` is a valid center; inner `Ok`/`Err` is the radius
            // check (`point` is infallible, so its arm is always `Ok`).
            Ok(Ok(location)) => (Some(UnresolvedLocation::Resolved(location)), warnings),
            Ok(Err(e)) => {
                warnings.push(format!(
                    "invalid radius for ({}, {}): {e}",
                    coord.latitude, coord.longitude
                ));
                (None, warnings)
            }
            Err(e) => {
                warnings.push(format!(
                    "invalid coordinates ({}, {}): {e}",
                    coord.latitude, coord.longitude
                ));
                (None, warnings)
            }
        }
    }

    /// Extract Q-ID from claim's mainsnak.
    pub fn mainsnak_qid(claim: &Claim) -> (Option<String>, Vec<String>) {
        let warnings = Vec::new();

        if claim.mainsnak.is_special() {
            return (None, warnings);
        }

        (
            claim.mainsnak.entity_id().map(|id| id.to_string()),
            warnings,
        )
    }

    /// Extract all times from a qualifier property.
    pub fn qualifier_times(
        claim: &Claim,
        prop: &str,
    ) -> (Vec<(UncertainDate, String)>, Vec<String>) {
        let mut warnings = Vec::new();
        let mut results = Vec::new();

        let Some(qualifiers) = claim.qualifiers.get(prop) else {
            return (results, warnings);
        };

        for snak in qualifiers {
            let Snak::Value(DataValue::Time(time_val)) = snak else {
                warnings.push(format!("{prop}: qualifier missing time value"));
                continue;
            };

            match parse_wikidata_time(time_val.time.as_str(), time_val.precision) {
                Some(date) => results.push((date, time_val.time.to_string())),
                None => warnings.push(format!("{prop}: failed to parse {}", time_val.time)),
            }
        }

        (results, warnings)
    }
}

// =============================================================================
// PROPERTY EXTRACTION HELPERS
// =============================================================================

/// Extract every asserted time claim of a property, each cited to its
/// statement.
fn extract_property_dates(
    claims: &BTreeMap<PropertyId, Vec<Claim>>,
    property_id: WikidataPropertyId,
    ctx: &ItemContext,
    warnings: &mut Vec<String>,
) -> Vec<CitedDate> {
    let key = property_id.to_string();
    let Some(prop_claims) = claims.get(key.as_str()) else {
        return Vec::new();
    };

    let mut dates = Vec::new();
    for claim in asserted_claims(prop_claims) {
        let (time, w) = extract::mainsnak_time(claim);
        warnings.extend(w.into_iter().map(|w| format!("{property_id}: {w}")));
        let Some((bound, raw)) = time else {
            continue;
        };
        match ctx.statement_citation(property_id, raw) {
            Ok(citation) => dates.push(CitedDate { bound, citation }),
            Err(e) => warnings.push(format!("{property_id}: citation: {e}")),
        }
    }
    dates
}

/// Extract every asserted P625 coordinate claim, each cited to its statement.
fn extract_property_locations(
    claims: &BTreeMap<PropertyId, Vec<Claim>>,
    ctx: &ItemContext,
    warnings: &mut Vec<String>,
) -> Vec<CitedLocation> {
    let Some(prop_claims) = claims.get("P625") else {
        return Vec::new();
    };

    let mut locations = Vec::new();
    for claim in asserted_claims(prop_claims) {
        let (location, w) = extract::mainsnak_coordinates(claim);
        warnings.extend(w.into_iter().map(|w| format!("P625: {w}")));
        let Some(location) = location else {
            continue;
        };
        let raw = match &location {
            UnresolvedLocation::Resolved(Location::Circle { center, .. }) => {
                format!("{},{}", center.lat(), center.lon())
            }
            _ => "location".to_string(),
        };
        match ctx.statement_citation(WikidataPropertyId::new(625), raw) {
            Ok(citation) => locations.push(CitedLocation { location, citation }),
            Err(e) => warnings.push(format!("P625: citation: {e}")),
        }
    }
    locations
}

/// The citation for an interior event: its primary date's, or the item
/// citation when the event carries no date.
fn event_citation(primary: Option<&CitedDate>, ctx: &ItemContext) -> FactualCitation {
    primary.map_or_else(|| ctx.item_citation(), |d| d.citation.clone())
}

/// Earliest possible day across a set of parallel bounds — the chronological
/// sort key.
fn earliest_of(dates: &[CitedDate]) -> Option<NaiveDate> {
    dates.iter().filter_map(|d| d.bound.earliest()).min()
}

// =============================================================================
// P793 CLAIM PROCESSING
// =============================================================================

/// A contribution with its sort key for chronological ordering.
struct DatedContribution {
    contribution: Contribution,
    sort_key: Option<NaiveDate>,
}

/// Process one P793 claim into at most one contribution.
fn process_p793_claim(
    claim: &Claim,
    ctx: &ItemContext,
) -> (Option<DatedContribution>, Vec<String>) {
    let mut warnings = Vec::new();

    let (qid, w) = extract::mainsnak_qid(claim);
    warnings.extend(w);
    let Some(qid) = qid else {
        return (None, warnings);
    };

    let p793 = WikidataPropertyId::new(793);
    // Qualifier dates are cited to the P793 statement; the excerpt quotes the
    // trigger-event QID alongside the qualifier the date was read from, so the
    // trigger survives in the citation text.
    let qualifier_dates = |prop: &str, warnings: &mut Vec<String>| -> Vec<CitedDate> {
        let (dates, w) = extract::qualifier_times(claim, prop);
        warnings.extend(w);
        dates
            .into_iter()
            .filter_map(|(bound, raw)| {
                match ctx.statement_citation(p793, format!("{qid} {prop}:{raw}")) {
                    Ok(citation) => Some(CitedDate { bound, citation }),
                    Err(e) => {
                        warnings.push(format!("P793 {prop}: citation: {e}"));
                        None
                    }
                }
            })
            .collect()
    };
    let p580 = qualifier_dates("P580", &mut warnings); // start time
    let p582 = qualifier_dates("P582", &mut warnings); // end time
    let p585 = qualifier_dates("P585", &mut warnings); // point in time

    // Sort key from the earliest qualifier date, matching `earliest_of` — the
    // first in source order can be later, which would sort a construction after
    // its demolition and fabricate a spurious split.
    let sort_key = p580
        .iter()
        .chain(&p582)
        .chain(&p585)
        .filter_map(|d| d.bound.earliest())
        .min();

    let contribution: Option<Contribution> = match qid.as_str() {
        // =================================================================
        // CONSTRUCTION EVENTS
        // =================================================================

        // Q385378: construction (with start/end qualifiers)
        "Q385378" => construction(p580, fallback(p582, p585)),

        // Q27136782: start of construction
        // Q1068633: groundbreaking ceremony
        "Q27136782" | "Q1068633" => construction(p585, Vec::new()),

        // Q59913255: end of construction
        "Q59913255" => construction(Vec::new(), p585),

        // =================================================================
        // OPENING EVENTS
        // =================================================================

        // Q1417098: inauguration
        // Q3010369: opening ceremony
        // Q15051339: opening
        // A dated usage change; the source states no usage set.
        "Q1417098" | "Q3010369" | "Q15051339" => usage_changed(p585, None, ctx),

        // Q125375: consecration
        "Q125375" => usage_changed(p585, Some([Usage::Religious].into_iter().collect()), ctx),

        // =================================================================
        // REPAIR/RESTORATION EVENTS
        // =================================================================

        // Q1370468: architectural reconstruction
        // Q2478058: reconstruction
        // Q217102: restoration
        "Q1370468" | "Q2478058" | "Q217102" => durational(
            DurationalKind::Repaired,
            p580,
            fallback(p582, p585),
            None,
            ctx,
        ),

        // =================================================================
        // MODIFICATION EVENTS
        // =================================================================

        // Q2144402: renovation
        // Q19841649: expansion
        // Q1441983: redevelopment
        "Q2144402" | "Q19841649" | "Q1441983" => durational(
            DurationalKind::Modified,
            p580,
            fallback(p582, p585),
            None,
            ctx,
        ),

        // =================================================================
        // DAMAGE EVENTS
        // =================================================================

        // Q168983: conflagration (fire)
        "Q168983" => damage(DamageCause::Fire, p585, ctx),

        // Q7944: earthquake
        "Q7944" => damage(DamageCause::Earthquake, p585, ctx),

        // Q8068: flood
        "Q8068" => damage(DamageCause::Flood, p585, ctx),

        // =================================================================
        // DEMOLITION EVENTS
        // =================================================================

        // Q331483: demolition
        // Q17781833: destruction
        "Q331483" | "Q17781833" => {
            let completed = fallback(p582, p585);
            (!p580.is_empty() || !completed.is_empty()).then_some(Contribution::Demolition {
                started: p580,
                completed,
            })
        }

        // =================================================================
        // CLOSURE EVENTS
        // =================================================================

        // Q5135520: closure — ceased use, the empty usage set
        "Q5135520" => usage_changed(p585, Some(BTreeSet::new()), ctx),

        _ => {
            warnings.push(format!("P793: unrecognized event QID {qid}"));
            None
        }
    };

    (
        contribution.map(|contribution| DatedContribution {
            contribution,
            sort_key,
        }),
        warnings,
    )
}

/// The preferred bounds when any were extracted, else the alternate bounds
/// (end-time qualifiers over point-in-time, and so on).
fn fallback(preferred: Vec<CitedDate>, alternate: Vec<CitedDate>) -> Vec<CitedDate> {
    if preferred.is_empty() {
        alternate
    } else {
        preferred
    }
}

/// A construction contribution, when at least one date was extracted.
fn construction(started: Vec<CitedDate>, completed: Vec<CitedDate>) -> Option<Contribution> {
    (!started.is_empty() || !completed.is_empty()).then_some(Contribution::Construction {
        started,
        completed,
        location: Vec::new(),
    })
}

/// A durational interior event, when at least one date was extracted.
fn durational(
    kind: DurationalKind,
    started: Vec<CitedDate>,
    completed: Vec<CitedDate>,
    payload: Option<InteriorPayload>,
    ctx: &ItemContext,
) -> Option<Contribution> {
    if started.is_empty() && completed.is_empty() {
        return None;
    }
    let citation = event_citation(started.first().or(completed.first()), ctx);
    Some(Contribution::Event(Box::new(InteriorEvent {
        shape: EventShape::Durational {
            kind,
            started,
            completed,
        },
        payload,
        citation,
    })))
}

/// A damage event: durational, with the cause as payload; the damage spans
/// from the claimed point in time.
fn damage(cause: DamageCause, at: Vec<CitedDate>, ctx: &ItemContext) -> Option<Contribution> {
    durational(
        DurationalKind::Damaged,
        at,
        Vec::new(),
        Some(InteriorPayload::DamageCause { cause }),
        ctx,
    )
}

/// A usage-changed point event, when at least one date was extracted.
/// `new_usages: None` asserts the change with no claimed usage set.
fn usage_changed(
    at: Vec<CitedDate>,
    new_usages: Option<BTreeSet<Usage>>,
    ctx: &ItemContext,
) -> Option<Contribution> {
    if at.is_empty() {
        return None;
    }
    let citation = event_citation(at.first(), ctx);
    Some(Contribution::Event(Box::new(InteriorEvent {
        shape: EventShape::Point {
            kind: PointKind::UsageChanged,
            at,
        },
        payload: new_usages.map(|new_usages| InteriorPayload::UsageChange { new_usages }),
        citation,
    })))
}

// =============================================================================
// MAIN LIFECYCLE BUILDER
// =============================================================================

/// Build lifecycles from claims.
///
/// Extracts P571, P576, P625, P793, and the usage-transition properties
/// (P1619, P3999, P729, P730), fuses them into complete contributions, and
/// sorts chronologically. Returns (`entity_lifecycles`, warnings). Multiple
/// inner vecs when demolish→construct indicates entity splitting; the caller
/// creates `Replaces` relationships between them.
pub fn build_lifecycles(
    claims: &BTreeMap<PropertyId, Vec<Claim>>,
    ctx: &ItemContext,
) -> (Vec<Vec<Contribution>>, Vec<String>) {
    let mut warnings = Vec::new();
    let mut dated: Vec<DatedContribution> = Vec::new();

    // 1. Extract top-level properties
    let inceptions =
        extract_property_dates(claims, WikidataPropertyId::new(571), ctx, &mut warnings);
    let demolition_dates =
        extract_property_dates(claims, WikidataPropertyId::new(576), ctx, &mut warnings);
    let locations = extract_property_locations(claims, ctx, &mut warnings);
    let openings =
        extract_property_dates(claims, WikidataPropertyId::new(1619), ctx, &mut warnings);
    let closures =
        extract_property_dates(claims, WikidataPropertyId::new(3999), ctx, &mut warnings);
    let service_entries =
        extract_property_dates(claims, WikidataPropertyId::new(729), ctx, &mut warnings);
    let service_retirements =
        extract_property_dates(claims, WikidataPropertyId::new(730), ctx, &mut warnings);

    // 2. Process P793 events first to collect construction events
    let mut p793_constructions: Vec<DatedContribution> = Vec::new();
    let mut p793_other: Vec<DatedContribution> = Vec::new();

    if let Some(p793_claims) = claims.get("P793") {
        for claim in asserted_claims(p793_claims) {
            let (contribution, w) = process_p793_claim(claim, ctx);
            warnings.extend(w);
            if let Some(dc) = contribution {
                if matches!(dc.contribution, Contribution::Construction { .. }) {
                    p793_constructions.push(dc);
                } else {
                    p793_other.push(dc);
                }
            }
        }
    }

    // 3. Build construction contributions, fusing P571/P625 with P793:
    //    P571 inception is a competing construction *start* bound (existence
    //    onset ≈ construction start), and P625 is the build location. With no
    //    P793 construction, the inceptions stand as their own construction;
    //    otherwise they join the earliest P793 construction's start bounds.
    if p793_constructions.is_empty() {
        if !inceptions.is_empty() || !locations.is_empty() {
            let sort_key = earliest_of(&inceptions);
            dated.push(DatedContribution {
                contribution: Contribution::Construction {
                    started: inceptions,
                    completed: Vec::new(),
                    location: locations,
                },
                sort_key,
            });
        }
    } else {
        // The earliest P793 construction takes the location and absorbs the
        // P571 inceptions as competing start bounds — every non-deprecated
        // inception is asserted, never conditionally dropped.
        p793_constructions.sort_by(|a, b| a.sort_key.cmp(&b.sort_key));
        let mut remaining_locations = locations;
        let mut remaining_inceptions = inceptions;

        for (i, mut dc) in p793_constructions.into_iter().enumerate() {
            if i == 0
                && let Contribution::Construction {
                    started,
                    completed,
                    location,
                } = &mut dc.contribution
            {
                *location = std::mem::take(&mut remaining_locations);
                started.append(&mut remaining_inceptions);
                // An absorbed inception can predate the construction's own
                // start; keep the sort key equal to the earliest emitted bound.
                let earliest = started
                    .iter()
                    .chain(completed.iter())
                    .filter_map(|d| d.bound.earliest())
                    .min();
                dc.sort_key = earliest;
            }
            dated.push(dc);
        }
    }

    // 4. Add non-construction P793 events
    dated.extend(p793_other);

    // 5. Add P576 demolition
    if !demolition_dates.is_empty() {
        let sort_key = earliest_of(&demolition_dates);
        dated.push(DatedContribution {
            contribution: Contribution::Demolition {
                started: Vec::new(),
                completed: demolition_dates,
            },
            sort_key,
        });
    }

    // 6. Add usage-transition events, one per distinct claimed date. One
    //    property can carry several genuine transitions — a station reopened
    //    over decades — so each distinct date becomes its own event, and claims
    //    sharing a date merge into one, pooling their citations.
    let mut push_usage = |at: Vec<CitedDate>, new_usages: Option<BTreeSet<Usage>>| {
        let mut by_date: BTreeMap<UncertainDate, Vec<CitedDate>> = BTreeMap::new();
        for cited in at {
            by_date.entry(cited.bound.clone()).or_default().push(cited);
        }
        for group in by_date.into_values() {
            let sort_key = earliest_of(&group);
            if let Some(contribution) = usage_changed(group, new_usages.clone(), ctx) {
                dated.push(DatedContribution {
                    contribution,
                    sort_key,
                });
            }
        }
    };
    // P1619 official opening: a dated usage change with no claimed usage set.
    push_usage(openings, None);
    // P3999 official closure: ceased use.
    push_usage(closures, Some(BTreeSet::new()));
    // P729 service entry.
    push_usage(
        service_entries,
        Some([Usage::Transportation].into_iter().collect()),
    );
    // P730 service retirement: ceased use.
    push_usage(service_retirements, Some(BTreeSet::new()));

    // 7. Sort chronologically
    dated.sort_by(|a, b| a.sort_key.cmp(&b.sort_key));

    // 8. Split on demolish->construct boundaries
    let entities = split_on_rebuild(dated);

    (entities, warnings)
}

/// Split contributions into separate entities when demolish→construct
/// indicates a rebuild.
///
/// When splitting, if the construction that triggers the split has a location,
/// the predecessor gets a synthetic dateless `Construction` with that same
/// location — the previous building occupied the same site, we just don't know
/// when it was built.
fn split_on_rebuild(contributions: Vec<DatedContribution>) -> Vec<Vec<Contribution>> {
    if contributions.is_empty() {
        return vec![];
    }

    let mut entities: Vec<Vec<Contribution>> = vec![vec![]];
    let mut saw_demolition = false;

    for dc in contributions {
        let is_construction = matches!(dc.contribution, Contribution::Construction { .. });
        let is_demolition = matches!(dc.contribution, Contribution::Demolition { .. });

        // If we see construction after demolition, start a new entity.
        // Give the predecessor entity a Construction at the same location.
        if saw_demolition && is_construction {
            if let Contribution::Construction { location, .. } = &dc.contribution
                && !location.is_empty()
                && let Some(prev) = entities.last_mut()
            {
                let has_location = prev.iter().any(|c| {
                    matches!(
                        c,
                        Contribution::Construction { location, .. } if !location.is_empty()
                    )
                });
                if !has_location {
                    prev.insert(
                        0,
                        Contribution::Construction {
                            started: Vec::new(),
                            completed: Vec::new(),
                            location: location.clone(),
                        },
                    );
                }
            }
            entities.push(vec![]);
            saw_demolition = false;
        }

        // Add contribution to current entity (vec is provably non-empty)
        if let Some(last) = entities.last_mut() {
            last.push(dc.contribution);
        }

        if is_demolition {
            saw_demolition = true;
        }
    }

    // Filter out empty entities
    entities.into_iter().filter(|e| !e.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;
    use chronoscope_core::external_ids::WikidataEntityId;
    use chronoscope_core::geo::GeoPoint;
    use chronoscope_core::grammar::citations::{ExternalSource, WikidataField};
    use chronoscope_integrations::wikidata::{
        CoordinateValue, DataValue, EntityRefValue, PropertyId, Rank, Snak, TimeValue, WikidataId,
        WikidataPrecision, WikidataTimestamp,
    };

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn wikidata_id(s: &str) -> Result<WikidataId, String> {
        WikidataId::try_from(s.to_string())
    }

    fn property_id(s: &str) -> Result<PropertyId, String> {
        PropertyId::try_from(s.to_string())
    }

    fn ymd(y: i32, m: u32, d: u32) -> Option<NaiveDate> {
        NaiveDate::from_ymd_opt(y, m, d)
    }

    fn ctx() -> Result<ItemContext, Box<dyn std::error::Error>> {
        Ok(ItemContext::new(WikidataEntityId::new(12345), 100)?)
    }

    /// The statement property a citation attributes its value to.
    fn citation_property(citation: &FactualCitation) -> Result<WikidataPropertyId, String> {
        match &citation.source {
            ExternalSource::Wikidata {
                field: WikidataField::Statement { property_id },
                ..
            } => Ok(*property_id),
            other => Err(format!("expected Wikidata statement source, got {other:?}")),
        }
    }

    /// The observed value a citation quotes.
    fn citation_value(citation: &FactualCitation) -> Result<&str, String> {
        match &citation.source {
            ExternalSource::Wikidata { value, .. } => Ok(value),
            other => Err(format!("expected Wikidata source, got {other:?}")),
        }
    }

    fn time_claim(time_str: &str, precision: WikidataPrecision) -> Result<Claim, String> {
        Ok(Claim::simple(Snak::Value(DataValue::Time(TimeValue {
            time: WikidataTimestamp::try_from(time_str.to_string())?,
            precision,
        }))))
    }

    fn coordinate_claim(lat: f64, lon: f64) -> Claim {
        Claim::simple(Snak::Value(DataValue::GlobeCoordinate(CoordinateValue {
            latitude: lat,
            longitude: lon,
            precision: Some(0.0001),
        })))
    }

    fn p793_event(
        qid: &str,
        point_in_time: &str,
        precision: WikidataPrecision,
    ) -> Result<Claim, String> {
        let mut qualifiers = BTreeMap::new();
        qualifiers.insert(
            property_id("P585")?,
            vec![Snak::Value(DataValue::Time(TimeValue {
                time: WikidataTimestamp::try_from(point_in_time.to_string())?,
                precision,
            }))],
        );

        Ok(Claim {
            mainsnak: Snak::Value(DataValue::WikibaseEntityId(EntityRefValue {
                id: wikidata_id(qid)?,
            })),
            qualifiers,
            rank: Rank::Normal,
        })
    }

    fn p793_event_with_range(qid: &str, start: &str, end: &str) -> Result<Claim, String> {
        let mut qualifiers = BTreeMap::new();
        qualifiers.insert(
            property_id("P580")?,
            vec![Snak::Value(DataValue::Time(TimeValue {
                time: WikidataTimestamp::try_from(start.to_string())?,
                precision: WikidataPrecision::Year,
            }))],
        );
        qualifiers.insert(
            property_id("P582")?,
            vec![Snak::Value(DataValue::Time(TimeValue {
                time: WikidataTimestamp::try_from(end.to_string())?,
                precision: WikidataPrecision::Year,
            }))],
        );

        Ok(Claim {
            mainsnak: Snak::Value(DataValue::WikibaseEntityId(EntityRefValue {
                id: wikidata_id(qid)?,
            })),
            qualifiers,
            rank: Rank::Normal,
        })
    }

    fn claims_from(
        pairs: Vec<(&str, Vec<Claim>)>,
    ) -> Result<BTreeMap<PropertyId, Vec<Claim>>, String> {
        pairs
            .into_iter()
            .map(|(k, v)| Ok((property_id(k)?, v)))
            .collect()
    }

    #[test]
    fn test_extract_mainsnak_time() -> TestResult {
        let claim = time_claim("+1920-01-01T00:00:00Z", WikidataPrecision::Year)?;
        let (result, warnings) = extract::mainsnak_time(&claim);
        assert!(warnings.is_empty());
        assert!(result.is_some());

        let (date, raw) = result.ok_or("expected Some")?;
        assert_eq!(raw, "+1920-01-01T00:00:00Z");
        assert_eq!(date.earliest().ok_or("expected earliest")?.year(), 1920);
        Ok(())
    }

    #[test]
    fn test_extract_mainsnak_coordinates() -> TestResult {
        let claim = coordinate_claim(40.7128, -74.0060);
        let (result, warnings) = extract::mainsnak_coordinates(&claim);
        assert!(warnings.is_empty());
        let result = result.ok_or("expected Some")?;

        if let UnresolvedLocation::Resolved(Location::Circle { center, .. }) = result {
            assert!((center.lat() - 40.7128).abs() < 0.0001);
            assert!((center.lon() - (-74.0060)).abs() < 0.0001);
        } else {
            return Err("expected Resolved(Circle)".into());
        }
        Ok(())
    }

    #[test]
    fn test_coordinate_precision_accounts_for_latitude() -> TestResult {
        // At 60°N, 1 degree of longitude is ~55.8km (cos(60°) * 111km).
        // The old equator approximation would give ~111km for any latitude.
        let claim_high_lat = Claim {
            mainsnak: Snak::Value(DataValue::GlobeCoordinate(CoordinateValue {
                latitude: 60.0,
                longitude: 25.0,
                precision: Some(1.0),
            })),
            qualifiers: BTreeMap::new(),
            rank: Rank::Normal,
        };
        let (loc, warnings) = extract::mainsnak_coordinates(&claim_high_lat);
        assert!(warnings.is_empty());
        let loc = loc.ok_or("expected Some")?;

        if let UnresolvedLocation::Resolved(Location::Circle { radius, .. }) = loc {
            // At 60°N, 1 degree longitude ≈ 55,800m (not 111,000m)
            let radius_m = radius.0;
            assert!(
                radius_m < 70_000.0,
                "precision at 60°N should be well under 70km, got {radius_m}m"
            );
            assert!(
                radius_m > 40_000.0,
                "precision at 60°N should be over 40km, got {radius_m}m"
            );
        } else {
            return Err("expected Resolved(Circle)".into());
        }
        Ok(())
    }

    #[test]
    fn test_extract_qualifier_times() -> TestResult {
        let claim =
            p793_event_with_range("Q385378", "+1918-01-01T00:00:00Z", "+1920-01-01T00:00:00Z")?;

        let (p580, w1) = extract::qualifier_times(&claim, "P580");
        let (p582, w2) = extract::qualifier_times(&claim, "P582");

        assert!(w1.is_empty());
        assert!(w2.is_empty());
        assert_eq!(p580.len(), 1);
        assert_eq!(p582.len(), 1);
        assert_eq!(
            p580[0].0.earliest().ok_or("expected earliest")?.year(),
            1918
        );
        assert_eq!(
            p582[0].0.earliest().ok_or("expected earliest")?.year(),
            1920
        );
        Ok(())
    }

    #[test]
    fn split_on_rebuild_partitions_at_demolish_construct_boundary() {
        // Simulate: Constructed 1920, Demolished 1950, Constructed 1960
        let bare_construction = || Contribution::Construction {
            started: Vec::new(),
            completed: Vec::new(),
            location: Vec::new(),
        };
        let contributions = vec![
            DatedContribution {
                contribution: bare_construction(),
                sort_key: ymd(1920, 1, 1),
            },
            DatedContribution {
                contribution: Contribution::Demolition {
                    started: Vec::new(),
                    completed: Vec::new(),
                },
                sort_key: ymd(1950, 1, 1),
            },
            DatedContribution {
                contribution: bare_construction(),
                sort_key: ymd(1960, 1, 1),
            },
        ];

        let entities = split_on_rebuild(contributions);
        assert_eq!(entities.len(), 2);
        assert_eq!(entities[0].len(), 2); // Construction + Demolition
        assert_eq!(entities[1].len(), 1); // Construction
    }

    #[test]
    fn split_on_rebuild_gives_predecessor_the_successor_location() -> TestResult {
        // Predecessor has no location; successor was constructed at a known
        // location. The predecessor gets a synthetic dateless Construction
        // with the inherited location, same citation.
        let location = CitedLocation {
            location: UnresolvedLocation::Resolved(Location::point(GeoPoint::new(45.217, 12.277)?)),
            citation: ctx()?.statement_citation(WikidataPropertyId::new(625), "45.217,12.277")?,
        };
        let contributions = vec![
            DatedContribution {
                contribution: Contribution::Demolition {
                    started: Vec::new(),
                    completed: Vec::new(),
                },
                sort_key: ymd(1623, 1, 1),
            },
            DatedContribution {
                contribution: Contribution::Construction {
                    started: Vec::new(),
                    completed: Vec::new(),
                    location: vec![location.clone()],
                },
                sort_key: ymd(1633, 1, 1),
            },
        ];

        let entities = split_on_rebuild(contributions);
        assert_eq!(entities.len(), 2);

        // Predecessor: synthetic Construction (with location) + Demolition
        assert_eq!(entities[0].len(), 2);
        let Contribution::Construction {
            started,
            completed,
            location: inherited,
        } = &entities[0][0]
        else {
            return Err("predecessor should lead with a synthetic Construction".into());
        };
        assert!(
            started.is_empty() && completed.is_empty(),
            "no dates invented"
        );
        assert_eq!(inherited, &vec![location]);
        assert!(matches!(&entities[0][1], Contribution::Demolition { .. }));

        // Successor: Construction with location
        assert_eq!(entities[1].len(), 1);
        assert!(matches!(
            &entities[1][0],
            Contribution::Construction { location, .. } if !location.is_empty()
        ));

        Ok(())
    }

    // =========================================================================
    // build_lifecycles integration tests
    // =========================================================================

    /// P571 inception date only -> single Construction start bound
    #[test]
    fn build_p571_inception_only() -> TestResult {
        let claims = claims_from(vec![(
            "P571",
            vec![time_claim(
                "+1920-01-01T00:00:00Z",
                WikidataPrecision::Year,
            )?],
        )])?;

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx()?);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(lifecycles.len(), 1);

        let contributions = &lifecycles[0];
        assert_eq!(contributions.len(), 1);
        let Contribution::Construction {
            started,
            completed,
            location,
        } = &contributions[0]
        else {
            return Err("expected Construction".into());
        };
        // P571 is a construction start bound (inception = existence onset)
        assert_eq!(started.len(), 1);
        assert_eq!(
            started[0]
                .bound
                .earliest()
                .ok_or("expected earliest")?
                .year(),
            1920
        );
        assert_eq!(
            citation_property(&started[0].citation)?,
            WikidataPropertyId::new(571)
        );
        assert!(completed.is_empty());
        assert!(location.is_empty());
        Ok(())
    }

    /// P571 + P625 -> Construction with date and location
    #[test]
    fn build_p571_with_p625_location() -> TestResult {
        let claims = claims_from(vec![
            (
                "P571",
                vec![time_claim("+1889-03-31T00:00:00Z", WikidataPrecision::Day)?],
            ),
            ("P625", vec![coordinate_claim(48.8584, 2.2945)]),
        ])?;

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx()?);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(lifecycles.len(), 1);
        assert_eq!(lifecycles[0].len(), 1);

        let Contribution::Construction {
            started, location, ..
        } = &lifecycles[0][0]
        else {
            return Err("expected Construction".into());
        };
        assert_eq!(started.len(), 1);
        assert_eq!(
            citation_property(&started[0].citation)?,
            WikidataPropertyId::new(571)
        );
        assert_eq!(location.len(), 1);
        assert_eq!(
            citation_property(&location[0].citation)?,
            WikidataPropertyId::new(625)
        );
        if let UnresolvedLocation::Resolved(Location::Circle { center, .. }) = &location[0].location
        {
            assert!((center.lat() - 48.8584).abs() < 0.001);
            assert!((center.lon() - 2.2945).abs() < 0.001);
        } else {
            return Err("expected Coordinates".into());
        }
        Ok(())
    }

    /// P571 + P576 -> Construction + Demolition
    #[test]
    fn build_p571_p576_construction_and_demolition() -> TestResult {
        let claims = claims_from(vec![
            (
                "P571",
                vec![time_claim(
                    "+1900-01-01T00:00:00Z",
                    WikidataPrecision::Year,
                )?],
            ),
            (
                "P576",
                vec![time_claim(
                    "+1960-01-01T00:00:00Z",
                    WikidataPrecision::Year,
                )?],
            ),
        ])?;

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx()?);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(lifecycles.len(), 1);
        assert_eq!(lifecycles[0].len(), 2);

        // Sorted chronologically: Construction (P571) 1900, Demolition (P576) 1960
        let Contribution::Construction { started, .. } = &lifecycles[0][0] else {
            return Err("expected Construction".into());
        };
        assert_eq!(
            citation_property(&started[0].citation)?,
            WikidataPropertyId::new(571)
        );

        let Contribution::Demolition { completed, .. } = &lifecycles[0][1] else {
            return Err("expected Demolition".into());
        };
        assert_eq!(
            citation_property(&completed[0].citation)?,
            WikidataPropertyId::new(576)
        );
        Ok(())
    }

    /// P793 with Q385378 (construction) using start/end qualifiers
    #[test]
    fn build_p793_construction_with_date_range() -> TestResult {
        let claims = claims_from(vec![(
            "P793",
            vec![p793_event_with_range(
                "Q385378",
                "+1887-01-28T00:00:00Z",
                "+1889-03-31T00:00:00Z",
            )?],
        )])?;

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx()?);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(lifecycles.len(), 1);
        assert_eq!(lifecycles[0].len(), 1);

        let Contribution::Construction {
            started, completed, ..
        } = &lifecycles[0][0]
        else {
            return Err("expected Construction".into());
        };
        assert_eq!(started.len(), 1);
        assert_eq!(completed.len(), 1);
        assert_eq!(
            started[0]
                .bound
                .earliest()
                .ok_or("expected earliest")?
                .year(),
            1887
        );
        assert_eq!(
            completed[0]
                .bound
                .earliest()
                .ok_or("expected earliest")?
                .year(),
            1889
        );
        // Dates come from P580/P582 qualifiers; the honest source is
        // the P793 significant-event statement.
        assert_eq!(
            citation_property(&started[0].citation)?,
            WikidataPropertyId::new(793)
        );
        assert_eq!(
            citation_property(&completed[0].citation)?,
            WikidataPropertyId::new(793)
        );
        Ok(())
    }

    /// P793 damage events map to correct `DamageCause` payloads
    #[test]
    fn build_p793_damage_events() -> TestResult {
        let claims = claims_from(vec![(
            "P793",
            vec![
                p793_event("Q168983", "+1871-10-08T00:00:00Z", WikidataPrecision::Day)?, // fire
                p793_event("Q7944", "+1906-04-18T00:00:00Z", WikidataPrecision::Day)?, // earthquake
                p793_event("Q8068", "+1927-04-15T00:00:00Z", WikidataPrecision::Day)?, // flood
            ],
        )])?;

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx()?);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(lifecycles.len(), 1);

        let contributions = &lifecycles[0];
        assert_eq!(contributions.len(), 3);

        // Sorted chronologically: fire 1871, earthquake 1906, flood 1927
        let causes: Vec<DamageCause> = contributions
            .iter()
            .filter_map(|c| {
                if let Contribution::Event(event) = c
                    && matches!(
                        event.shape,
                        EventShape::Durational {
                            kind: DurationalKind::Damaged,
                            ..
                        }
                    )
                    && let Some(InteriorPayload::DamageCause { cause }) = &event.payload
                {
                    Some(cause.clone())
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(
            causes,
            vec![
                DamageCause::Fire,
                DamageCause::Earthquake,
                DamageCause::Flood
            ]
        );
        Ok(())
    }

    /// P793 demolish->construct pattern triggers entity splitting
    #[test]
    fn build_p793_demolish_rebuild_splits_entities() -> TestResult {
        let claims = claims_from(vec![(
            "P793",
            vec![
                p793_event("Q385378", "+1850-01-01T00:00:00Z", WikidataPrecision::Year)?, // construction
                p793_event("Q331483", "+1900-01-01T00:00:00Z", WikidataPrecision::Year)?, // demolition
                p793_event("Q385378", "+1910-01-01T00:00:00Z", WikidataPrecision::Year)?, // reconstruction
            ],
        )])?;

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx()?);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");

        // Should split into 2 entities
        assert_eq!(lifecycles.len(), 2);

        // First entity: construction + demolition
        assert_eq!(lifecycles[0].len(), 2);
        assert!(matches!(
            &lifecycles[0][0],
            Contribution::Construction { .. }
        ));
        assert!(matches!(&lifecycles[0][1], Contribution::Demolition { .. }));

        // Second entity: construction
        assert_eq!(lifecycles[1].len(), 1);
        assert!(matches!(
            &lifecycles[1][0],
            Contribution::Construction { .. }
        ));
        Ok(())
    }

    /// Combined scenario: P571 inception + P793 renovation + P576 demolition
    /// + P1619 opening. All property types integrate and sort chronologically.
    #[test]
    fn build_combined_lifecycle() -> TestResult {
        let claims = claims_from(vec![
            (
                "P571",
                vec![time_claim(
                    "+1850-01-01T00:00:00Z",
                    WikidataPrecision::Year,
                )?],
            ),
            (
                "P576",
                vec![time_claim(
                    "+1960-01-01T00:00:00Z",
                    WikidataPrecision::Year,
                )?],
            ),
            (
                "P793",
                vec![p793_event(
                    "Q2144402",
                    "+1920-01-01T00:00:00Z",
                    WikidataPrecision::Year,
                )?],
            ), // renovation
            (
                "P1619",
                vec![time_claim(
                    "+1855-06-01T00:00:00Z",
                    WikidataPrecision::Month,
                )?],
            ), // opening
        ])?;

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx()?);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(lifecycles.len(), 1);

        let contributions = &lifecycles[0];
        // Construction (1850), Opening (1855), Renovation (1920), Demolition (1960)
        assert_eq!(contributions.len(), 4);

        assert!(matches!(
            &contributions[0],
            Contribution::Construction { .. }
        ));
        assert!(matches!(
            &contributions[1],
            Contribution::Event(e) if matches!(
                e.shape,
                EventShape::Point { kind: PointKind::UsageChanged, .. }
            )
        )); // opening
        assert!(matches!(
            &contributions[2],
            Contribution::Event(e) if matches!(
                e.shape,
                EventShape::Durational { kind: DurationalKind::Modified, .. }
            )
        )); // renovation
        assert!(matches!(&contributions[3], Contribution::Demolition { .. }));
        Ok(())
    }

    /// P793 construction merges P625 location into first construction event
    #[test]
    fn build_p793_construction_inherits_p625_location() -> TestResult {
        let claims = claims_from(vec![
            ("P625", vec![coordinate_claim(51.5074, -0.1278)]),
            (
                "P793",
                vec![p793_event(
                    "Q385378",
                    "+1850-01-01T00:00:00Z",
                    WikidataPrecision::Year,
                )?],
            ),
        ])?;

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx()?);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(lifecycles.len(), 1);
        assert_eq!(lifecycles[0].len(), 1);

        let Contribution::Construction { location, .. } = &lifecycles[0][0] else {
            return Err("expected Construction".into());
        };
        assert_eq!(
            location.len(),
            1,
            "P625 should be merged into P793 construction"
        );
        if let UnresolvedLocation::Resolved(Location::Circle { center, .. }) = &location[0].location
        {
            assert!((center.lat() - 51.5074).abs() < 0.001);
            assert!((center.lon() - (-0.1278)).abs() < 0.001);
        } else {
            return Err("expected Coordinates".into());
        }
        Ok(())
    }

    /// P571 joins a P793 construction as a competing start bound, cited to
    /// P571, alongside the P793 start — it fuses, it does not stand alone.
    #[test]
    fn p571_inception_joins_p793_construction_as_competing_start() -> TestResult {
        let claims = claims_from(vec![
            (
                "P571",
                vec![time_claim(
                    "+1889-01-01T00:00:00Z",
                    WikidataPrecision::Year,
                )?],
            ),
            (
                "P793",
                vec![p793_event(
                    "Q27136782", // start of construction: only a started bound
                    "+1887-01-01T00:00:00Z",
                    WikidataPrecision::Year,
                )?],
            ),
        ])?;

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx()?);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(lifecycles.len(), 1);
        assert_eq!(
            lifecycles[0].len(),
            1,
            "P571 fuses, it does not stand alone"
        );

        let Contribution::Construction {
            started, completed, ..
        } = &lifecycles[0][0]
        else {
            return Err("expected Construction".into());
        };
        assert!(completed.is_empty(), "no completion is invented");

        // Both the P793 start (1887) and the P571 inception (1889) are asserted
        // as competing start bounds, each cited to its own property.
        let p793_start = started
            .iter()
            .find(|d| citation_property(&d.citation) == Ok(WikidataPropertyId::new(793)))
            .ok_or("expected a P793-cited start bound")?;
        assert_eq!(p793_start.bound.earliest().ok_or("earliest")?.year(), 1887);
        let p571_start = started
            .iter()
            .find(|d| citation_property(&d.citation) == Ok(WikidataPropertyId::new(571)))
            .ok_or("the P571 inception is asserted, not dropped")?;
        assert_eq!(p571_start.bound.earliest().ok_or("earliest")?.year(), 1889);
        Ok(())
    }

    /// Mirrors Notre-Dame (Q2981): a P571 founding date plus a P793 construction
    /// that already carries a completion. The inception must survive as a
    /// construction start bound rather than being dropped for lack of an empty
    /// completion slot.
    #[test]
    fn p571_inception_survives_alongside_completed_p793_construction() -> TestResult {
        let claims = claims_from(vec![
            (
                "P571",
                vec![time_claim(
                    "+1160-01-01T00:00:00Z",
                    WikidataPrecision::Year,
                )?],
            ),
            (
                "P793",
                vec![p793_event_with_range(
                    "Q385378",
                    "+1163-01-01T00:00:00Z",
                    "+1345-01-01T00:00:00Z",
                )?],
            ),
        ])?;

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx()?);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(lifecycles.len(), 1);
        assert_eq!(lifecycles[0].len(), 1, "one fused construction");

        let Contribution::Construction {
            started, completed, ..
        } = &lifecycles[0][0]
        else {
            return Err("expected Construction".into());
        };
        // The P793 completion (1345) is retained.
        assert_eq!(completed.len(), 1);
        assert_eq!(
            completed[0].bound.earliest().ok_or("earliest")?.year(),
            1345
        );
        // The P571 founding (1160) survives as a competing start bound.
        let p571_start = started
            .iter()
            .find(|d| citation_property(&d.citation) == Ok(WikidataPropertyId::new(571)))
            .ok_or("the P571 founding date is not dropped")?;
        assert_eq!(p571_start.bound.earliest().ok_or("earliest")?.year(), 1160);
        Ok(())
    }

    /// P729/P730 service entry/retirement -> transportation / ceased-use payloads
    #[test]
    fn build_service_entry_and_retirement() -> TestResult {
        let claims = claims_from(vec![
            (
                "P729",
                vec![time_claim("+1935-05-01T00:00:00Z", WikidataPrecision::Day)?],
            ),
            (
                "P730",
                vec![time_claim("+1980-09-15T00:00:00Z", WikidataPrecision::Day)?],
            ),
        ])?;

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx()?);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(lifecycles.len(), 1);
        assert_eq!(lifecycles[0].len(), 2);

        // Service entry (P729) -> Transportation usage
        let Contribution::Event(entry) = &lifecycles[0][0] else {
            return Err("expected usage event for service entry".into());
        };
        let Some(InteriorPayload::UsageChange { new_usages }) = &entry.payload else {
            return Err("service entry should carry a usage payload".into());
        };
        assert!(new_usages.contains(&Usage::Transportation));
        let EventShape::Point { at, .. } = &entry.shape else {
            return Err("usage change is a point event".into());
        };
        assert_eq!(
            citation_property(&at[0].citation)?,
            WikidataPropertyId::new(729)
        );

        // Service retirement (P730) -> empty usage set (ceased use)
        let Contribution::Event(retirement) = &lifecycles[0][1] else {
            return Err("expected usage event for service retirement".into());
        };
        let Some(InteriorPayload::UsageChange { new_usages }) = &retirement.payload else {
            return Err("service retirement should carry a usage payload".into());
        };
        assert!(
            new_usages.is_empty(),
            "retired service should have empty usage set"
        );
        let EventShape::Point { at, .. } = &retirement.shape else {
            return Err("usage change is a point event".into());
        };
        assert_eq!(
            citation_property(&at[0].citation)?,
            WikidataPropertyId::new(730)
        );
        Ok(())
    }

    /// P793 consecration -> Religious usage payload
    #[test]
    fn build_p793_consecration() -> TestResult {
        let claims = claims_from(vec![(
            "P793",
            vec![p793_event(
                "Q125375",
                "+1626-11-18T00:00:00Z",
                WikidataPrecision::Day,
            )?],
        )])?;

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx()?);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(lifecycles.len(), 1);
        assert_eq!(lifecycles[0].len(), 1);

        let Contribution::Event(event) = &lifecycles[0][0] else {
            return Err("expected usage event for consecration".into());
        };
        let Some(InteriorPayload::UsageChange { new_usages }) = &event.payload else {
            return Err("consecration should carry a usage payload".into());
        };
        assert!(new_usages.contains(&Usage::Religious));
        // P585 point-in-time qualifier; cited under the P793 statement, with
        // the trigger QID quoted in the observed value.
        let EventShape::Point { at, .. } = &event.shape else {
            return Err("usage change is a point event".into());
        };
        assert_eq!(
            citation_property(&at[0].citation)?,
            WikidataPropertyId::new(793)
        );
        assert_eq!(
            citation_value(&at[0].citation)?,
            "Q125375 P585:+1626-11-18T00:00:00Z"
        );
        Ok(())
    }

    /// P1619 official opening -> dated usage change with no payload
    #[test]
    fn opening_emits_usage_changed_without_payload() -> TestResult {
        let claims = claims_from(vec![(
            "P1619",
            vec![time_claim(
                "+1855-06-01T00:00:00Z",
                WikidataPrecision::Month,
            )?],
        )])?;

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx()?);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(lifecycles.len(), 1);
        assert_eq!(lifecycles[0].len(), 1);

        let Contribution::Event(event) = &lifecycles[0][0] else {
            return Err("expected usage event for opening".into());
        };
        assert!(
            event.payload.is_none(),
            "an opening asserts the change, not a usage set"
        );
        let EventShape::Point {
            kind: PointKind::UsageChanged,
            at,
        } = &event.shape
        else {
            return Err("opening is a UsageChanged point event".into());
        };
        assert_eq!(at.len(), 1);
        assert_eq!(
            citation_property(&at[0].citation)?,
            WikidataPropertyId::new(1619)
        );
        Ok(())
    }

    /// Deprecated claims are dropped at extraction
    #[test]
    fn deprecated_p571_claim_is_dropped() -> TestResult {
        let mut deprecated = time_claim("+1800-01-01T00:00:00Z", WikidataPrecision::Year)?;
        deprecated.rank = Rank::Deprecated;
        let claims = claims_from(vec![(
            "P571",
            vec![
                deprecated,
                time_claim("+1920-01-01T00:00:00Z", WikidataPrecision::Year)?,
            ],
        )])?;

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx()?);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(lifecycles.len(), 1);
        let Contribution::Construction { started, .. } = &lifecycles[0][0] else {
            return Err("expected Construction".into());
        };
        assert_eq!(started.len(), 1, "the deprecated bound contributes nothing");
        assert_eq!(
            started[0]
                .bound
                .earliest()
                .ok_or("expected earliest")?
                .year(),
            1920
        );
        Ok(())
    }

    #[test]
    fn deprecated_p793_claim_is_dropped() -> TestResult {
        let mut deprecated =
            p793_event("Q168983", "+1871-10-08T00:00:00Z", WikidataPrecision::Day)?;
        deprecated.rank = Rank::Deprecated;
        let claims = claims_from(vec![("P793", vec![deprecated])])?;

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx()?);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert!(lifecycles.is_empty(), "a deprecated event asserts nothing");
        Ok(())
    }

    /// Multiple non-deprecated claims all assert, as competing citations
    #[test]
    fn parallel_p571_claims_yield_competing_start_bounds() -> TestResult {
        let claims = claims_from(vec![(
            "P571",
            vec![
                time_claim("+1900-01-01T00:00:00Z", WikidataPrecision::Year)?,
                time_claim("+1905-01-01T00:00:00Z", WikidataPrecision::Year)?,
            ],
        )])?;

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx()?);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(lifecycles.len(), 1);
        assert_eq!(
            lifecycles[0].len(),
            1,
            "one construction, two parallel bounds"
        );
        let Contribution::Construction { started, .. } = &lifecycles[0][0] else {
            return Err("expected Construction".into());
        };
        let years: Vec<i32> = started
            .iter()
            .filter_map(|d| d.bound.earliest().map(|e| e.year()))
            .collect();
        assert_eq!(years, vec![1900, 1905]);
        Ok(())
    }

    /// Out-of-range coordinates produce a warning, not a crash
    #[test]
    fn build_invalid_coordinates_warns() -> TestResult {
        let claims = claims_from(vec![("P625", vec![coordinate_claim(999.0, -999.0)])])?;

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx()?);
        // Should produce a warning about invalid coordinates
        assert!(
            warnings.iter().any(|w| w.contains("invalid coordinates")),
            "expected invalid coordinates warning, got: {warnings:?}"
        );
        // No contributions since only the coordinate was provided (no P571)
        assert!(lifecycles.is_empty() || lifecycles[0].is_empty());
        Ok(())
    }

    /// Empty claims produce no contributions and no warnings
    #[test]
    fn build_empty_claims() -> TestResult {
        let claims = BTreeMap::new();
        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx()?);
        assert!(warnings.is_empty());
        assert!(lifecycles.is_empty());
        Ok(())
    }

    /// Unknown P793 event type contributes nothing but records a warning
    #[test]
    fn build_p793_unknown_event_skipped_with_warning() -> TestResult {
        let claims = claims_from(vec![(
            "P793",
            vec![p793_event(
                "Q99999999",
                "+1920-01-01T00:00:00Z",
                WikidataPrecision::Year,
            )?],
        )])?;

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx()?);
        assert!(
            warnings.iter().any(|w| w.contains("Q99999999")),
            "the unrecognized QID is named in a warning, got: {warnings:?}"
        );
        // Unknown event -> no contributions
        assert!(lifecycles.is_empty());
        Ok(())
    }

    /// A construction's sort key is the earliest qualifier date, not the first
    /// in source order — an out-of-order 1960-then-1900 pair sorts by 1900.
    #[test]
    fn construction_sort_key_is_earliest_qualifier_date_not_first() -> TestResult {
        let mut qualifiers = BTreeMap::new();
        qualifiers.insert(
            property_id("P580")?,
            vec![
                Snak::Value(DataValue::Time(TimeValue {
                    time: WikidataTimestamp::try_from("+1960-01-01T00:00:00Z".to_string())?,
                    precision: WikidataPrecision::Year,
                })),
                Snak::Value(DataValue::Time(TimeValue {
                    time: WikidataTimestamp::try_from("+1900-01-01T00:00:00Z".to_string())?,
                    precision: WikidataPrecision::Year,
                })),
            ],
        );
        let claim = Claim {
            mainsnak: Snak::Value(DataValue::WikibaseEntityId(EntityRefValue {
                id: wikidata_id("Q385378")?, // construction
            })),
            qualifiers,
            rank: Rank::Normal,
        };

        let (dated, warnings) = process_p793_claim(&claim, &ctx()?);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        let dated = dated.ok_or("construction should produce a contribution")?;
        assert_eq!(dated.sort_key, ymd(1900, 1, 1));
        Ok(())
    }
}
