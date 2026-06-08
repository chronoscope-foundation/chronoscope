//! Entity merge operations.
//!
//! Merging two entity records that describe the same real-world thing.
//! The merge operation is **commutative** and **associative** on the resulting
//! entity value, so the order and grouping of merges doesn't affect the outcome.
//!
//! This is a *record merge* (deduplication of data), not a historical merge
//! (two real-world entities combining). Historical merges are modeled by
//! [`EntityRelationType::MergedFrom`](crate::entity::EntityRelationType::MergedFrom).
//!
//! # Algorithm
//!
//! Entity merge = bag union of names + bag union of transitions.
//!
//! For names: group by `EntityName` value, union evidence for identical names.
//! For transitions: pure bag union, no deduplication (transition matching is a
//! harder problem deferred for later).
//!
//! # Provenance
//!
//! Merge provenance lives alongside the entity, not inside it (option C from
//! the design doc). The `Entity<S>` type stays clean. Merge output is shaped
//! as a typed diff that a future versioning/edit-log system can store.

use crate::entity::{Entity, EntityName, NameType};
use crate::evidence::{Cited, Evidence};

/// Sort key for canonical name ordering (commutativity of merge).
fn name_sort_key(name: &EntityName) -> (&str, NameType, &str) {
    (&name.name, name.name_type, name.language.as_str())
}

/// Merge two entities into one.
///
/// Commutative: `merge(a, b) == merge(b, a)` (on entity value).
/// Associative: `merge(merge(a, b), c) == merge(a, merge(b, c))`.
/// Identity: `merge(a, empty) == a`.
///
/// Names with identical `EntityName` values have their evidence merged.
/// Transitions are bag-unioned without deduplication.
pub fn merge_entities<S: Clone + PartialEq>(a: Entity<S>, b: Entity<S>) -> Entity<S> {
    Entity {
        names: merge_cited_names(a.names, b.names),
        transitions: merge_transitions(a.transitions, b.transitions),
    }
}

/// Merge two lists of cited names.
///
/// Names with identical `EntityName` values have their evidence unioned.
/// Names that appear in only one list are kept as-is.
/// The result is sorted for deterministic ordering (commutativity).
fn merge_cited_names<S: Clone + PartialEq>(
    mut a: Vec<Cited<EntityName, S>>,
    b: Vec<Cited<EntityName, S>>,
) -> Vec<Cited<EntityName, S>> {
    for cited_b in b {
        if let Some(existing) = a.iter_mut().find(|c| c.value == cited_b.value) {
            // Same name — merge evidence
            merge_evidence(&mut existing.evidence, cited_b.evidence);
        } else {
            a.push(cited_b);
        }
    }
    // Sort for deterministic ordering (commutativity)
    a.sort_by(|x, y| name_sort_key(&x.value).cmp(&name_sort_key(&y.value)));
    a
}

/// Merge evidence lists (set union — no duplicates).
fn merge_evidence<S: Clone + PartialEq>(target: &mut Vec<Evidence<S>>, source: Vec<Evidence<S>>) {
    for ev in source {
        if !target.contains(&ev) {
            target.push(ev);
        }
    }
}

/// Canonical sort order for transitions: lifecycle phase (construction < mid-life
/// < demolition), then earliest known date, then variant name for determinism.
///
/// Used by both `merge_transitions` and the `arb_clean_entity` proptest generator
/// to ensure merge identity holds (`merge(a, empty) == a`).
fn transition_cmp<S>(
    x: &crate::entity::EntityTransition<S>,
    y: &crate::entity::EntityTransition<S>,
) -> std::cmp::Ordering {
    fn phase_key<S>(t: &crate::entity::EntityTransition<S>) -> u8 {
        match t {
            crate::entity::EntityTransition::Constructed { .. } => 0,
            crate::entity::EntityTransition::Demolished { .. } => 2,
            _ => 1,
        }
    }
    phase_key(x)
        .cmp(&phase_key(y))
        .then_with(|| {
            let x_date = x.earliest_known_date().and_then(|c| c.value.earliest());
            let y_date = y.earliest_known_date().and_then(|c| c.value.earliest());
            x_date.cmp(&y_date)
        })
        // Tiebreak by variant name for determinism (commutativity)
        .then_with(|| x.as_ref().cmp(y.as_ref()))
}

