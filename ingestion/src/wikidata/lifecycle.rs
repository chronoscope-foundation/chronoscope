//! Lifecycle extraction from Wikidata claims.
//!
//! Extracts entity lifecycle transitions (construction, demolition, modifications, etc.)
//! from Wikidata properties. Handles entity splitting when demolish->rebuild patterns
//! indicate a new entity.

use std::collections::HashMap;

use chrono::NaiveDateTime;
use chronoscope_core::{
    Cited, DamageCause, EntityTransition, TriggerEventId, UncertainDate, UncertainLocation, Usage,
};
use chronoscope_integrations::wikidata::{Claim, PropertyId};

use crate::wikidata::ingest::PropertyContext;

// =============================================================================
// EXTRACTION COMBINATORS
// =============================================================================

mod extract {
    use chronoscope_core::{UncertainDate, UncertainLocation};
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
    pub fn mainsnak_coordinates(claim: &Claim) -> (Option<UncertainLocation>, Vec<String>) {
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
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let precision_m = coord.precision.map(|deg| {
            use geo::{Distance, Haversine};
            let center = geo::Point::new(coord.longitude, coord.latitude);
            let offset = geo::Point::new(coord.longitude + deg.abs(), coord.latitude);
            Haversine::distance(center, offset) as u32
        });

        match UncertainLocation::coordinates(coord.latitude, coord.longitude, None, precision_m) {
            Ok(location) => (Some(location), warnings),
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

    /// Take first claim from array, warn if multiple where one expected.
    pub fn first_claim<'a>(claims: &'a [Claim], prop: &str) -> (Option<&'a Claim>, Vec<String>) {
        let mut warnings = Vec::new();

        if claims.len() > 1 {
            warnings.push(format!(
                "{}: expected single value, got {} (using first)",
                prop,
                claims.len()
            ));
        }

        (claims.first(), warnings)
    }
}

// =============================================================================
// CITATION HELPERS
// =============================================================================

/// Cite the first date from a vec.
fn cite_first(
    dates: &[(UncertainDate, String)],
    prop: &str,
    ctx: &PropertyContext<'_>,
) -> Option<Cited<UncertainDate>> {
    dates
        .first()
        .map(|(date, raw)| ctx.cited(format!("{prop}:{raw}"), date.clone()))
}

/// Cite all dates from a vec.
fn cite_all(
    dates: &[(UncertainDate, String)],
    prop: &str,
    ctx: &PropertyContext<'_>,
) -> Vec<Cited<UncertainDate>> {
    dates
        .iter()
        .map(|(date, raw)| ctx.cited(format!("{prop}:{raw}"), date.clone()))
        .collect()
}

// =============================================================================
// PROPERTY EXTRACTION HELPERS
// =============================================================================

/// Extract single time from a property's claims.
fn extract_property_time(
    claims: &HashMap<PropertyId, Vec<Claim>>,
    prop: &str,
    ctx: &PropertyContext<'_>,
) -> (Option<Cited<UncertainDate>>, Vec<String>) {
    let mut warnings = Vec::new();

    let Some(prop_claims) = claims.get(prop) else {
        return (None, warnings);
    };

    let (claim, w) = extract::first_claim(prop_claims, prop);
    warnings.extend(w);

    let Some(claim) = claim else {
        return (None, warnings);
    };

    let (time, w) = extract::mainsnak_time(claim);
    warnings.extend(w);

    let cited = time.map(|(date, raw)| ctx.cited(raw, date));
    (cited, warnings)
}

/// Extract location from P625.
fn extract_property_location(
    claims: &HashMap<PropertyId, Vec<Claim>>,
    ctx: &PropertyContext<'_>,
) -> (Option<Cited<UncertainLocation>>, Vec<String>) {
    let mut warnings = Vec::new();

    let Some(prop_claims) = claims.get("P625") else {
        return (None, warnings);
    };

    let (claim, w) = extract::first_claim(prop_claims, "P625");
    warnings.extend(w);

    let Some(claim) = claim else {
        return (None, warnings);
    };

    let (location, w) = extract::mainsnak_coordinates(claim);
    warnings.extend(w);

    let cited = location.map(|loc| {
        // mainsnak_coordinates always returns Coordinates variant
        let raw = if let UncertainLocation::Coordinates { lat, lon, .. } = &loc {
            format!("{lat},{lon}")
        } else {
            "location".to_string()
        };
        ctx.cited(raw, loc)
    });

    (cited, warnings)
}

// =============================================================================
// P793 CLAIM PROCESSING
// =============================================================================

/// A transition with its sort key for chronological ordering.
struct DatedTransition {
    transition: EntityTransition,
    sort_key: Option<NaiveDateTime>,
}

/// Process one P793 claim. May return multiple transitions for point-in-time events.
fn process_p793_claim(
    claim: &Claim,
    ctx: &PropertyContext<'_>,
) -> (Vec<DatedTransition>, Vec<String>) {
    let mut warnings = Vec::new();

    // Extract Q-ID
    let (qid, w) = extract::mainsnak_qid(claim);
    warnings.extend(w);
    let Some(qid) = qid else {
        return (vec![], warnings);
    };

    // Extract qualifier dates
    let (p580, w) = extract::qualifier_times(claim, "P580"); // start time
    warnings.extend(w);
    let (p582, w) = extract::qualifier_times(claim, "P582"); // end time
    warnings.extend(w);
    let (p585, w) = extract::qualifier_times(claim, "P585"); // point in time
    warnings.extend(w);

    // Warn about multiple values
    if p580.len() > 1 {
        warnings.push("P793: multiple P580 (start) dates, using first".to_string());
    }
    if p582.len() > 1 {
        warnings.push("P793: multiple P582 (end) dates, using first".to_string());
    }

    let trigger = TriggerEventId(qid.clone());

    // Compute sort key from available dates
    let sort_key = p580
        .first()
        .or(p582.first())
        .or(p585.first())
        .map(|(d, _)| d.earliest());

    let transitions: Vec<EntityTransition> = match qid.as_str() {
        // =================================================================
        // CONSTRUCTION EVENTS
        // =================================================================

        // Q385378: construction (with start/end qualifiers)
        "Q385378" => {
            let started = cite_first(&p580, "P580", ctx);
            let completed =
                cite_first(&p582, "P582", ctx).or_else(|| cite_first(&p585, "P585", ctx));

            if started.is_some() || completed.is_some() {
                vec![EntityTransition::Constructed {
                    started_at: started,
                    completed_at: completed,
                    location: None,
                    trigger_event: Some(trigger),
                }]
            } else {
                vec![]
            }
        }

        // Q27136782: start of construction
        // Q1068633: groundbreaking ceremony
        "Q27136782" | "Q1068633" => cite_all(&p585, "P585", ctx)
            .into_iter()
            .map(|d| EntityTransition::Constructed {
                started_at: Some(d),
                completed_at: None,
                location: None,
                trigger_event: Some(trigger.clone()),
            })
            .collect(),

        // Q59913255: end of construction
        "Q59913255" => cite_all(&p585, "P585", ctx)
            .into_iter()
            .map(|d| EntityTransition::Constructed {
                started_at: None,
                completed_at: Some(d),
                location: None,
                trigger_event: Some(trigger.clone()),
            })
            .collect(),

        // =================================================================
        // OPENING EVENTS
        // =================================================================

        // Q1417098: inauguration
        // Q3010369: opening ceremony
        // Q15051339: opening
        "Q1417098" | "Q3010369" | "Q15051339" => cite_all(&p585, "P585", ctx)
            .into_iter()
            .map(|d| EntityTransition::UsageModified {
                occurred_at: Some(d),
                new_usages: [Usage::Unknown].into_iter().collect(),
                description: Some("Opening".to_string()),
                trigger_event: Some(trigger.clone()),
            })
            .collect(),

        // Q125375: consecration
        "Q125375" => cite_all(&p585, "P585", ctx)
            .into_iter()
            .map(|d| EntityTransition::UsageModified {
                occurred_at: Some(d),
                new_usages: [Usage::Religious].into_iter().collect(),
                description: Some("Consecration".to_string()),
                trigger_event: Some(trigger.clone()),
            })
            .collect(),

        // =================================================================
        // REPAIR/RESTORATION EVENTS
        // =================================================================

        // Q1370468: architectural reconstruction
        // Q2478058: reconstruction
        // Q217102: restoration
        "Q1370468" | "Q2478058" | "Q217102" => {
            let started = cite_first(&p580, "P580", ctx);
            let completed =
                cite_first(&p582, "P582", ctx).or_else(|| cite_first(&p585, "P585", ctx));

            if started.is_some() || completed.is_some() {
                vec![EntityTransition::Repaired {
                    started_at: started,
                    completed_at: completed,
                    description: None,
                    trigger_event: Some(trigger),
                }]
            } else {
                vec![]
            }
        }

        // =================================================================
        // MODIFICATION EVENTS
        // =================================================================

        // Q2144402: renovation
        // Q19841649: expansion
        // Q1441983: redevelopment
        "Q2144402" | "Q19841649" | "Q1441983" => {
            let started = cite_first(&p580, "P580", ctx);
            let completed =
                cite_first(&p582, "P582", ctx).or_else(|| cite_first(&p585, "P585", ctx));

            if started.is_some() || completed.is_some() {
                vec![EntityTransition::Modified {
                    started_at: started,
                    completed_at: completed,
                    description: None,
                    trigger_event: Some(trigger),
                }]
            } else {
                vec![]
            }
        }

        // =================================================================
        // DAMAGE EVENTS
        // =================================================================

        // Q168983: conflagration (fire)
        "Q168983" => cite_all(&p585, "P585", ctx)
            .into_iter()
            .map(|d| EntityTransition::Damaged {
                occurred_at: Some(d),
                cause: Some(DamageCause::Fire),
                description: None,
                trigger_event: Some(trigger.clone()),
            })
            .collect(),

        // Q7944: earthquake
        "Q7944" => cite_all(&p585, "P585", ctx)
            .into_iter()
            .map(|d| EntityTransition::Damaged {
                occurred_at: Some(d),
                cause: Some(DamageCause::Earthquake),
                description: None,
                trigger_event: Some(trigger.clone()),
            })
            .collect(),

        // Q8068: flood
        "Q8068" => cite_all(&p585, "P585", ctx)
            .into_iter()
            .map(|d| EntityTransition::Damaged {
                occurred_at: Some(d),
                cause: Some(DamageCause::Flood),
                description: None,
                trigger_event: Some(trigger.clone()),
            })
            .collect(),

        // =================================================================
        // DEMOLITION EVENTS
        // =================================================================

        // Q331483: demolition
        // Q17781833: destruction
        "Q331483" | "Q17781833" => {
            let started = cite_first(&p580, "P580", ctx);
            let completed =
                cite_first(&p582, "P582", ctx).or_else(|| cite_first(&p585, "P585", ctx));

            if started.is_some() || completed.is_some() {
                vec![EntityTransition::Demolished {
                    started_at: started,
                    completed_at: completed,
                    cause: None,
                    trigger_event: Some(trigger),
                }]
            } else {
                vec![]
            }
        }

        // =================================================================
        // CLOSURE EVENTS
        // =================================================================

        // Q5135520: closure
        "Q5135520" => cite_all(&p585, "P585", ctx)
            .into_iter()
            .map(|d| EntityTransition::UsageModified {
                occurred_at: Some(d),
                new_usages: std::collections::BTreeSet::new(), // Empty = closed
                description: Some("Closure".to_string()),
                trigger_event: Some(trigger.clone()),
            })
            .collect(),

        // Unknown event type - skip silently
        _ => vec![],
    };

    let dated = transitions
        .into_iter()
        .map(|t| DatedTransition {
            transition: t,
            sort_key,
        })
        .collect();

    (dated, warnings)
}

// =============================================================================
// MAIN LIFECYCLE BUILDER
// =============================================================================

/// Build lifecycles from claims.
///
/// Extracts P571, P576, P625, P793, P1619 and combines into complete transitions.
/// Returns (`entity_lifecycles`, warnings). Multiple inner vecs when demolish->construct
/// indicates entity splitting. Caller creates `EntityRelation::Replaces` between them.
pub fn build_lifecycles(
    claims: &HashMap<PropertyId, Vec<Claim>>,
    ctx: &PropertyContext<'_>,
) -> (Vec<Vec<EntityTransition>>, Vec<String>) {
    let mut warnings = Vec::new();
    let mut dated_transitions: Vec<DatedTransition> = Vec::new();

    // 1. Extract top-level properties
    let (inception, w) = extract_property_time(claims, "P571", ctx);
    warnings.extend(w);

    let (demolished_date, w) = extract_property_time(claims, "P576", ctx);
    warnings.extend(w);

    let (location, w) = extract_property_location(claims, ctx);
    warnings.extend(w);

    let (opening, w) = extract_property_time(claims, "P1619", ctx);
    warnings.extend(w);

    let (closure, w) = extract_property_time(claims, "P3999", ctx);
    warnings.extend(w);

    let (service_entry, w) = extract_property_time(claims, "P729", ctx);
    warnings.extend(w);

    let (service_retirement, w) = extract_property_time(claims, "P730", ctx);
    warnings.extend(w);

    // 2. Process P793 events first to collect construction events
    let mut p793_constructions: Vec<DatedTransition> = Vec::new();
    let mut p793_other: Vec<DatedTransition> = Vec::new();

    if let Some(p793_claims) = claims.get("P793") {
        for claim in p793_claims {
            let (transitions, w) = process_p793_claim(claim, ctx);
            warnings.extend(w);
            for dt in transitions {
                if matches!(dt.transition, EntityTransition::Constructed { .. }) {
                    p793_constructions.push(dt);
                } else {
                    p793_other.push(dt);
                }
            }
        }
    }

    // 3. Build Constructed transitions, merging P571/P625 with P793 when appropriate
    if p793_constructions.is_empty() {
        // No P793 construction events - create from P571 + P625
        if inception.is_some() || location.is_some() {
            let sort_key = inception.as_ref().map(|c| c.value.earliest());
            dated_transitions.push(DatedTransition {
                transition: EntityTransition::Constructed {
                    started_at: None,
                    completed_at: inception,
                    location,
                    trigger_event: None,
                },
                sort_key,
            });
        }
    } else {
        // Have P793 construction events - merge P625 location into the first one
        // and P571 inception as completed_at if appropriate
        let mut used_location = false;
        let mut used_inception = false;

        // Sort P793 constructions by date to find the earliest
        p793_constructions.sort_by(|a, b| a.sort_key.cmp(&b.sort_key));

        for (i, mut dt) in p793_constructions.into_iter().enumerate() {
            if let EntityTransition::Constructed {
                location: ref mut loc_field,
                completed_at: ref mut comp_field,
                ..
            } = dt.transition
            {
                // Attach location to the first construction event
                if i == 0
                    && !used_location
                    && let Some(loc) = location.clone()
                {
                    *loc_field = Some(loc);
                    used_location = true;
                }

                // If this construction has no completed_at and we have P571, use it
                // (only for the last construction, which is likely the final completion)
                if comp_field.is_none()
                    && !used_inception
                    && let Some(inc) = inception.clone()
                {
                    *comp_field = Some(inc);
                    used_inception = true;
                }
            }
            dated_transitions.push(dt);
        }
    }

    // 4. Add non-construction P793 events
    dated_transitions.extend(p793_other);

    // 5. Add P576 demolished
    if let Some(demolished) = demolished_date {
        let sort_key = Some(demolished.value.earliest());
        dated_transitions.push(DatedTransition {
            transition: EntityTransition::Demolished {
                started_at: None,
                completed_at: Some(demolished),
                cause: None,
                trigger_event: None,
            },
            sort_key,
        });
    }

    // 6. Add usage-related transitions
    if let Some(opened) = opening {
        let sort_key = Some(opened.value.earliest());
        dated_transitions.push(DatedTransition {
            transition: EntityTransition::UsageModified {
                occurred_at: Some(opened),
                new_usages: [Usage::Unknown].into_iter().collect(),
                description: Some("Official opening".to_string()),
                trigger_event: None,
            },
            sort_key,
        });
    }

    if let Some(closed) = closure {
        let sort_key = Some(closed.value.earliest());
        dated_transitions.push(DatedTransition {
            transition: EntityTransition::UsageModified {
                occurred_at: Some(closed),
                new_usages: std::collections::BTreeSet::new(), // Empty = closed
                description: Some("Official closure".to_string()),
                trigger_event: None,
            },
            sort_key,
        });
    }

    if let Some(entry) = service_entry {
        let sort_key = Some(entry.value.earliest());
        dated_transitions.push(DatedTransition {
            transition: EntityTransition::UsageModified {
                occurred_at: Some(entry),
                new_usages: [Usage::Transportation].into_iter().collect(),
                description: Some("Service entry".to_string()),
                trigger_event: None,
            },
            sort_key,
        });
    }

    if let Some(retirement) = service_retirement {
        let sort_key = Some(retirement.value.earliest());
        dated_transitions.push(DatedTransition {
            transition: EntityTransition::UsageModified {
                occurred_at: Some(retirement),
                new_usages: std::collections::BTreeSet::new(), // Empty = retired
                description: Some("Service retirement".to_string()),
                trigger_event: None,
            },
            sort_key,
        });
    }

    // 7. Sort chronologically
    dated_transitions.sort_by(|a, b| a.sort_key.cmp(&b.sort_key));

    // 8. Split on demolish->construct boundaries
    let entities = split_on_rebuild(dated_transitions);

    (entities, warnings)
}

/// Split transitions into separate entities when demolish->construct indicates rebuild.
///
/// When splitting, if the construction that triggers the split has a location, the
/// predecessor gets a synthetic `Constructed` with that same location and no dates —
/// the previous building occupied the same site, we just don't know when it was built.
fn split_on_rebuild(transitions: Vec<DatedTransition>) -> Vec<Vec<EntityTransition>> {
    if transitions.is_empty() {
        return vec![];
    }

    let mut entities: Vec<Vec<EntityTransition>> = vec![vec![]];
    let mut saw_demolition = false;

    for dt in transitions {
        let is_construction = matches!(dt.transition, EntityTransition::Constructed { .. });
        let is_demolition = matches!(dt.transition, EntityTransition::Demolished { .. });

        // If we see construction after demolition, start a new entity.
        // Give the predecessor entity a Constructed at the same location.
        if saw_demolition && is_construction {
            if let EntityTransition::Constructed {
                location: Some(ref loc),
                ..
            } = dt.transition
            {
                if let Some(prev) = entities.last_mut() {
                    let has_location = prev.iter().any(|t| {
                        matches!(
                            t,
                            EntityTransition::Constructed {
                                location: Some(_),
                                ..
                            }
                        )
                    });
                    if !has_location {
                        prev.insert(
                            0,
                            EntityTransition::Constructed {
                                started_at: None,
                                completed_at: None,
                                location: Some(loc.clone()),
                                trigger_event: None,
                            },
                        );
                    }
                }
            }
            entities.push(vec![]);
            saw_demolition = false;
        }

        // Add transition to current entity (vec is provably non-empty)
        if let Some(last) = entities.last_mut() {
            last.push(dt.transition);
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
    use chrono::{Datelike, NaiveDate};
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

    fn midnight(y: i32, m: u32, d: u32) -> Option<NaiveDateTime> {
        NaiveDate::from_ymd_opt(y, m, d)?.and_hms_opt(0, 0, 0)
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
        let mut qualifiers = HashMap::new();
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
        let mut qualifiers = HashMap::new();
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
    ) -> Result<HashMap<PropertyId, Vec<Claim>>, String> {
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
        assert_eq!(date.earliest().year(), 1920);
        Ok(())
    }

    #[test]
    fn test_extract_mainsnak_coordinates() -> TestResult {
        let claim = coordinate_claim(40.7128, -74.0060);
        let (result, warnings) = extract::mainsnak_coordinates(&claim);
        assert!(warnings.is_empty());
        let result = result.ok_or("expected Some")?;

        if let UncertainLocation::Coordinates { lat, lon, .. } = result {
            assert!((lat - 40.7128).abs() < 0.0001);
            assert!((lon - (-74.0060)).abs() < 0.0001);
        } else {
            return Err("expected Coordinates".into());
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
            qualifiers: HashMap::new(),
            rank: Rank::Normal,
        };
        let (loc, warnings) = extract::mainsnak_coordinates(&claim_high_lat);
        assert!(warnings.is_empty());
        let loc = loc.ok_or("expected Some")?;

        if let UncertainLocation::Coordinates { precision_m, .. } = loc {
            let p = precision_m.ok_or("expected precision")?;
            // At 60°N, 1 degree longitude ≈ 55,800m (not 111,000m)
            assert!(
                p < 70_000,
                "precision at 60°N should be well under 70km, got {p}m"
            );
            assert!(
                p > 40_000,
                "precision at 60°N should be over 40km, got {p}m"
            );
        } else {
            return Err("expected Coordinates".into());
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
        assert_eq!(p580[0].0.earliest().year(), 1918);
        assert_eq!(p582[0].0.earliest().year(), 1920);
        Ok(())
    }

    #[test]
    fn test_split_on_rebuild() {
        // Simulate: Constructed 1920, Demolished 1950, Constructed 1960
        let transitions = vec![
            DatedTransition {
                transition: EntityTransition::Constructed {
                    started_at: None,
                    completed_at: None,
                    location: None,
                    trigger_event: None,
                },
                sort_key: midnight(1920, 1, 1),
            },
            DatedTransition {
                transition: EntityTransition::Demolished {
                    started_at: None,
                    completed_at: None,
                    cause: None,
                    trigger_event: None,
                },
                sort_key: midnight(1950, 1, 1),
            },
            DatedTransition {
                transition: EntityTransition::Constructed {
                    started_at: None,
                    completed_at: None,
                    location: None,
                    trigger_event: None,
                },
                sort_key: midnight(1960, 1, 1),
            },
        ];

        let entities = split_on_rebuild(transitions);
        assert_eq!(entities.len(), 2);
        assert_eq!(entities[0].len(), 2); // Constructed + Demolished
        assert_eq!(entities[1].len(), 1); // Constructed
    }

    #[test]
    fn test_split_on_rebuild_propagates_location() -> TestResult {
        // Predecessor has no location; successor was constructed at a known location.
        // The predecessor should get a synthetic Constructed with the inherited location.
        let loc = UncertainLocation::coordinates(45.217, 12.277, None, None)?;
        let transitions = vec![
            DatedTransition {
                transition: EntityTransition::Demolished {
                    started_at: None,
                    completed_at: None,
                    cause: None,
                    trigger_event: None,
                },
                sort_key: midnight(1623, 1, 1),
            },
            DatedTransition {
                transition: EntityTransition::Constructed {
                    started_at: None,
                    completed_at: None,
                    location: Some(Cited::uncited(loc)),
                    trigger_event: None,
                },
                sort_key: midnight(1633, 1, 1),
            },
        ];

        let entities = split_on_rebuild(transitions);
        assert_eq!(entities.len(), 2);

        // Predecessor: should have synthetic Constructed (with location) + Demolished
        assert_eq!(entities[0].len(), 2);
        assert!(
            matches!(
                &entities[0][0],
                EntityTransition::Constructed {
                    location: Some(_),
                    started_at: None,
                    completed_at: None,
                    ..
                }
            ),
            "predecessor should have synthetic Constructed with inherited location"
        );
        assert!(matches!(
            &entities[0][1],
            EntityTransition::Demolished { .. }
        ));

        // Successor: Constructed with location
        assert_eq!(entities[1].len(), 1);
        assert!(matches!(
            &entities[1][0],
            EntityTransition::Constructed {
                location: Some(_),
                ..
            }
        ));

        Ok(())
    }

    // =========================================================================
    // build_lifecycles integration tests
    // =========================================================================

    fn ctx() -> PropertyContext<'static> {
        PropertyContext::new("Q12345", 100, "lifecycle")
    }

    /// P571 inception date only -> single Constructed transition
    #[test]
    fn build_p571_inception_only() -> TestResult {
        let claims = claims_from(vec![(
            "P571",
            vec![time_claim(
                "+1920-01-01T00:00:00Z",
                WikidataPrecision::Year,
            )?],
        )])?;

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx());
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(lifecycles.len(), 1);

        let transitions = &lifecycles[0];
        assert_eq!(transitions.len(), 1);
        if let EntityTransition::Constructed {
            completed_at,
            started_at,
            location,
            ..
        } = &transitions[0]
        {
            // P571 becomes completed_at (inception = completion date)
            let date = completed_at.as_ref().ok_or("expected completed_at")?;
            assert_eq!(date.value.earliest().year(), 1920);
            assert!(started_at.is_none());
            assert!(location.is_none());
        } else {
            return Err("expected Constructed".into());
        }
        Ok(())
    }

    /// P571 + P625 -> Constructed with date and location
    #[test]
    fn build_p571_with_p625_location() -> TestResult {
        let claims = claims_from(vec![
            (
                "P571",
                vec![time_claim("+1889-03-31T00:00:00Z", WikidataPrecision::Day)?],
            ),
            ("P625", vec![coordinate_claim(48.8584, 2.2945)]),
        ])?;

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx());
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(lifecycles.len(), 1);
        assert_eq!(lifecycles[0].len(), 1);

        if let EntityTransition::Constructed {
            completed_at,
            location,
            ..
        } = &lifecycles[0][0]
        {
            assert!(completed_at.is_some());
            let loc = location.as_ref().ok_or("expected location")?;
            if let UncertainLocation::Coordinates { lat, lon, .. } = &loc.value {
                assert!((lat - 48.8584).abs() < 0.001);
                assert!((lon - 2.2945).abs() < 0.001);
            } else {
                return Err("expected Coordinates".into());
            }
        } else {
            return Err("expected Constructed".into());
        }
        Ok(())
    }

    /// P571 + P576 -> Constructed + Demolished
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

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx());
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(lifecycles.len(), 1);
        assert_eq!(lifecycles[0].len(), 2);

