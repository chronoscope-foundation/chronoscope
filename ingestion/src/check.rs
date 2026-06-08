//! Ingestion output analysis and validation.
//!
//! Analyzes `IngestionOutput` for consistency, distributions, and interesting entities.
//! Source-agnostic - works with output from any ingestion pathway.

use crate::{EntityIdx, IngestionOutput, SourceIdx};
use chrono::Datelike;
use chronoscope_core::{
    ConsistencyWarning, Entity, EntityTransition, LinkTarget, Location, UncertainDate,
    UnresolvedLocation,
};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Entity type specialized for ingestion output.
type IngestionEntity = Entity<SourceIdx>;
/// Entity transition type specialized for ingestion output.
type IngestionTransition = EntityTransition<SourceIdx>;
/// Uncertain location type specialized for ingestion output.
type IngestionLocation = UnresolvedLocation;

// =============================================================================
// OUTPUT STRUCTURES
// =============================================================================

#[derive(Serialize)]
pub struct AnalysisReport {
    pub summary: Summary,
    pub distributions: Distributions,
    pub consistency: ConsistencyAnalysis,
    pub interesting_entities: Vec<InterestingEntity>,
}

#[derive(Serialize)]
pub struct Summary {
    pub total_entities: usize,
    pub total_images: usize,
    pub total_links: usize,
    pub total_annotations: usize,
    pub entities_with_location: usize,
    pub entities_with_dates: usize,
    pub entities_with_transitions: usize,
    pub entities_with_images: usize,
}

#[derive(Serialize)]
pub struct Distributions {
    pub transition_types: BTreeMap<String, usize>,
    pub transition_counts_per_entity: BTreeMap<usize, usize>,
    pub date_precisions: BTreeMap<String, usize>,
    pub date_decades: BTreeMap<String, usize>,
    pub location_types: BTreeMap<String, usize>,
    pub usage_types: BTreeMap<String, usize>,
    pub images_per_entity: BTreeMap<String, usize>,
    pub names_per_entity: BTreeMap<usize, usize>,
    pub languages: BTreeMap<String, usize>,
}

#[derive(Serialize)]
pub struct ConsistencyAnalysis {
    pub total_warnings: usize,
    pub entities_with_warnings: usize,
    pub warnings_by_code: BTreeMap<String, usize>,
    pub sample_warnings: Vec<SampleWarning>,
}

#[derive(Serialize)]
pub struct SampleWarning {
    pub entity_name: String,
    pub code: String,
    pub message: String,
}

#[derive(Serialize)]
pub struct InterestingEntity {
    pub reason: String,
    pub name: String,
    pub wikidata_id: Option<String>,
    pub entity_index: EntityIdx,
    pub details: serde_json::Value,
}

// =============================================================================
// ANALYSIS
// =============================================================================

/// Analyze an `IngestionOutput` and produce a comprehensive report.
#[must_use]
pub fn analyze(output: &IngestionOutput) -> AnalysisReport {
    let summary = compute_summary(output);
    let distributions = compute_distributions(output);
    let consistency = analyze_consistency(output);
    let interesting_entities = find_interesting_entities(output);

    AnalysisReport {
        summary,
        distributions,
        consistency,
        interesting_entities,
    }
}

fn compute_summary(output: &IngestionOutput) -> Summary {
    let entities_with_location = output.entities.values().filter(|e| has_location(e)).count();

    let entities_with_dates = output.entities.values().filter(|e| has_dates(e)).count();

    let entities_with_transitions = output
        .entities
        .values()
        .filter(|e| !e.transitions.is_empty())
        .count();

    // Count entities that have at least one image annotation
    let mut entities_with_images_set = HashSet::new();
    for ann in &output.annotations {
        entities_with_images_set.insert(ann.entity);
    }

    Summary {
        total_entities: output.entities.len(),
        total_images: output.images.len(),
        total_links: output.external_links.len(),
        total_annotations: output.annotations.len(),
        entities_with_location,
        entities_with_dates,
        entities_with_transitions,
        entities_with_images: entities_with_images_set.len(),
    }
}

