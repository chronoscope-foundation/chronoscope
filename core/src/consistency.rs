//! Entity consistency checking.
//!
//! Checks for logical inconsistencies in entity data. Inconsistencies are reported
//! but don't prevent data from being accepted.

use chrono::NaiveDateTime;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::date::UncertainDate;
use crate::entity::{Entity, EntityTransition};
use crate::moment::{Moment, decompose, structural_edges};

// Consistency checking is generic over entity/source reference types — it only
// inspects dates and names, never the reference types themselves.

/// A consistency warning carrying structured data.
///
/// Presentation-layer code is responsible for formatting these into human-readable
/// messages (enabling multilingual UIs and machine reasoning over warnings).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConsistencyWarning {
    /// A transition's completion date is before its start date.
    CompletionBeforeStart {
        started_earliest: NaiveDateTime,
        completed_latest: NaiveDateTime,
    },
    /// Transitions are not in chronological order as listed.
    EventsOutOfOrder {
        earlier_date: NaiveDateTime,
        later_date: NaiveDateTime,
    },
    /// A non-demolition event occurs after the entity was demolished.
    EventAfterDemolished {
        event_date: NaiveDateTime,
        demolished_date: NaiveDateTime,
    },
    /// Multiple construction events exist (may indicate data merge issue or reconstruction).
    MultipleConstructions { count: usize },
    /// A name's validity range is inverted (`valid_from` > `valid_to`).
    NameValidityInverted {
        name: String,
        valid_from: UncertainDate,
        valid_to: UncertainDate,
    },
}

