//! Entity consistency checking.
//!
//! Checks for logical inconsistencies in entity data. Inconsistencies are reported
//! but don't prevent data from being accepted.

use chrono::NaiveDateTime;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::date::UncertainDate;
use crate::entity::{Entity, EntityTransition};
use crate::evidence::Cited;

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

impl Entity {
    /// Check this entity for consistency issues.
    #[must_use]
    pub fn check_consistency(&self) -> Vec<ConsistencyWarning> {
        let mut warnings = Vec::new();

        for transition in &self.transitions {
            let (started, completed) = transition.date_range();
            if let Some(warning) = check_date_ordering(started, completed) {
                warnings.push(warning);
            }
        }

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

        warnings.extend(check_chronological_order(&self.transitions));
        warnings.extend(check_events_after_demolished(&self.transitions));

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

fn check_date_ordering(
    started: Option<&Cited<UncertainDate>>,
    completed: Option<&Cited<UncertainDate>>,
) -> Option<ConsistencyWarning> {
    let start_earliest = started.map(|c| c.value.earliest());
    let complete_latest = completed.map(|c| c.value.latest());

    match (start_earliest, complete_latest) {
        (Some(start), Some(complete)) if complete < start => {
            Some(ConsistencyWarning::CompletionBeforeStart {
                started_earliest: start,
                completed_latest: complete,
            })
        }
        _ => None,
    }
}

fn get_event_date(transition: &EntityTransition) -> Option<NaiveDateTime> {
    transition.event_date().map(|c| c.value.earliest())
}

fn check_chronological_order(transitions: &[EntityTransition]) -> Vec<ConsistencyWarning> {
    let dated_events: Vec<_> = transitions.iter().filter_map(get_event_date).collect();

    let mut warnings = Vec::new();
    for i in 1..dated_events.len() {
        let prev_date = &dated_events[i - 1];
        let curr_date = &dated_events[i];
        if curr_date < prev_date {
            warnings.push(ConsistencyWarning::EventsOutOfOrder {
                earlier_date: *curr_date,
                later_date: *prev_date,
            });
        }
    }
    warnings
}

fn check_events_after_demolished(transitions: &[EntityTransition]) -> Vec<ConsistencyWarning> {
    let demolished_date = transitions.iter().find_map(|t| {
        if let EntityTransition::Demolished {
            started_at,
            completed_at,
            ..
        } = t
        {
            started_at
                .as_ref()
                .map(|c| c.value.earliest())
                .or_else(|| completed_at.as_ref().map(|c| c.value.earliest()))
        } else {
            None
        }
    });

    let Some(demolished) = demolished_date else {
        return Vec::new();
    };

    let mut warnings = Vec::new();
    for transition in transitions {
        if matches!(transition, EntityTransition::Demolished { .. }) {
            continue;
        }

        if let Some(event_date) = get_event_date(transition)
            && event_date > demolished
        {
            warnings.push(ConsistencyWarning::EventAfterDemolished {
                event_date,
                demolished_date: demolished,
            });
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::date::{DateError, DatePrecision};
    use crate::entity::EntityType;
    use chrono::NaiveDate;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn midnight(y: i32, m: u32, d: u32) -> Result<NaiveDateTime, &'static str> {
        NaiveDate::from_ymd_opt(y, m, d)
            .ok_or("invalid date")?
            .and_hms_opt(0, 0, 0)
            .ok_or("invalid time")
    }

    fn year(y: i32) -> UncertainDate {
        UncertainDate::with_precision(midnight(y, 1, 1).unwrap(), DatePrecision::Year).unwrap()
    }

    #[test]
    fn completion_before_start() -> TestResult {
        let entity = Entity {
            entity_type: EntityType::Building,
            names: vec![],
            transitions: vec![EntityTransition::Constructed {
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
        let entity = Entity {
            entity_type: EntityType::Building,
            names: vec![],
            transitions: vec![EntityTransition::Demolished {
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
        let entity = Entity {
            entity_type: EntityType::Building,
            names: vec![],
            transitions: vec![EntityTransition::Constructed {
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
    fn same_year_is_ok() -> Result<(), DateError> {
        let entity = Entity {
            entity_type: EntityType::Building,
            names: vec![],
            transitions: vec![EntityTransition::Constructed {
                started_at: Some(Cited::uncited(UncertainDate::with_precision(
                    midnight(1830, 6, 15).expect("valid date"),
                    DatePrecision::Year,
                )?)),
                completed_at: Some(Cited::uncited(UncertainDate::with_precision(
                    midnight(1830, 9, 20).expect("valid date"),
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
        let entity = Entity {
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
        let entity = Entity {
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

        let entity = Entity {
            entity_type: EntityType::Building,
            names: vec![Cited::uncited(EntityName {
                name: "Old Name".to_string(),
                name_type: NameType::Historical,
                language: LanguageTag::parse("en".to_string())?,
                valid_from: Some(year(2000)),
                valid_to: Some(year(1990)),
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
        let entity = Entity {
            entity_type: EntityType::Building,
            names: vec![],
            transitions: vec![
                EntityTransition::Modified {
                    started_at: Some(Cited::uncited(UncertainDate::exact(midnight(2000, 1, 1)?)?)),
                    completed_at: None,
                    description: Some("renovation".to_string()),
                    trigger_event: None,
                },
                EntityTransition::Constructed {
                    started_at: Some(Cited::uncited(UncertainDate::exact(midnight(1900, 1, 1)?)?)),
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
    fn multiple_constructions() -> TestResult {
        let entity = Entity {
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

        let entity = Entity {
            entity_type: EntityType::Building,
            names: vec![Cited::uncited(EntityName {
                name: "Current Name".to_string(),
                name_type: NameType::Official,
                language: LanguageTag::parse("en".to_string())?,
                valid_from: Some(year(1990)),
                valid_to: Some(year(2000)),
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