fn compute_distributions(output: &IngestionOutput) -> Distributions {
    let mut transition_types: BTreeMap<String, usize> = BTreeMap::new();
    let mut transition_counts: BTreeMap<usize, usize> = BTreeMap::new();
    let mut date_precisions: BTreeMap<String, usize> = BTreeMap::new();
    let mut date_decades: BTreeMap<String, usize> = BTreeMap::new();
    let mut location_types: BTreeMap<String, usize> = BTreeMap::new();
    let mut usage_types: BTreeMap<String, usize> = BTreeMap::new();
    let mut names_per_entity: BTreeMap<usize, usize> = BTreeMap::new();
    let mut languages: BTreeMap<String, usize> = BTreeMap::new();

    for entity in output.entities.values() {
        // Transition counts
        *transition_counts
            .entry(entity.transitions.len())
            .or_default() += 1;

        // Names
        *names_per_entity.entry(entity.names.len()).or_default() += 1;

        for name in &entity.names {
            *languages
                .entry(name.value.language.as_str().to_string())
                .or_default() += 1;
        }

        for transition in &entity.transitions {
            // Transition types
            *transition_types.entry(transition.to_string()).or_default() += 1;

            // Extract dates and locations from transitions
            for date in extract_dates(transition) {
                let precision = get_date_precision(date);
                *date_precisions.entry(precision).or_default() += 1;

                if let Some(earliest) = date.earliest() {
                    let decade = (earliest.year() / 10) * 10;
                    let decade_str = format!("{decade}s");
                    *date_decades.entry(decade_str).or_default() += 1;
                }
            }

            if let Some(loc) = extract_location(transition) {
                let loc_type = match loc {
                    UnresolvedLocation::Resolved(Location::Circle { .. }) => "Circle",
                    UnresolvedLocation::Resolved(Location::UnionOf { .. }) => "UnionOf",
                    UnresolvedLocation::Resolved(Location::Unbounded) => "Unbounded",
                    UnresolvedLocation::Reference(_) => "Reference",
                    UnresolvedLocation::OneOf(_) => "OneOf",
                };
                *location_types.entry(loc_type.to_string()).or_default() += 1;
            }

            // Usage types
            if let EntityTransition::UsageModified { new_usages, .. } = transition {
                for usage in new_usages {
                    let usage_str = format!("{usage:?}");
                    *usage_types.entry(usage_str).or_default() += 1;
                }
            }
        }
    }

    // Images per entity distribution
    let mut images_count: HashMap<EntityIdx, usize> = HashMap::new();
    for ann in &output.annotations {
        *images_count.entry(ann.entity).or_default() += 1;
    }
    let mut images_per_entity: BTreeMap<String, usize> = BTreeMap::new();
    for (_entity_key, count) in images_count {
        let bucket = match count {
            0 => "0",
            1 => "1",
            2..=5 => "2-5",
            6..=10 => "6-10",
            11..=50 => "11-50",
            _ => "50+",
        };
        *images_per_entity.entry(bucket.to_string()).or_default() += 1;
    }

    Distributions {
        transition_types,
        transition_counts_per_entity: transition_counts,
        date_precisions,
        date_decades,
        location_types,
        usage_types,
        images_per_entity,
        names_per_entity,
        languages,
    }
}

fn analyze_consistency(output: &IngestionOutput) -> ConsistencyAnalysis {
    let mut total_warnings = 0;
    let mut entities_with_warnings = 0;
    let mut warnings_by_code: BTreeMap<String, usize> = BTreeMap::new();
    let mut sample_warnings: Vec<SampleWarning> = Vec::new();

    for entity in output.entities.values() {
        let warnings = entity.check_consistency();
        if !warnings.is_empty() {
            entities_with_warnings += 1;
            let name = get_entity_name(entity);

            for warning in &warnings {
                total_warnings += 1;
                let code = warning_code(warning).to_string();
                *warnings_by_code.entry(code.clone()).or_default() += 1;

                // Collect samples (up to 5 per code)
                let code_count = sample_warnings.iter().filter(|w| w.code == code).count();
                if code_count < 5 {
                    sample_warnings.push(SampleWarning {
                        entity_name: name.clone(),
                        code,
                        message: format!("{warning:?}"),
                    });
                }
            }
        }
    }

    ConsistencyAnalysis {
        total_warnings,
        entities_with_warnings,
        warnings_by_code,
        sample_warnings,
    }
}