/// Detect temporal violations by checking each structural edge against date
/// evidence. A structural edge `from → to` is violated when
/// `to.latest < from.earliest` — i.e. the dates say `to` comes strictly
/// before `from`, contradicting the lifecycle rule.
///
/// Violations are classified by the kind of structural edge:
/// - Same-transition start→end: [`ConsistencyWarning::CompletionBeforeStart`]
/// - Non-demolition→demolition: [`ConsistencyWarning::EventAfterDemolished`]
/// - Anything else: [`ConsistencyWarning::EventsOutOfOrder`]
///
/// TODO: this validates moment-level projections rather than the underlying
/// transitions directly. Whether that's the right surface for validation is
/// an open question — it may change when the spatiotemporal solver lands,
/// which will need to reason about constraints more holistically.
fn find_violations<E, S>(moments: &[Moment<'_, E, S>]) -> Vec<ConsistencyWarning> {
    let edges = structural_edges(moments);
    let mut warnings = Vec::new();

    for &(from, to) in &edges {
        let (from_m, to_m) = (&moments[from], &moments[to]);
        let (Some(from_early), Some(to_late)) = (from_m.earliest(), to_m.latest()) else {
            continue;
        };
        if to_late >= from_early {
            continue;
        }

        // Dates contradict structural edge — classify the violation.
        if from_m.transition_index == to_m.transition_index
            && from_m.role.durational_end() == Some(to_m.role)
        {
            warnings.push(ConsistencyWarning::CompletionBeforeStart {
                started_earliest: from_early,
                completed_latest: to_late,
            });
        } else if !from_m.role.is_demolition() && to_m.role.is_demolition() {
            warnings.push(ConsistencyWarning::EventAfterDemolished {
                event_date: from_early,
                demolished_date: to_late,
            });
        } else {
            warnings.push(ConsistencyWarning::EventsOutOfOrder {
                earlier_date: to_late,
                later_date: from_early,
            });
        }
    }

    warnings
}

impl<E, S> Entity<E, S> {
    /// Check this entity for consistency issues.
    ///
    /// Temporal violations (`CompletionBeforeStart`, `EventsOutOfOrder`,
    /// `EventAfterDemolished`) are detected by checking structural lifecycle
    /// edges against date evidence — see [`find_violations`]. Non-temporal
    /// checks (`MultipleConstructions`, `NameValidityInverted`) are handled
    /// directly here.
    #[must_use]
    pub fn check_consistency(&self) -> Vec<ConsistencyWarning> {
        let mut warnings = find_violations(&decompose(&self.transitions));

        let construction_count = self
            .transitions
            .iter()
            .filter(|t| matches!(t, EntityTransition::Constructed { .. }))
            .count();
        if construction_count > 1 {
            warnings.push(ConsistencyWarning::MultipleConstructions {
                count: construction_count,
            });
        }

        for cited_name in &self.names {
            let name = &cited_name.value;
            if let (Some(from), Some(to)) = (&name.valid_from, &name.valid_to)
                && from.earliest() > to.latest()
            {
                warnings.push(ConsistencyWarning::NameValidityInverted {
                    name: name.name.clone(),
                    valid_from: from.clone(),
                    valid_to: to.clone(),
                });
            }
        }

        warnings
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::date::DatePrecision;
    use crate::entity::EntityType;
    use crate::evidence::Cited;
    use chrono::NaiveDate;

    // Tests use () for entity/source refs since consistency checking doesn't inspect them.
    type TestEntity = Entity<(), ()>;
    type TestTransition = EntityTransition<(), ()>;
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn midnight(y: i32, m: u32, d: u32) -> Result<NaiveDateTime, &'static str> {
        NaiveDate::from_ymd_opt(y, m, d)
            .ok_or("invalid date")?
            .and_hms_opt(0, 0, 0)
            .ok_or("invalid time")
    }

    fn year(y: i32) -> Result<UncertainDate, Box<dyn std::error::Error>> {
        Ok(UncertainDate::with_precision(
            midnight(y, 1, 1)?,
            DatePrecision::Year,
        )?)
    }

    #[test]
    fn completion_before_start() -> TestResult {
        let entity: TestEntity = Entity {
            entity_type: EntityType::Building,
            names: vec![],
            transitions: vec![TestTransition::Constructed {
                started_at: Some(Cited::uncited(UncertainDate::exact(midnight(1950, 1, 1)?)?)),
                completed_at: Some(Cited::uncited(UncertainDate::exact(midnight(1940, 1, 1)?)?)),
                location: None,
                trigger_event: None,
            }],
        };

        let warnings = entity.check_consistency();
        assert_eq!(warnings.len(), 1);
        assert!(matches!(
            warnings[0],
            ConsistencyWarning::CompletionBeforeStart { .. }
        ));
        Ok(())
    }

    #[test]
    fn completion_before_start_on_demolition() -> TestResult {
        let entity: TestEntity = Entity {
            entity_type: EntityType::Building,
            names: vec![],
            transitions: vec![TestTransition::Demolished {
                started_at: Some(Cited::uncited(UncertainDate::exact(midnight(1950, 1, 1)?)?)),
                completed_at: Some(Cited::uncited(UncertainDate::exact(midnight(1940, 1, 1)?)?)),
                cause: None,
                trigger_event: None,
            }],
        };

        let warnings = entity.check_consistency();
        assert_eq!(warnings.len(), 1);
        assert!(matches!(
            warnings[0],
            ConsistencyWarning::CompletionBeforeStart { .. }
        ));
        Ok(())
    }

    #[test]
    fn valid_construction() -> TestResult {
        let entity: TestEntity = Entity {
            entity_type: EntityType::Building,
            names: vec![],
            transitions: vec![TestTransition::Constructed {
                started_at: Some(Cited::uncited(UncertainDate::exact(midnight(1940, 1, 1)?)?)),
                completed_at: Some(Cited::uncited(UncertainDate::exact(midnight(1950, 1, 1)?)?)),
                location: None,
                trigger_event: None,
            }],
        };

        assert!(entity.check_consistency().is_empty());
        Ok(())
    }

    #[test]
    fn same_year_is_ok() -> TestResult {
        let entity: TestEntity = Entity {
            entity_type: EntityType::Building,
            names: vec![],
            transitions: vec![TestTransition::Constructed {
                started_at: Some(Cited::uncited(UncertainDate::with_precision(
                    midnight(1830, 6, 15)?,
                    DatePrecision::Year,
                )?)),
                completed_at: Some(Cited::uncited(UncertainDate::with_precision(
                    midnight(1830, 9, 20)?,
                    DatePrecision::Year,
                )?)),
                location: None,
                trigger_event: None,
            }],
        };

        assert!(
            entity.check_consistency().is_empty(),
            "Same year should not be flagged"
        );
        Ok(())
    }

    #[test]
    fn demolished_completed_at_fallback() -> TestResult {
        let entity: TestEntity = Entity {
            entity_type: EntityType::Building,
            names: vec![],
            transitions: vec![
                EntityTransition::Modified {
                    started_at: Some(Cited::uncited(UncertainDate::exact(midnight(2010, 1, 1)?)?)),
                    completed_at: None,
                    description: None,
                    trigger_event: None,
                },
                EntityTransition::Demolished {
                    started_at: None,
                    completed_at: Some(Cited::uncited(UncertainDate::exact(midnight(
                        2000, 1, 1,
                    )?)?)),
                    cause: None,
                    trigger_event: None,
                },
            ],
        };

        let warnings = entity.check_consistency();
        assert!(
            warnings
                .iter()
                .any(|w| matches!(w, ConsistencyWarning::EventAfterDemolished { .. })),
            "Should detect event after demolition when only completed_at is set"
        );
        Ok(())
    }

    #[test]
    fn reports_all_post_demolition_events() -> TestResult {
        let entity: TestEntity = Entity {
            entity_type: EntityType::Building,
            names: vec![],
            transitions: vec![
                EntityTransition::Demolished {
                    started_at: Some(Cited::uncited(UncertainDate::exact(midnight(2000, 1, 1)?)?)),
                    completed_at: None,
                    cause: None,
                    trigger_event: None,
                },
                EntityTransition::Modified {
                    started_at: Some(Cited::uncited(UncertainDate::exact(midnight(2005, 1, 1)?)?)),
                    completed_at: None,
                    description: None,
                    trigger_event: None,
                },
                EntityTransition::Repaired {
                    started_at: Some(Cited::uncited(UncertainDate::exact(midnight(2010, 1, 1)?)?)),
                    completed_at: None,
                    description: None,
                    trigger_event: None,
                },
            ],
        };

        let after_demolished = entity
            .check_consistency()
            .iter()
            .filter(|w| matches!(w, ConsistencyWarning::EventAfterDemolished { .. }))
            .count();
        assert_eq!(
            after_demolished, 2,
            "Should report both post-demolition events"
        );
        Ok(())
    }

    #[test]
    fn name_validity_inverted() -> TestResult {
        use crate::entity::{EntityName, NameType};
        use oxilangtag::LanguageTag;

        let entity: TestEntity = Entity {
            entity_type: EntityType::Building,
            names: vec![Cited::uncited(EntityName {
                name: "Old Name".to_string(),
                name_type: NameType::Historical,
                language: LanguageTag::parse("en".to_string())?,
                valid_from: Some(year(2000)?),
                valid_to: Some(year(1990)?),
            })],
            transitions: vec![],
        };

        let warnings = entity.check_consistency();
        assert_eq!(warnings.len(), 1);
        assert!(matches!(
            &warnings[0],
            ConsistencyWarning::NameValidityInverted { name, .. } if name == "Old Name"
        ));
        Ok(())
    }

    #[test]
    fn events_out_of_order() -> TestResult {
        // Structural vs. date contradiction: a Constructed dated 2000 with
        // a UsageModified dated 1900 — structurally the construction must
        // precede any usage change, but the dates say otherwise.
        let entity: TestEntity = Entity {
            entity_type: EntityType::Building,
            names: vec![],
            transitions: vec![
                EntityTransition::UsageModified {
                    occurred_at: Some(Cited::uncited(UncertainDate::exact(midnight(1900, 1, 1)?)?)),
                    new_usages: Default::default(),
                    description: None,
                    trigger_event: None,
                },
                EntityTransition::Constructed {
                    started_at: Some(Cited::uncited(UncertainDate::exact(midnight(2000, 1, 1)?)?)),
                    completed_at: None,
                    location: None,
                    trigger_event: None,
                },
            ],
        };

        let warnings = entity.check_consistency();
        assert!(
            warnings
                .iter()
                .any(|w| matches!(w, ConsistencyWarning::EventsOutOfOrder { .. })),
            "Should detect events listed out of chronological order"
        );
        Ok(())
    }

    #[test]
    fn event_after_demolished_with_only_completed_at() -> TestResult {
        let entity: TestEntity = Entity {
            entity_type: EntityType::Building,
            names: vec![],
            transitions: vec![
                EntityTransition::Demolished {
                    started_at: Some(Cited::uncited(UncertainDate::exact(midnight(2000, 1, 1)?)?)),
                    completed_at: None,
                    cause: None,
                    trigger_event: None,
                },
                EntityTransition::Modified {
                    started_at: None,
                    completed_at: Some(Cited::uncited(UncertainDate::exact(midnight(
                        2010, 1, 1,
                    )?)?)),
                    description: None,
                    trigger_event: None,
                },
            ],
        };

        let warnings = entity.check_consistency();
        assert!(
            warnings
                .iter()
                .any(|w| matches!(w, ConsistencyWarning::EventAfterDemolished { .. })),
            "modified completed_at=2010 after demolished=2000 should fire \
             EventAfterDemolished"
        );
        Ok(())
    }

    #[test]
    fn multiple_constructions() -> TestResult {
        let entity: TestEntity = Entity {
            entity_type: EntityType::Building,
            names: vec![],
            transitions: vec![
                EntityTransition::Constructed {
                    started_at: Some(Cited::uncited(UncertainDate::exact(midnight(1900, 1, 1)?)?)),
                    completed_at: None,
                    location: None,
                    trigger_event: None,
                },
                EntityTransition::Constructed {
                    started_at: Some(Cited::uncited(UncertainDate::exact(midnight(1950, 1, 1)?)?)),
                    completed_at: None,
                    location: None,
                    trigger_event: None,
                },
            ],
        };

        let warnings = entity.check_consistency();
        assert!(
            warnings
                .iter()
                .any(|w| matches!(w, ConsistencyWarning::MultipleConstructions { count: 2 })),
            "Should detect multiple construction events"
        );
        Ok(())
    }

    #[test]
    fn name_validity_normal_is_ok() -> TestResult {
        use crate::entity::{EntityName, NameType};
        use oxilangtag::LanguageTag;

        let entity: TestEntity = Entity {
            entity_type: EntityType::Building,
            names: vec![Cited::uncited(EntityName {
                name: "Current Name".to_string(),
                name_type: NameType::Official,
                language: LanguageTag::parse("en".to_string())?,
                valid_from: Some(year(1990)?),
                valid_to: Some(year(2000)?),
            })],
            transitions: vec![],
        };

        assert!(
            entity.check_consistency().is_empty(),
            "Valid name range should not be flagged"
        );
        Ok(())
    }
}
