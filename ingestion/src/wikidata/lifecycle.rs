//! Lifecycle extraction from Wikidata claims.
//!
//! Extracts construction/demolition bookends, existence witnesses, and interior
//! lifetime events from Wikidata properties as fact-shaped [`Contribution`]s,
//! each date and location paired with its [`FactualCitation`] at the point of
//! extraction. Construction phases come from P793's start/end qualifiers; P571
//! inception dates the entity's existence. Handles entity splitting when
//! demolish→rebuild patterns indicate a new entity.
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
use chronoscope_core::grammar::ids::IdScheme;
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
    /// Existence witnesses: dates the entity is attested to have existed at,
    /// each becoming its own existence fact.
    Existence { dates: Vec<CitedDate> },
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
    pub fn fact<R: IdScheme>(&self, event: R::Event) -> event::Fact<R> {
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

        // Wikidata coordinate precision is an angular grid size in degrees. The
        // largest physical extent of that uncertainty is along the meridian, so
        // measure the radius there — on WGS84, the same ellipsoid the spatial
        // predicate uses. Step toward the equator when a poleward step would
        // leave `[-90, 90]` (a near-pole center); the meridian arc is the same
        // length either way, and an out-of-range latitude gives a NaN distance.
        let radius_m = coord.precision.map(|deg| {
            use geo::{Distance, Geodesic};
            let deg = deg.abs();
            let offset_lat = if coord.latitude + deg <= 90.0 {
                coord.latitude + deg
            } else {
                coord.latitude - deg
            };
            let center = geo::Point::new(coord.longitude, coord.latitude);
            let offset = geo::Point::new(coord.longitude, offset_lat);
            Geodesic::distance(center, offset)
        });

        let result = GeoPoint::new(coord.latitude, coord.longitude).map(|center| match radius_m {
            Some(r) => match Meters::try_new(r) {
                Ok(m) => Location::circle(center, m).map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            },
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

/// One contribution per distinct claimed date: a point event repeated on
/// several dates is several events. Shared by the P793 point events and the
/// usage-transition properties.
fn fan_out_points(
    dates: Vec<CitedDate>,
    make: impl Fn(Vec<CitedDate>) -> Option<Contribution>,
) -> Vec<DatedContribution> {
    let mut by_date: BTreeMap<UncertainDate, Vec<CitedDate>> = BTreeMap::new();
    for cited in dates {
        by_date.entry(cited.bound.clone()).or_default().push(cited);
    }
    by_date
        .into_values()
        .filter_map(|group| {
            let sort_key = earliest_of(&group);
            make(group).map(|contribution| DatedContribution {
                contribution,
                sort_key,
            })
        })
        .collect()
}

/// A range event's single optional contribution, carrying the claim-wide sort
/// key. Its several date qualifiers are competing bounds for one event.
fn range_single(
    contribution: Option<Contribution>,
    sort_key: Option<NaiveDate>,
) -> Vec<DatedContribution> {
    contribution
        .map(|contribution| DatedContribution {
            contribution,
            sort_key,
        })
        .into_iter()
        .collect()
}

/// Process one P793 claim into contributions: point events fan out to one per
/// distinct claimed date, range events yield at most one.
fn process_p793_claim(claim: &Claim, ctx: &ItemContext) -> (Vec<DatedContribution>, Vec<String>) {
    let mut warnings = Vec::new();

    let (qid, w) = extract::mainsnak_qid(claim);
    warnings.extend(w);
    let Some(qid) = qid else {
        return (Vec::new(), warnings);
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

    let contributions: Vec<DatedContribution> = match qid.as_str() {
        // =================================================================
        // CONSTRUCTION EVENTS
        // =================================================================

        // Q385378: construction (with start/end qualifiers)
        "Q385378" => range_single(construction(p580, fallback(p582, p585)), sort_key),

        // Q27136782: start of construction
        // Q1068633: groundbreaking ceremony
        "Q27136782" | "Q1068633" => range_single(construction(p585, Vec::new()), sort_key),

        // Q59913255: end of construction
        "Q59913255" => range_single(construction(Vec::new(), p585), sort_key),

        // =================================================================
        // OPENING EVENTS
        // =================================================================

        // Q1417098: inauguration
        // Q3010369: opening ceremony
        // Q15051339: opening
        // A dated usage change; the source states no usage set.
        "Q1417098" | "Q3010369" | "Q15051339" => {
            fan_out_points(p585, |group| usage_changed(group, None, ctx))
        }

        // Q125375: consecration
        "Q125375" => fan_out_points(p585, |group| {
            usage_changed(group, Some([Usage::Religious].into_iter().collect()), ctx)
        }),

        // =================================================================
        // REPAIR/RESTORATION EVENTS
        // =================================================================

        // Q1370468: architectural reconstruction
        // Q2478058: reconstruction
        // Q217102: restoration
        "Q1370468" | "Q2478058" | "Q217102" => range_single(
            durational(
                DurationalKind::Repaired,
                p580,
                fallback(p582, p585),
                None,
                ctx,
            ),
            sort_key,
        ),

        // =================================================================
        // MODIFICATION EVENTS
        // =================================================================

        // Q2144402: renovation
        // Q19841649: expansion
        // Q1441983: redevelopment
        "Q2144402" | "Q19841649" | "Q1441983" => range_single(
            durational(
                DurationalKind::Modified,
                p580,
                fallback(p582, p585),
                None,
                ctx,
            ),
            sort_key,
        ),

        // =================================================================
        // DAMAGE EVENTS
        // =================================================================

        // Q168983: conflagration (fire)
        "Q168983" => fan_out_points(p585, |group| damage(DamageCause::Fire, group, ctx)),

        // Q7944: earthquake
        "Q7944" => fan_out_points(p585, |group| damage(DamageCause::Earthquake, group, ctx)),

        // Q8068: flood
        "Q8068" => fan_out_points(p585, |group| damage(DamageCause::Flood, group, ctx)),

        // =================================================================
        // DEMOLITION EVENTS
        // =================================================================

        // Q331483: demolition
        // Q17781833: destruction
        "Q331483" | "Q17781833" => {
            let completed = fallback(p582, p585);
            let demolition =
                (!p580.is_empty() || !completed.is_empty()).then_some(Contribution::Demolition {
                    started: p580,
                    completed,
                });
            range_single(demolition, sort_key)
        }

        // =================================================================
        // CLOSURE EVENTS
        // =================================================================

        // Q5135520: closure — ceased use, the empty usage set
        "Q5135520" => fan_out_points(p585, |group| {
            usage_changed(group, Some(BTreeSet::new()), ctx)
        }),

        _ => {
            warnings.push(format!("P793: unrecognized event QID {qid}"));
            Vec::new()
        }
    };

    (contributions, warnings)
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
/// (P1619, P3999, P729, P730) into contributions, and sorts chronologically.
/// P571 becomes existence witnesses; construction bookends come from P793;
/// P625 is the build location. Returns (`entity_lifecycles`, warnings). Multiple
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
            let (contributions, w) = process_p793_claim(claim, ctx);
            warnings.extend(w);
            for dc in contributions {
                if matches!(dc.contribution, Contribution::Construction { .. }) {
                    p793_constructions.push(dc);
                } else {
                    p793_other.push(dc);
                }
            }
        }
    }

    // 3. P571 inceptions are existence witnesses — the entity provably existed
    //    at each. A witness before the construction start surfaces as a
    //    read-time contradiction.
    if !inceptions.is_empty() {
        let sort_key = earliest_of(&inceptions);
        dated.push(DatedContribution {
            contribution: Contribution::Existence { dates: inceptions },
            sort_key,
        });
    }

    // 4. Construction bookends come from P793's start/end qualifiers; P625 is the
    //    build location. It rides the earliest P793 construction, or — with no
    //    dated construction — its own location-only construction, so a placeable
    //    entity keeps its site.
    if p793_constructions.is_empty() {
        if !locations.is_empty() {
            dated.push(DatedContribution {
                contribution: Contribution::Construction {
                    started: Vec::new(),
                    completed: Vec::new(),
                    location: locations,
                },
                sort_key: None,
            });
        }
    } else {
        p793_constructions.sort_by(|a, b| a.sort_key.cmp(&b.sort_key));
        let mut remaining_locations = locations;

        for (i, mut dc) in p793_constructions.into_iter().enumerate() {
            if i == 0
                && let Contribution::Construction { location, .. } = &mut dc.contribution
            {
                *location = std::mem::take(&mut remaining_locations);
            }
            dated.push(dc);
        }
    }

    // 5. Add non-construction P793 events
    dated.extend(p793_other);

    // 6. Add P576 demolition
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

    // 7. Add usage-transition events, one per distinct claimed date. One
    //    property can carry several genuine transitions — a station reopened
    //    over decades — so each distinct date becomes its own event, and claims
    //    sharing a date merge into one, pooling their citations.
    let mut push_usage = |at: Vec<CitedDate>, new_usages: Option<BTreeSet<Usage>>| {
        dated.extend(fan_out_points(at, |group| {
            usage_changed(group, new_usages.clone(), ctx)
        }));
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

    // 8. Sort chronologically
    dated.sort_by(|a, b| a.sort_key.cmp(&b.sort_key));

    // 9. Split on demolish->construct boundaries
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
    fn near_pole_coordinate_precision_still_yields_a_finite_radius() -> TestResult {
        // A poleward meridian step from a near-pole center leaves [-90, 90]
        // (89.5° + 1° = 90.5°), where the WGS84 geodesic returns NaN and the
        // location would be dropped. The equatorward step keeps the arc finite.
        let claim_near_pole = Claim {
            mainsnak: Snak::Value(DataValue::GlobeCoordinate(CoordinateValue {
                latitude: 89.5,
                longitude: 25.0,
                precision: Some(1.0),
            })),
            qualifiers: BTreeMap::new(),
            rank: Rank::Normal,
        };
        let (loc, warnings) = extract::mainsnak_coordinates(&claim_near_pole);
        assert!(warnings.is_empty(), "a valid near-pole coord warns nothing");
        let Some(UnresolvedLocation::Resolved(Location::Circle { radius, .. })) = loc else {
            return Err("near-pole coordinate must keep a finite-radius circle".into());
        };
        assert!(
            radius.get().is_finite() && (100_000.0..120_000.0).contains(&radius.get()),
            "1° of meridian near the pole is ~111.7 km, got {}m",
            radius.get()
        );
        Ok(())
    }

    #[test]
    fn coordinate_precision_uses_meridian_extent() -> TestResult {
        // Precision is an angular grid size applied to both axes; its largest
        // physical extent is the meridian arc, ~111 km per degree at every
        // latitude. A 1° grid at 60°N therefore yields the same radius it would
        // at the equator.
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
            // Meridian arc of 1° latitude on WGS84 ≈ 110.6–111.7 km across
            // latitudes (≈ 111.4 km at 60°N), so the radius lands in this band.
            let radius_m = radius.get();
            assert!(
                radius_m > 100_000.0,
                "1° of WGS84 meridian is ~111 km, got {radius_m}m"
            );
            assert!(
                radius_m < 120_000.0,
                "1° of WGS84 meridian is ~111 km, got {radius_m}m"
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

    /// P571 inception date only -> a single existence witness, no construction.
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
        let Contribution::Existence { dates } = &contributions[0] else {
            return Err("expected Existence".into());
        };
        // P571 witnesses existence, cited to P571.
        assert_eq!(dates.len(), 1);
        assert_eq!(
            dates[0].bound.earliest().ok_or("expected earliest")?.year(),
            1920
        );
        assert_eq!(
            citation_property(&dates[0].citation)?,
            WikidataPropertyId::new(571)
        );
        Ok(())
    }

    /// P571 + P625 -> an existence witness (P571) plus a dateless construction
    /// carrying the P625 build location, since no P793 dates the construction.
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
        assert_eq!(lifecycles[0].len(), 2);

        let existence = lifecycles[0]
            .iter()
            .find_map(|c| match c {
                Contribution::Existence { dates } => Some(dates),
                _ => None,
            })
            .ok_or("expected an existence witness")?;
        assert_eq!(existence.len(), 1);
        assert_eq!(
            citation_property(&existence[0].citation)?,
            WikidataPropertyId::new(571)
        );

        let (started, completed, location) = lifecycles[0]
            .iter()
            .find_map(|c| match c {
                Contribution::Construction {
                    started,
                    completed,
                    location,
                } => Some((started, completed, location)),
                _ => None,
            })
            .ok_or("expected a location-only construction")?;
        assert!(
            started.is_empty() && completed.is_empty(),
            "no construction date is invented from P571"
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

    /// P571 + P576 -> an existence witness (P571) + a demolition (P576), sorted
    /// chronologically. No construction, since nothing dates a build phase.
    #[test]
    fn build_p571_p576_existence_and_demolition() -> TestResult {
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

        // Sorted chronologically: Existence (P571) 1900, Demolition (P576) 1960
        let Contribution::Existence { dates } = &lifecycles[0][0] else {
            return Err("expected Existence".into());
        };
        assert_eq!(
            citation_property(&dates[0].citation)?,
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

    /// One fire claim carrying two distinct point-in-time dates is two separate
    /// damage events — each dated fire is its own event, not one event with two
    /// competing bounds.
    #[test]
    fn p793_fire_on_two_dates_yields_two_damage_events() -> TestResult {
        let mut qualifiers = BTreeMap::new();
        qualifiers.insert(
            property_id("P585")?,
            vec![
                Snak::Value(DataValue::Time(TimeValue {
                    time: WikidataTimestamp::try_from("+1871-10-08T00:00:00Z".to_string())?,
                    precision: WikidataPrecision::Day,
                })),
                Snak::Value(DataValue::Time(TimeValue {
                    time: WikidataTimestamp::try_from("+1906-04-18T00:00:00Z".to_string())?,
                    precision: WikidataPrecision::Day,
                })),
            ],
        );
        let claim = Claim {
            mainsnak: Snak::Value(DataValue::WikibaseEntityId(EntityRefValue {
                id: wikidata_id("Q168983")?, // fire
            })),
            qualifiers,
            rank: Rank::Normal,
        };
        let claims = claims_from(vec![("P793", vec![claim])])?;

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx()?);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(lifecycles.len(), 1);

        let damage_starts: Vec<&Vec<CitedDate>> = lifecycles[0]
            .iter()
            .filter_map(|c| match c {
                Contribution::Event(event) => match &event.shape {
                    EventShape::Durational {
                        kind: DurationalKind::Damaged,
                        started,
                        ..
                    } => Some(started),
                    _ => None,
                },
                _ => None,
            })
            .collect();

        assert_eq!(
            damage_starts.len(),
            2,
            "each distinct fire date is its own damage event"
        );
        // Each fanned-out event carries exactly the one date it split on, and the
        // events are ordered chronologically by that date.
        let years: Vec<i32> = damage_starts
            .iter()
            .map(|started| {
                assert_eq!(started.len(), 1, "one bound per damage event");
                started
                    .first()
                    .and_then(|d| d.bound.earliest())
                    .map(|d| d.year())
                    .ok_or("expected a dated bound")
            })
            .collect::<Result<_, _>>()?;
        assert_eq!(years, vec![1871, 1906]);
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
        // Existence (1850), Opening (1855), Renovation (1920), Demolition (1960)
        assert_eq!(contributions.len(), 4);

        assert!(matches!(&contributions[0], Contribution::Existence { .. }));
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

    /// P571 and a P793 construction stand as separate contributions: the P793
    /// start dates the build, the P571 inception witnesses existence, each cited
    /// to its own property.
    #[test]
    fn p571_inception_and_p793_construction_are_separate() -> TestResult {
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
        assert_eq!(lifecycles[0].len(), 2);

        // The P793 construction start (1887), cited to P793, dates the build.
        let (started, completed) = lifecycles[0]
            .iter()
            .find_map(|c| match c {
                Contribution::Construction {
                    started, completed, ..
                } => Some((started, completed)),
                _ => None,
            })
            .ok_or("expected a construction")?;
        assert!(completed.is_empty(), "no completion is invented");
        assert_eq!(started.len(), 1);
        assert_eq!(
            citation_property(&started[0].citation)?,
            WikidataPropertyId::new(793)
        );
        assert_eq!(started[0].bound.earliest().ok_or("earliest")?.year(), 1887);

        // The P571 inception (1889) survives as an existence witness, cited to P571.
        let dates = lifecycles[0]
            .iter()
            .find_map(|c| match c {
                Contribution::Existence { dates } => Some(dates),
                _ => None,
            })
            .ok_or("the P571 inception is asserted, not dropped")?;
        assert_eq!(dates.len(), 1);
        assert_eq!(
            citation_property(&dates[0].citation)?,
            WikidataPropertyId::new(571)
        );
        assert_eq!(dates[0].bound.earliest().ok_or("earliest")?.year(), 1889);
        Ok(())
    }

    /// Mirrors Notre-Dame (Q2981): a P571 founding of 1160 plus a P793
    /// construction (1163–1345). The founding is an existence witness before the
    /// build start — the read-time conflict the solver surfaces.
    #[test]
    fn p571_founding_witnesses_existence_before_p793_construction() -> TestResult {
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
        assert_eq!(lifecycles[0].len(), 2);

        // The P793 construction dates the build: 1163 start through 1345 completion.
        let (started, completed) = lifecycles[0]
            .iter()
            .find_map(|c| match c {
                Contribution::Construction {
                    started, completed, ..
                } => Some((started, completed)),
                _ => None,
            })
            .ok_or("expected a construction")?;
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].bound.earliest().ok_or("earliest")?.year(), 1163);
        assert_eq!(completed.len(), 1);
        assert_eq!(
            completed[0].bound.earliest().ok_or("earliest")?.year(),
            1345
        );

        // The P571 founding (1160) witnesses existence before the build start.
        let dates = lifecycles[0]
            .iter()
            .find_map(|c| match c {
                Contribution::Existence { dates } => Some(dates),
                _ => None,
            })
            .ok_or("the P571 founding is not dropped")?;
        assert_eq!(dates.len(), 1);
        assert_eq!(
            citation_property(&dates[0].citation)?,
            WikidataPropertyId::new(571)
        );
        assert_eq!(dates[0].bound.earliest().ok_or("earliest")?.year(), 1160);
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
        let Contribution::Existence { dates } = &lifecycles[0][0] else {
            return Err("expected Existence".into());
        };
        assert_eq!(dates.len(), 1, "the deprecated bound contributes nothing");
        assert_eq!(
            dates[0].bound.earliest().ok_or("expected earliest")?.year(),
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

    /// Multiple non-deprecated P571 claims each witness existence, pooled in one
    /// existence contribution.
    #[test]
    fn parallel_p571_claims_yield_multiple_existence_witnesses() -> TestResult {
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
        assert_eq!(lifecycles[0].len(), 1, "one existence contribution");
        let Contribution::Existence { dates } = &lifecycles[0][0] else {
            return Err("expected Existence".into());
        };
        let years: Vec<i32> = dates
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
        assert_eq!(dated.len(), 1, "construction yields one contribution");
        let sort_key = dated.first().ok_or("expected a contribution")?.sort_key;
        assert_eq!(sort_key, ymd(1900, 1, 1));
        Ok(())
    }
}