fn find_interesting_entities(output: &IngestionOutput) -> Vec<InterestingEntity> {
    let mut interesting = Vec::new();

    for (entity_key, entity) in &output.entities {
        let name = get_entity_name(entity);
        let wikidata_id = get_wikidata_id(output, entity_key);

        // Entities with many transitions (complex history)
        if entity.transitions.len() >= 5 {
            interesting.push(InterestingEntity {
                reason: "Many transitions (complex history)".to_string(),
                name: name.clone(),
                wikidata_id: wikidata_id.clone(),
                entity_index: *entity_key,
                details: serde_json::json!({
                    "transition_count": entity.transitions.len(),
                    "transition_types": entity.transitions.iter()
                        .map(|t| t.to_string())
                        .collect::<Vec<_>>()
                }),
            });
        }

        // Entities with damage events
        if entity
            .transitions
            .iter()
            .any(|t| matches!(t, EntityTransition::Damaged { .. }))
        {
            let damage_events: Vec<_> = entity
                .transitions
                .iter()
                .filter_map(|t| {
                    if let EntityTransition::Damaged {
                        occurred_at, cause, ..
                    } = t
                    {
                        Some(serde_json::json!({
                            "date": occurred_at.as_ref().and_then(|d| d.value.earliest()).map(|e| e.to_string()),
                            "cause": cause.as_ref().map(|c| format!("{c:?}"))
                        }))
                    } else {
                        None
                    }
                })
                .collect();

            if interesting.len() < 50 {
                interesting.push(InterestingEntity {
                    reason: "Has damage event".to_string(),
                    name: name.clone(),
                    wikidata_id: wikidata_id.clone(),
                    entity_index: *entity_key,
                    details: serde_json::json!({ "damage_events": damage_events }),
                });
            }
        }

        // Demolished entities
        if entity
            .transitions
            .iter()
            .any(|t| matches!(t, EntityTransition::Demolished { .. }))
            && interesting.len() < 50
        {
            let demolished = entity.transitions.iter().find_map(|t| {
                if let EntityTransition::Demolished { started_at, .. } = t {
                    started_at
                        .as_ref()
                        .and_then(|d| d.value.earliest())
                        .map(|e| e.to_string())
                } else {
                    None
                }
            });

            interesting.push(InterestingEntity {
                reason: "Demolished".to_string(),
                name: name.clone(),
                wikidata_id: wikidata_id.clone(),
                entity_index: *entity_key,
                details: serde_json::json!({ "demolished_date": demolished }),
            });
        }

        // Very old entities (before 1500)
        'outer: for transition in &entity.transitions {
            for date in extract_dates(transition) {
                if date.earliest().is_some_and(|e| e.year() < 1500) && interesting.len() < 50 {
                    interesting.push(InterestingEntity {
                        reason: "Very old (pre-1500)".to_string(),
                        name: name.clone(),
                        wikidata_id: wikidata_id.clone(),
                        entity_index: *entity_key,
                        details: serde_json::json!({
                            "earliest_date": date.earliest().map(|e| e.to_string())
                        }),
                    });
                    break 'outer;
                }
            }
        }

        // Entities with consistency warnings
        let warnings = entity.check_consistency();
        if !warnings.is_empty() && interesting.len() < 50 {
            interesting.push(InterestingEntity {
                reason: "Has consistency warnings".to_string(),
                name: name.clone(),
                wikidata_id: wikidata_id.clone(),
                entity_index: *entity_key,
                details: serde_json::json!({
                    "warnings": warnings.iter()
                        .map(|w| serde_json::json!({
                            "code": warning_code(w),
                            "message": format!("{w:?}")
                        }))
                        .collect::<Vec<_>>()
                }),
            });
        }

        // Cap total interesting entities
        if interesting.len() >= 100 {
            break;
        }
    }

    interesting
}

// =============================================================================
// HELPERS
// =============================================================================

fn has_location(entity: &IngestionEntity) -> bool {
    entity.transitions.iter().any(|t| {
        if let EntityTransition::Constructed { location, .. } = t {
            location.is_some()
        } else {
            false
        }
    })
}

fn has_dates(entity: &IngestionEntity) -> bool {
    entity
        .transitions
        .iter()
        .any(|t| !extract_dates(t).is_empty())
}

fn extract_dates(transition: &IngestionTransition) -> Vec<&UncertainDate> {
    let mut dates = Vec::new();
    match transition {
        EntityTransition::Constructed {
            started_at,
            completed_at,
            ..
        }
        | EntityTransition::Modified {
            started_at,
            completed_at,
            ..
        }
        | EntityTransition::Repaired {
            started_at,
            completed_at,
            ..
        } => {
            if let Some(d) = started_at {
                dates.push(&d.value);
            }
            if let Some(d) = completed_at {
                dates.push(&d.value);
            }
        }
        EntityTransition::Damaged { occurred_at, .. }
        | EntityTransition::Moved { occurred_at, .. }
        | EntityTransition::UsageModified { occurred_at, .. }
        | EntityTransition::Designated { occurred_at, .. } => {
            if let Some(d) = occurred_at {
                dates.push(&d.value);
            }
        }
        EntityTransition::Demolished {
            started_at,
            completed_at,
            ..
        } => {
            if let Some(d) = started_at {
                dates.push(&d.value);
            }
            if let Some(d) = completed_at {
                dates.push(&d.value);
            }
        }
    }
    dates
}