        // Sorted chronologically: Constructed 1900, Demolished 1960
        assert!(matches!(
            &lifecycles[0][0],
            EntityTransition::Constructed { .. }
        ));
        assert!(matches!(
            &lifecycles[0][1],
            EntityTransition::Demolished { .. }
        ));
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

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx());
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(lifecycles.len(), 1);
        assert_eq!(lifecycles[0].len(), 1);

        if let EntityTransition::Constructed {
            started_at,
            completed_at,
            ..
        } = &lifecycles[0][0]
        {
            let start = started_at.as_ref().ok_or("expected started_at")?;
            let end = completed_at.as_ref().ok_or("expected completed_at")?;
            assert_eq!(start.value.earliest().year(), 1887);
            assert_eq!(end.value.earliest().year(), 1889);
        } else {
            return Err("expected Constructed".into());
        }
        Ok(())
    }

    /// P793 damage events map to correct `DamageCause`
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

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx());
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(lifecycles.len(), 1);

        let transitions = &lifecycles[0];
        assert_eq!(transitions.len(), 3);

        // Sorted chronologically: fire 1871, earthquake 1906, flood 1927
        let causes: Vec<_> = transitions
            .iter()
            .filter_map(|t| {
                if let EntityTransition::Damaged { cause, .. } = t {
                    cause.clone()
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(causes.len(), 3);
        assert_eq!(causes[0], DamageCause::Fire);
        assert_eq!(causes[1], DamageCause::Earthquake);
        assert_eq!(causes[2], DamageCause::Flood);
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

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx());
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");

        // Should split into 2 entities
        assert_eq!(lifecycles.len(), 2);

        // First entity: constructed + demolished
        assert_eq!(lifecycles[0].len(), 2);
        assert!(matches!(
            &lifecycles[0][0],
            EntityTransition::Constructed { .. }
        ));
        assert!(matches!(
            &lifecycles[0][1],
            EntityTransition::Demolished { .. }
        ));

        // Second entity: constructed
        assert_eq!(lifecycles[1].len(), 1);
        assert!(matches!(
            &lifecycles[1][0],
            EntityTransition::Constructed { .. }
        ));
        Ok(())
    }

    /// Combined scenario: P571 inception + P793 renovation + P576 demolition
    /// Tests that all property types integrate correctly and sort chronologically
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

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx());
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(lifecycles.len(), 1);

        let transitions = &lifecycles[0];
        // Constructed (1850), Opening (1855), Renovation (1920), Demolished (1960)
        assert_eq!(transitions.len(), 4);

        // Verify chronological ordering
        assert!(matches!(
            transitions[0],
            EntityTransition::Constructed { .. }
        ));
        assert!(matches!(
            transitions[1],
            EntityTransition::UsageModified { .. }
        )); // opening
        assert!(matches!(transitions[2], EntityTransition::Modified { .. })); // renovation
        assert!(matches!(
            transitions[3],
            EntityTransition::Demolished { .. }
        ));
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

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx());
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(lifecycles.len(), 1);
        assert_eq!(lifecycles[0].len(), 1);

        if let EntityTransition::Constructed { location, .. } = &lifecycles[0][0] {
            let loc = location
                .as_ref()
                .ok_or("P625 should be merged into P793 construction")?;
            if let UncertainLocation::Coordinates { lat, lon, .. } = &loc.value {
                assert!((lat - 51.5074).abs() < 0.001);
                assert!((lon - (-0.1278)).abs() < 0.001);
            } else {
                return Err("expected Coordinates".into());
            }
        } else {
            return Err("expected Constructed".into());
        }
        Ok(())
    }

    /// P729/P730 service entry/retirement -> transportation usage transitions
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

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx());
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(lifecycles.len(), 1);
        assert_eq!(lifecycles[0].len(), 2);

        // Service entry -> Transportation usage
        if let EntityTransition::UsageModified {
            new_usages,
            description,
            ..
        } = &lifecycles[0][0]
        {
            assert!(new_usages.contains(&Usage::Transportation));
            assert_eq!(description.as_deref(), Some("Service entry"));
        } else {
            return Err("expected UsageModified for service entry".into());
        }

        // Service retirement -> empty usage (closed)
        if let EntityTransition::UsageModified {
            new_usages,
            description,
            ..
        } = &lifecycles[0][1]
        {
            assert!(
                new_usages.is_empty(),
                "retired service should have empty usage set"
            );
            assert_eq!(description.as_deref(), Some("Service retirement"));
        } else {
            return Err("expected UsageModified for service retirement".into());
        }
        Ok(())
    }

    /// P793 consecration -> Religious usage
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

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx());
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(lifecycles.len(), 1);
        assert_eq!(lifecycles[0].len(), 1);

        if let EntityTransition::UsageModified {
            new_usages,
            description,
            ..
        } = &lifecycles[0][0]
        {
            assert!(new_usages.contains(&Usage::Religious));
            assert_eq!(description.as_deref(), Some("Consecration"));
        } else {
            return Err("expected UsageModified for consecration".into());
        }
        Ok(())
    }

    /// Out-of-range coordinates produce a warning, not a crash
    #[test]
    fn build_invalid_coordinates_warns() -> TestResult {
        let claims = claims_from(vec![("P625", vec![coordinate_claim(999.0, -999.0)])])?;

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx());
        // Should produce a warning about invalid coordinates
        assert!(
            warnings.iter().any(|w| w.contains("invalid coordinates")),
            "expected invalid coordinates warning, got: {warnings:?}"
        );
        // No transitions since only the coordinate was provided (no P571)
        assert!(lifecycles.is_empty() || lifecycles[0].is_empty());
        Ok(())
    }

    /// Empty claims produce no transitions and no warnings
    #[test]
    fn build_empty_claims() -> TestResult {
        let claims = HashMap::new();
        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx());
        assert!(warnings.is_empty());
        assert!(lifecycles.is_empty());
        Ok(())
    }

    /// Unknown P793 event type is silently skipped
    #[test]
    fn build_p793_unknown_event_skipped() -> TestResult {
        let claims = claims_from(vec![(
            "P793",
            vec![p793_event(
                "Q99999999",
                "+1920-01-01T00:00:00Z",
                WikidataPrecision::Year,
            )?],
        )])?;

        let (lifecycles, warnings) = build_lifecycles(&claims, &ctx());
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        // Unknown event -> no transitions
        assert!(lifecycles.is_empty());
        Ok(())
    }
}