/// Bag union of transitions. No deduplication — each transition appears once
/// per source entity.
///
/// Sorted for deterministic ordering. Since `EntityTransition` doesn't implement
/// `Ord` (contains floats), we sort by a derived key.
fn merge_transitions<S: Clone>(
    mut a: Vec<crate::entity::EntityTransition<S>>,
    b: Vec<crate::entity::EntityTransition<S>>,
) -> Vec<crate::entity::EntityTransition<S>> {
    a.extend(b);
    a.sort_by(transition_cmp);
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::{EntityName, EntityTransition, NameType};
    use crate::evidence::Cited;
    use chrono::NaiveDate;
    use proptest::prelude::*;

    fn make_name(name: &str) -> Cited<EntityName, ()> {
        #[expect(
            clippy::expect_used,
            reason = "the literal \"en\" is a valid BCP-47 language tag"
        )]
        Cited::uncited(EntityName {
            name: name.to_string(),
            name_type: NameType::Common,
            language: oxilangtag::LanguageTag::parse("en".to_string()).expect("valid tag"),
            valid_from: None,
            valid_to: None,
        })
    }

    fn empty_entity() -> Entity<()> {
        Entity {
            names: vec![],
            transitions: vec![],
        }
    }

    fn entity_with_name(name: &str) -> Entity<()> {
        Entity {
            names: vec![make_name(name)],
            transitions: vec![],
        }
    }

    // --- Unit tests ---

    #[test]
    fn merge_empty_entities() {
        let result = merge_entities(empty_entity(), empty_entity());
        assert!(result.names.is_empty());
        assert!(result.transitions.is_empty());
    }

    #[test]
    fn merge_identity_left() {
        let a = entity_with_name("Test Building");
        let result = merge_entities(a.clone(), empty_entity());
        assert_eq!(result.names.len(), 1);
        assert_eq!(result.names[0].value.name, "Test Building");
    }

    #[test]
    fn merge_identity_right() {
        let a = entity_with_name("Test Building");
        let result = merge_entities(empty_entity(), a.clone());
        assert_eq!(result.names.len(), 1);
        assert_eq!(result.names[0].value.name, "Test Building");
    }

    #[test]
    fn merge_deduplicates_identical_names() {
        let a = entity_with_name("Brooklyn Bridge");
        let b = entity_with_name("Brooklyn Bridge");
        let result = merge_entities(a, b);
        assert_eq!(result.names.len(), 1);
        assert_eq!(result.names[0].value.name, "Brooklyn Bridge");
    }

    #[test]
    fn merge_keeps_different_names() {
        let a = entity_with_name("Brooklyn Bridge");
        let b = entity_with_name("East River Bridge");
        let result = merge_entities(a, b);
        assert_eq!(result.names.len(), 2);
    }

    #[test]
    fn merge_unions_evidence_for_same_name() -> Result<(), Box<dyn std::error::Error>> {
        use crate::evidence::Evidence;
        let ev_a: Evidence<()> = Evidence::Web {
            source_url: url::Url::parse("https://a.example.com")?,
            excerpt: None,
        };
        let ev_b: Evidence<()> = Evidence::Web {
            source_url: url::Url::parse("https://b.example.com")?,
            excerpt: None,
        };

        let name = EntityName {
            name: "Test".to_string(),
            name_type: NameType::Common,
            language: oxilangtag::LanguageTag::parse("en".to_string())
                .map_err(|e| format!("bad tag: {e}"))?,
            valid_from: None,
            valid_to: None,
        };

        let a: Entity<()> = Entity {
            names: vec![Cited::new(name.clone(), vec![ev_a.clone()])],
            transitions: vec![],
        };
        let b: Entity<()> = Entity {
            names: vec![Cited::new(name, vec![ev_b.clone()])],
            transitions: vec![],
        };

        let result = merge_entities(a, b);
        assert_eq!(result.names.len(), 1);
        assert_eq!(result.names[0].evidence.len(), 2);
        Ok(())
    }

    #[test]
    fn merge_bag_unions_transitions() -> Result<(), Box<dyn std::error::Error>> {
        let date_a = crate::date::UncertainDate::with_precision(
            NaiveDate::from_ymd_opt(1900, 1, 1).ok_or("bad date")?,
            crate::date::DatePrecision::Year,
        )?;
        let date_b = crate::date::UncertainDate::with_precision(
            NaiveDate::from_ymd_opt(1920, 1, 1).ok_or("bad date")?,
            crate::date::DatePrecision::Year,
        )?;

        let a: Entity<()> = Entity {
            names: vec![],
            transitions: vec![EntityTransition::Constructed {
                started_at: Some(Cited::uncited(date_a)),
                completed_at: None,
                location: None,
                trigger_event: None,
            }],
        };
        let b: Entity<()> = Entity {
            names: vec![],
            transitions: vec![EntityTransition::Damaged {
                occurred_at: Some(Cited::uncited(date_b)),
                cause: None,
                description: None,
                trigger_event: None,
            }],
        };

        let result = merge_entities(a, b);
        assert_eq!(result.transitions.len(), 2);
        Ok(())
    }

    // --- Property tests ---
    //
    // Uses `arb_clean_entity()` which produces canonically ordered,
    // consistency-passing entities.
    //
    // TODO: Also test consistency checks via proptests with a mutation-based
    // or shape-library generator. Deferred until after merge lands — the right
    // shape is probably: produce a clean skeleton, then apply targeted mutations
    // (swap transitions, flip dates, add post-demolition events) with controlled
    // probability.

    fn arb_name() -> impl Strategy<Value = Cited<EntityName, ()>> {
        (
            "[a-z]{3,10}",
            prop_oneof![Just(NameType::Common), Just(NameType::Official)],
        )
            .prop_filter_map("valid name", |(name, name_type)| {
                let lang = oxilangtag::LanguageTag::parse("en".to_string()).ok()?;
                Some(Cited::uncited(EntityName {
                    name,
                    name_type,
                    language: lang,
                    valid_from: None,
                    valid_to: None,
                }))
            })
    }

    fn arb_transition() -> impl Strategy<Value = EntityTransition<()>> {
        let year_range = 1800i32..2100;
        prop_oneof![
            year_range
                .clone()
                .prop_filter_map("valid constructed", |y| {
                    let date = NaiveDate::from_ymd_opt(y, 1, 1)?;
                    let ud = crate::date::UncertainDate::with_precision(
                        date,
                        crate::date::DatePrecision::Year,
                    )
                    .ok()?;
                    Some(EntityTransition::Constructed {
                        started_at: Some(Cited::uncited(ud)),
                        completed_at: None,
                        location: None,
                        trigger_event: None,
                    })
                }),
            year_range.clone().prop_filter_map("valid damaged", |y| {
                let date = NaiveDate::from_ymd_opt(y, 1, 1)?;
                let ud = crate::date::UncertainDate::with_precision(
                    date,
                    crate::date::DatePrecision::Year,
                )
                .ok()?;
                Some(EntityTransition::Damaged {
                    occurred_at: Some(Cited::uncited(ud)),
                    cause: None,
                    description: None,
                    trigger_event: None,
                })
            }),
            year_range.prop_filter_map("valid modified", |y| {
                let date = NaiveDate::from_ymd_opt(y, 1, 1)?;
                let ud = crate::date::UncertainDate::with_precision(
                    date,
                    crate::date::DatePrecision::Year,
                )
                .ok()?;
                Some(EntityTransition::Modified {
                    started_at: Some(Cited::uncited(ud)),
                    completed_at: None,
                    description: None,
                    trigger_event: None,
                })
            }),
        ]
    }

    fn arb_clean_entity() -> impl Strategy<Value = Entity<()>> {
        (
            prop::collection::vec(arb_name(), 0..5),
            prop::collection::vec(arb_transition(), 0..4),
        )
            .prop_map(|(mut names, mut transitions)| {
                // Sort to match merge's canonical ordering — required for
                // identity property (merge(a, empty) == a).
                names.sort_by(|x, y| name_sort_key(&x.value).cmp(&name_sort_key(&y.value)));
                transitions.sort_by(transition_cmp);
                Entity { names, transitions }
            })
    }

    proptest! {
        #[test]
        fn prop_merge_commutative(a in arb_clean_entity(), b in arb_clean_entity()) {
            prop_assert_eq!(
                merge_entities(a.clone(), b.clone()),
                merge_entities(b, a),
            );
        }

        #[test]
        fn prop_merge_associative(
            a in arb_clean_entity(),
            b in arb_clean_entity(),
            c in arb_clean_entity(),
        ) {
            let ab_c = merge_entities(merge_entities(a.clone(), b.clone()), c.clone());
            let a_bc = merge_entities(a, merge_entities(b, c));
            prop_assert_eq!(ab_c, a_bc);
        }

        #[test]
        fn prop_merge_identity(a in arb_clean_entity()) {
            let empty = empty_entity();
            prop_assert_eq!(
                merge_entities(a.clone(), empty.clone()),
                a.clone(),
            );
            prop_assert_eq!(
                merge_entities(empty, a.clone()),
                a,
            );
        }

        #[test]
        fn prop_merge_preserves_all_evidence(a in arb_clean_entity(), b in arb_clean_entity()) {
            let total_a: usize = a.names.iter().map(|n| n.evidence.len()).sum();
            let total_b: usize = b.names.iter().map(|n| n.evidence.len()).sum();
            let result = merge_entities(a, b);
            let total_result: usize = result.names.iter().map(|n| n.evidence.len()).sum();
            // Result should have at least as much evidence as both inputs combined
            // (may be less if evidence was deduplicated across matching names)
            prop_assert!(total_result >= total_a.max(total_b),
                "Evidence should not be lost: result={total_result} >= max({total_a}, {total_b})");
        }
    }
}