fn extract_location(transition: &IngestionTransition) -> Option<&IngestionLocation> {
    if let EntityTransition::Constructed { location, .. } = transition {
        location.as_ref().map(|l| &l.value)
    } else {
        None
    }
}

fn get_entity_name(entity: &IngestionEntity) -> String {
    entity
        .names
        .iter()
        .find(|n| n.value.language.as_str() == "en")
        .or(entity.names.first())
        .map(|n| n.value.name.clone())
        .unwrap_or_else(|| "Unknown".to_string())
}

fn get_wikidata_id(output: &IngestionOutput, entity_key: &EntityIdx) -> Option<String> {
    output.entity_links.get(entity_key).and_then(|link_keys| {
        for link_key in link_keys {
            if let Some(link) = output.external_links.get(link_key)
                && let LinkTarget::Wikidata { entity_id } = &link.target
            {
                return Some(entity_id.to_string());
            }
        }
        None
    })
}

fn get_date_precision(date: &UncertainDate) -> String {
    // Use the earliest bound's precision; fall back to latest bound; if neither, "Unknown"
    let bound = date.earliest_bound().or(date.latest_bound());
    match bound {
        Some(b) => format!("{:?}", b.precision()),
        None => "Unknown".to_string(),
    }
}

fn warning_code(w: &ConsistencyWarning) -> &'static str {
    match w {
        ConsistencyWarning::CompletionBeforeStart { .. } => "CompletionBeforeStart",
        ConsistencyWarning::EventsOutOfOrder { .. } => "EventsOutOfOrder",
        ConsistencyWarning::EventAfterDemolished { .. } => "EventAfterDemolished",
        ConsistencyWarning::MultipleConstructions { .. } => "MultipleConstructions",
        ConsistencyWarning::NameValidityInverted { .. } => "NameValidityInverted",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EntityIdx, LinkIdx, SourceIdx};
    use chrono::NaiveDate;
    use chronoscope_core::{
        Annotation, AnnotationKind, Cited, DamageCause, DatePrecision, Entity, ExternalLink,
        ImageSource, LinkTarget, LinkType, UncertainDate, Usage, WikidataEntityId,
    };
    use oxilangtag::LanguageTag;
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn ymd(y: i32, m: u32, d: u32) -> Result<NaiveDate, &'static str> {
        NaiveDate::from_ymd_opt(y, m, d).ok_or("invalid date")
    }

    fn make_name(name: &str, lang: &str) -> Cited<chronoscope_core::EntityName, SourceIdx> {
        #[allow(clippy::expect_used)]
        Cited::uncited(chronoscope_core::EntityName {
            name: name.to_string(),
            name_type: chronoscope_core::NameType::Common,
            language: LanguageTag::parse(lang.to_string()).expect("valid lang tag for test"),
            valid_from: None,
            valid_to: None,
        })
    }

    fn simple_entity(name: &str) -> IngestionEntity {
        Entity {
            names: vec![make_name(name, "en")],
            transitions: vec![],
        }
    }

    fn entity_with_construction(
        name: &str,
        year: i32,
    ) -> Result<IngestionEntity, Box<dyn std::error::Error>> {
        let date = UncertainDate::with_precision(ymd(year, 1, 1)?, DatePrecision::Year)?;
        Ok(Entity {
            names: vec![make_name(name, "en")],
            transitions: vec![EntityTransition::Constructed {
                started_at: None,
                completed_at: Some(Cited::uncited(date)),
                location: None,
                trigger_event: None,
            }],
        })
    }

    fn empty_output() -> IngestionOutput {
        IngestionOutput::new()
    }

    // =========================================================================
    // analyze() — summary
    // =========================================================================

    #[test]
    fn analyze_empty_output() -> TestResult {
        let output = empty_output();
        let report = analyze(&output);
        assert_eq!(report.summary.total_entities, 0);
        assert_eq!(report.summary.total_images, 0);
        assert_eq!(report.summary.total_links, 0);
        assert_eq!(report.summary.total_annotations, 0);
        assert_eq!(report.summary.entities_with_location, 0);
        assert_eq!(report.summary.entities_with_dates, 0);
        assert_eq!(report.summary.entities_with_transitions, 0);
        assert_eq!(report.summary.entities_with_images, 0);
        Ok(())
    }

    #[test]
    fn analyze_counts_entities_and_images() -> TestResult {
        let mut output = empty_output();
        output
            .entities
            .insert(EntityIdx::new(0), simple_entity("Building A"));
        output
            .entities
            .insert(EntityIdx::new(1), simple_entity("Building B"));
        output.images.insert(
            SourceIdx::new(0),
            ImageSource {
                url: url::Url::parse("https://example.com/img.jpg")?,
                date: None,
                location: None,
            },
        );

        let report = analyze(&output);
        assert_eq!(report.summary.total_entities, 2);
        assert_eq!(report.summary.total_images, 1);
        Ok(())
    }

    #[test]
    fn analyze_counts_entities_with_dates() -> TestResult {
        let mut output = empty_output();
        output
            .entities
            .insert(EntityIdx::new(0), entity_with_construction("Dated", 1900)?);
        output
            .entities
            .insert(EntityIdx::new(1), simple_entity("No Date"));

        let report = analyze(&output);
        assert_eq!(report.summary.entities_with_dates, 1);
        assert_eq!(report.summary.entities_with_transitions, 1);
        Ok(())
    }

    #[test]
    fn analyze_counts_entities_with_images() -> TestResult {
        let mut output = empty_output();
        output
            .entities
            .insert(EntityIdx::new(0), simple_entity("With Image"));
        output
            .entities
            .insert(EntityIdx::new(1), simple_entity("Without Image"));
        output.images.insert(
            SourceIdx::new(0),
            ImageSource {
                url: url::Url::parse("https://example.com/img.jpg")?,
                date: None,
                location: None,
            },
        );
        output.annotations.push(Annotation {
            source: SourceIdx::new(0),
            entity: EntityIdx::new(0),
            kind: AnnotationKind::ExteriorView { region: None },
        });

        let report = analyze(&output);
        assert_eq!(report.summary.entities_with_images, 1);
        assert_eq!(report.summary.total_annotations, 1);
        Ok(())
    }

    // =========================================================================
    // analyze() — distributions
    // =========================================================================

    #[test]
    fn analyze_transition_type_distribution() -> TestResult {
        let mut output = empty_output();
        let date = UncertainDate::with_precision(ymd(1900, 1, 1)?, DatePrecision::Year)?;
        let mut entity = simple_entity("Test");
        entity.transitions.push(EntityTransition::Constructed {
            started_at: None,
            completed_at: Some(Cited::uncited(date.clone())),
            location: None,
            trigger_event: None,
        });
        entity.transitions.push(EntityTransition::Damaged {
            occurred_at: Some(Cited::uncited(date)),
            cause: Some(DamageCause::Fire),
            description: None,
            trigger_event: None,
        });
        output.entities.insert(EntityIdx::new(0), entity);

        let report = analyze(&output);
        assert_eq!(
            report.distributions.transition_types.get("constructed"),
            Some(&1)
        );
        assert_eq!(
            report.distributions.transition_types.get("damaged"),
            Some(&1)
        );
        Ok(())
    }

    #[test]
    fn analyze_date_precision_distribution() -> TestResult {
        let mut output = empty_output();
        let day_date = UncertainDate::with_precision(ymd(1920, 6, 15)?, DatePrecision::Day)?;
        let year_date = UncertainDate::with_precision(ymd(1950, 1, 1)?, DatePrecision::Year)?;
        let mut entity = simple_entity("Test");
        entity.transitions.push(EntityTransition::Constructed {
            started_at: Some(Cited::uncited(day_date)),
            completed_at: Some(Cited::uncited(year_date)),
            location: None,
            trigger_event: None,
        });
        output.entities.insert(EntityIdx::new(0), entity);

        let report = analyze(&output);
        assert_eq!(report.distributions.date_precisions.get("Day"), Some(&1));
        assert_eq!(report.distributions.date_precisions.get("Year"), Some(&1));
        Ok(())
    }

    #[test]
    fn analyze_date_decades_distribution() -> TestResult {
        let mut output = empty_output();
        let date = UncertainDate::with_precision(ymd(1925, 1, 1)?, DatePrecision::Year)?;
        let mut entity = simple_entity("Test");
        entity.transitions.push(EntityTransition::Constructed {
            started_at: None,
            completed_at: Some(Cited::uncited(date)),
            location: None,
            trigger_event: None,
        });
        output.entities.insert(EntityIdx::new(0), entity);

        let report = analyze(&output);
        assert_eq!(report.distributions.date_decades.get("1920s"), Some(&1));
        Ok(())
    }

    #[test]
    fn analyze_usage_type_distribution() -> TestResult {
        let mut output = empty_output();
        let mut entity = simple_entity("Test");
        entity.transitions.push(EntityTransition::UsageModified {
            occurred_at: None,
            new_usages: std::collections::BTreeSet::from([Usage::Religious]),
            description: None,
            trigger_event: None,
        });
        output.entities.insert(EntityIdx::new(0), entity);

        let report = analyze(&output);
        assert_eq!(report.distributions.usage_types.get("Religious"), Some(&1));
        Ok(())
    }

    #[test]
    fn analyze_names_per_entity_distribution() -> TestResult {
        let mut output = empty_output();
        let mut entity = simple_entity("Primary Name");
        entity.names.push(make_name("Alternate Name", "de"));
        output.entities.insert(EntityIdx::new(0), entity);

        let report = analyze(&output);
        // Entity has 2 names
        assert_eq!(report.distributions.names_per_entity.get(&2), Some(&1));
        Ok(())
    }

    #[test]
    fn analyze_language_distribution() -> TestResult {
        let mut output = empty_output();
        let mut entity = simple_entity("English Name");
        entity.names.push(make_name("Nom Francais", "fr"));
        output.entities.insert(EntityIdx::new(0), entity);

        let report = analyze(&output);
        assert_eq!(report.distributions.languages.get("en"), Some(&1));
        assert_eq!(report.distributions.languages.get("fr"), Some(&1));
        Ok(())
    }

    #[test]
    fn analyze_images_per_entity_buckets() -> TestResult {
        let mut output = empty_output();
        output
            .entities
            .insert(EntityIdx::new(0), simple_entity("Test"));
        // Add 3 images for entity 0
        for i in 0..3 {
            output.images.insert(
                SourceIdx::new(i),
                ImageSource {
                    url: url::Url::parse(&format!("https://example.com/img{i}.jpg"))?,
                    date: None,
                    location: None,
                },
            );
            output.annotations.push(Annotation {
                source: SourceIdx::new(i),
                entity: EntityIdx::new(0),
                kind: AnnotationKind::ExteriorView { region: None },
            });
        }

        let report = analyze(&output);
        // 3 images -> "2-5" bucket
        assert_eq!(report.distributions.images_per_entity.get("2-5"), Some(&1));
        Ok(())
    }

    // =========================================================================
    // analyze() — interesting entities
    // =========================================================================

    #[test]
    fn analyze_finds_demolished_entities() -> TestResult {
        let mut output = empty_output();
        let date = UncertainDate::with_precision(ymd(1960, 1, 1)?, DatePrecision::Year)?;
        let mut entity = simple_entity("Demolished Building");
        entity.transitions.push(EntityTransition::Demolished {
            started_at: Some(Cited::uncited(date)),
            completed_at: None,
            cause: None,
            trigger_event: None,
        });
        output.entities.insert(EntityIdx::new(0), entity);

        let report = analyze(&output);
        let demolished: Vec<_> = report
            .interesting_entities
            .iter()
            .filter(|e| e.reason == "Demolished")
            .collect();
        assert_eq!(demolished.len(), 1);
        assert_eq!(demolished[0].name, "Demolished Building");
        Ok(())
    }

    #[test]
    fn analyze_finds_damaged_entities() -> TestResult {
        let mut output = empty_output();
        let date = UncertainDate::with_precision(ymd(1906, 4, 18)?, DatePrecision::Day)?;
        let mut entity = simple_entity("Damaged Building");
        entity.transitions.push(EntityTransition::Damaged {
            occurred_at: Some(Cited::uncited(date)),
            cause: Some(DamageCause::Earthquake),
            description: None,
            trigger_event: None,
        });
        output.entities.insert(EntityIdx::new(0), entity);

        let report = analyze(&output);
        let damaged: Vec<_> = report
            .interesting_entities
            .iter()
            .filter(|e| e.reason == "Has damage event")
            .collect();
        assert_eq!(damaged.len(), 1);
        assert_eq!(damaged[0].name, "Damaged Building");
        Ok(())
    }

    #[test]
    fn analyze_finds_very_old_entities() -> TestResult {
        let mut output = empty_output();
        let date = UncertainDate::with_precision(ymd(500, 1, 1)?, DatePrecision::Year)?;
        let mut entity = simple_entity("Ancient Structure");
        entity.transitions.push(EntityTransition::Constructed {
            started_at: None,
            completed_at: Some(Cited::uncited(date)),
            location: None,
            trigger_event: None,
        });
        output.entities.insert(EntityIdx::new(0), entity);

        let report = analyze(&output);
        let old: Vec<_> = report
            .interesting_entities
            .iter()
            .filter(|e| e.reason == "Very old (pre-1500)")
            .collect();
        assert_eq!(old.len(), 1);
        assert_eq!(old[0].name, "Ancient Structure");
        Ok(())
    }

    #[test]
    fn analyze_finds_many_transitions() -> TestResult {
        let mut output = empty_output();
        let mut entity = simple_entity("Complex Building");
        // Add 5+ transitions to trigger "Many transitions" interesting entity
        for year in 1900..1906 {
            let date = UncertainDate::with_precision(ymd(year, 1, 1)?, DatePrecision::Year)?;
            entity.transitions.push(EntityTransition::Modified {
                started_at: Some(Cited::uncited(date)),
                completed_at: None,
                description: Some(format!("Renovation {year}")),
                trigger_event: None,
            });
        }
        output.entities.insert(EntityIdx::new(0), entity);

        let report = analyze(&output);
        let complex: Vec<_> = report
            .interesting_entities
            .iter()
            .filter(|e| e.reason.contains("Many transitions"))
            .collect();
        assert_eq!(complex.len(), 1);
        assert_eq!(complex[0].name, "Complex Building");
        Ok(())
    }

    // =========================================================================
    // analyze() — wikidata ID linkage
    // =========================================================================

    #[test]
    fn analyze_interesting_entity_includes_wikidata_id() -> TestResult {
        let mut output = empty_output();
        let date = UncertainDate::with_precision(ymd(1960, 1, 1)?, DatePrecision::Year)?;
        let mut entity = simple_entity("Linked Building");
        entity.transitions.push(EntityTransition::Demolished {
            started_at: Some(Cited::uncited(date)),
            completed_at: None,
            cause: None,
            trigger_event: None,
        });
        output.entities.insert(EntityIdx::new(0), entity);

        // Add a Wikidata link
        output.external_links.insert(
            LinkIdx::new(0),
            ExternalLink {
                target: LinkTarget::Wikidata {
                    entity_id: WikidataEntityId::new(12345),
                },
                link_type: LinkType::SameAs,
            },
        );
        output
            .entity_links
            .insert(EntityIdx::new(0), vec![LinkIdx::new(0)]);

        let report = analyze(&output);
        let demolished: Vec<_> = report
            .interesting_entities
            .iter()
            .filter(|e| e.reason == "Demolished")
            .collect();
        assert_eq!(demolished.len(), 1);
        assert_eq!(demolished[0].wikidata_id, Some("Q12345".to_string()));
        Ok(())
    }

    // =========================================================================
    // Helper tests
    // =========================================================================

    #[test]
    fn get_entity_name_prefers_english() -> TestResult {
        let entity = Entity {
            names: vec![make_name("Maison", "fr"), make_name("House", "en")],
            transitions: vec![],
        };
        assert_eq!(get_entity_name(&entity), "House");
        Ok(())
    }

    #[test]
    fn get_entity_name_falls_back_to_first() -> TestResult {
        let entity = Entity {
            names: vec![make_name("Maison", "fr"), make_name("Haus", "de")],
            transitions: vec![],
        };
        assert_eq!(get_entity_name(&entity), "Maison");
        Ok(())
    }

    #[test]
    fn get_entity_name_returns_unknown_for_empty() -> TestResult {
        let entity = Entity {
            names: vec![],
            transitions: vec![],
        };
        assert_eq!(get_entity_name(&entity), "Unknown");
        Ok(())
    }

    #[test]
    fn get_date_precision_precise() -> TestResult {
        let date = UncertainDate::with_precision(ymd(2020, 1, 1)?, DatePrecision::Year)?;
        assert_eq!(get_date_precision(&date), "Year");
        Ok(())
    }

    #[test]
    fn get_date_precision_range() -> TestResult {
        let date = UncertainDate::bounded(
            Some(chronoscope_core::DateBound::new(
                ymd(1900, 1, 1)?,
                DatePrecision::Year,
            )?),
            Some(chronoscope_core::DateBound::new(
                ymd(1910, 1, 1)?,
                DatePrecision::Year,
            )?),
        )?;
        assert_eq!(get_date_precision(&date), "Year");
        Ok(())
    }

    #[test]
    fn extract_dates_from_constructed() -> TestResult {
        let date = UncertainDate::with_precision(ymd(1900, 1, 1)?, DatePrecision::Year)?;
        let transition = EntityTransition::Constructed {
            started_at: Some(Cited::uncited(date.clone())),
            completed_at: Some(Cited::uncited(date)),
            location: None,
            trigger_event: None,
        };
        let dates = extract_dates(&transition);
        assert_eq!(dates.len(), 2);
        Ok(())
    }

    #[test]
    fn extract_dates_from_damaged() -> TestResult {
        let date = UncertainDate::with_precision(ymd(1906, 4, 18)?, DatePrecision::Day)?;
        let transition = EntityTransition::Damaged {
            occurred_at: Some(Cited::uncited(date)),
            cause: None,
            description: None,
            trigger_event: None,
        };
        let dates = extract_dates(&transition);
        assert_eq!(dates.len(), 1);
        Ok(())
    }

    #[test]
    fn extract_dates_from_designated() -> TestResult {
        let date = UncertainDate::with_precision(ymd(1978, 1, 1)?, DatePrecision::Year)?;
        let transition = EntityTransition::Designated {
            occurred_at: Some(Cited::uncited(date)),
            designation: "National Historic Landmark".to_string(),
            description: None,
            trigger_event: None,
        };
        let dates = extract_dates(&transition);
        assert_eq!(dates.len(), 1);
        Ok(())
    }

    #[test]
    fn extract_location_from_constructed() -> TestResult {
        let loc = UnresolvedLocation::Reference(chronoscope_core::LocationReference::NamedPlace {
            name: "Paris".to_string(),
        });
        let transition = EntityTransition::Constructed {
            started_at: None,
            completed_at: None,
            location: Some(Cited::uncited(loc)),
            trigger_event: None,
        };
        assert!(extract_location(&transition).is_some());
        Ok(())
    }

    #[test]
    fn extract_location_from_non_constructed() -> TestResult {
        let transition = EntityTransition::Damaged {
            occurred_at: None,
            cause: None,
            description: None,
            trigger_event: None,
        };
        assert!(extract_location(&transition).is_none());
        Ok(())
    }

    #[test]
    fn analyze_report_serializes_to_json() -> TestResult {
        let mut output = empty_output();
        output
            .entities
            .insert(EntityIdx::new(0), entity_with_construction("Test", 1920)?);

        let report = analyze(&output);
        let json = serde_json::to_string(&report)?;
        // Should produce valid JSON
        let _: serde_json::Value = serde_json::from_str(&json)?;
        Ok(())
    }

    #[test]
    fn analyze_location_type_distribution() -> TestResult {
        let mut output = empty_output();
        let loc = UnresolvedLocation::Reference(chronoscope_core::LocationReference::NamedPlace {
            name: "Paris".to_string(),
        });
        let mut entity = simple_entity("Test");
        entity.transitions.push(EntityTransition::Constructed {
            started_at: None,
            completed_at: None,
            location: Some(Cited::uncited(loc)),
            trigger_event: None,
        });
        output.entities.insert(EntityIdx::new(0), entity);

        let report = analyze(&output);
        assert_eq!(
            report.distributions.location_types.get("Reference"),
            Some(&1)
        );
        assert_eq!(report.summary.entities_with_location, 1);
        Ok(())
    }

    #[test]
    fn analyze_transition_counts_per_entity() -> TestResult {
        let mut output = empty_output();
        // Entity with 0 transitions
        output
            .entities
            .insert(EntityIdx::new(0), simple_entity("Empty"));
        // Entity with 2 transitions
        let mut entity = simple_entity("Busy");
        entity.transitions.push(EntityTransition::Constructed {
            started_at: None,
            completed_at: None,
            location: None,
            trigger_event: None,
        });
        entity.transitions.push(EntityTransition::Demolished {
            started_at: None,
            completed_at: None,
            cause: None,
            trigger_event: None,
        });
        output.entities.insert(EntityIdx::new(1), entity);

        let report = analyze(&output);
        assert_eq!(
            report.distributions.transition_counts_per_entity.get(&0),
            Some(&1)
        );
        assert_eq!(
            report.distributions.transition_counts_per_entity.get(&2),
            Some(&1)
        );
        Ok(())
    }
}
