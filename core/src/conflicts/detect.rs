//! Date-conflict detection primitives over a projected entity.
//!
//! [`fact_lineage`] tags each projected slot's support with the whole stored
//! fact ([`FactAtom`]), so a slot's over-determination reads straight off its
//! consensus support with no second fact-map lookup. [`fact_date`] recovers a
//! bookend or event date fact's own interval; [`minimal_fighting_sets`] splits
//! an over-determined slot's premises into the minimal sets whose joint meet is
//! ⊥.

use crate::algebra::semiring::Label;
use crate::date::UncertainDate;
use crate::grammar::assertions::FactualAssertion;
use crate::grammar::ids::{FactId, IdScheme};
use crate::grammar::{bookend, event};
use crate::nonempty::NonEmptyVec;
use crate::submit::StoredFact;

use super::minimize::minimize;

/// A provenance atom carrying one whole stored fact beside its id, so a slot's
/// support holds the fighting facts themselves.
///
/// Identity is the [`FactId`] alone — fact ids are unique, so keying on the id
/// dedups a fact to one atom in the [`Label`] and spares [`StoredFact`] an
/// `Ord`/`Hash` it doesn't carry.
#[derive(Debug, Clone)]
pub struct FactAtom<R: IdScheme> {
    /// The fact's id — the atom's whole identity.
    pub id: FactId,
    /// The stored fact the id names.
    pub fact: StoredFact<R>,
}

impl<R: IdScheme> PartialEq for FactAtom<R> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl<R: IdScheme> Eq for FactAtom<R> {}

impl<R: IdScheme> PartialOrd for FactAtom<R> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<R: IdScheme> Ord for FactAtom<R> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.id.cmp(&other.id)
    }
}

impl<R: IdScheme> std::hash::Hash for FactAtom<R> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

/// The provenance closure for a fact-carrying projection: the fact's atom is the
/// premise `{{(id, fact)}}`. A caller passes this to
/// [`project_entity`](crate::projection::project_entity) to get a projection whose
/// slot support carries the whole facts behind each field, which the typed
/// flatten reads for citations and a date-conflict pass reads for fighting dates.
pub fn fact_lineage<R: IdScheme>(
    fact_id: &FactId,
    _subject: &R::Entity,
    fact: &StoredFact<R>,
) -> Label<FactAtom<R>> {
    Label::premise(FactAtom {
        id: *fact_id,
        fact: fact.clone(),
    })
}

/// The date a bookend or event date fact carries. A date-conflict pass needs each
/// fighting fact's own interval back: the consensus meet reports only that a slot
/// bottomed out, not which facts' intervals are mutually disjoint.
///
/// Reading it side-blind (started vs completed) is sound because the projection
/// already sorts the facts by side — each routes to its own endpoint slot, so a
/// slot's support holds one side only, and the caller is always inside one slot.
pub(crate) fn fact_date<R: IdScheme>(fact: &StoredFact<R>) -> Option<UncertainDate> {
    let StoredFact::Factual(f) = fact else {
        return None;
    };
    let bound = match &f.assertion {
        FactualAssertion::Construction {
            fact:
                bookend::ConstructionFact::Started { bound, .. }
                | bookend::ConstructionFact::Completed { bound, .. },
        }
        | FactualAssertion::Demolition {
            fact:
                bookend::DemolitionFact::Started { bound, .. }
                | bookend::DemolitionFact::Completed { bound, .. },
        }
        | FactualAssertion::Event {
            fact: event::Fact::DurationalDate { bound, .. } | event::Fact::PointDate { bound, .. },
        } => bound,
        _ => return None,
    };
    Some(bound.clone())
}

/// Whether a stored fact is a [`ConstructionFact::Started`](bookend::ConstructionFact::Started)
/// claim — the fact a temporal conflict names as the floor a witness fell below,
/// and the direct assertion that tells an asserted construction start apart from
/// a derived "built by" bound.
pub(crate) fn is_construction_start<R: IdScheme>(fact: &StoredFact<R>) -> bool {
    matches!(
        fact,
        StoredFact::Factual(f)
            if matches!(
                &f.assertion,
                FactualAssertion::Construction {
                    fact: bookend::ConstructionFact::Started { .. },
                }
            )
    )
}

/// The minimal fighting sets among a slot's date premises, whose joint meet is
/// already ⊥.
///
/// Pairwise first: any two premises whose meet is empty are a minimal fighting
/// set on their own. For single-interval dates this is complete — Helly's theorem
/// in one dimension forces an empty total intersection to already contain a
/// disjoint pair — so every real projection resolves here.
///
/// The [`minimize`] backstop fires only when no pair is disjoint yet the whole
/// slot still conflicts: multi-interval (disjunctive) values, where three or more
/// premises can pairwise overlap and meet to empty only together. It returns the
/// one irreducible set. The two paths never both fire, so the result is dup-free.
pub(crate) fn minimal_fighting_sets(
    premises: &[(FactId, UncertainDate)],
) -> Vec<NonEmptyVec<FactId>> {
    let mut sets: Vec<NonEmptyVec<FactId>> = Vec::new();
    for (i, (id_a, date_a)) in premises.iter().enumerate() {
        for (id_b, date_b) in premises.iter().skip(i + 1) {
            if !date_a.overlaps(date_b) {
                let mut pair = NonEmptyVec::singleton(*id_a);
                pair.push(*id_b);
                sets.push(pair);
            }
        }
    }
    if sets.is_empty()
        && let Some(set) = minimize(premises)
    {
        sets.push(set);
    }
    sets
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeSet;

    use crate::date::DatePrecision;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn year(y: i32) -> Result<UncertainDate, Box<dyn std::error::Error>> {
        Ok(UncertainDate::with_precision(
            chrono::NaiveDate::from_ymd_opt(y, 1, 1).ok_or("date")?,
            DatePrecision::Year,
        )?)
    }

    // ------------------------------------------------------------------
    // Multi-interval backstop — three disjunctive dates that overlap
    // pairwise but meet to empty. Unreachable through the real projection
    // (submit stores only single intervals; Helly makes pairwise complete),
    // so it exercises `minimal_fighting_sets` on hand-built disjunctions.
    // ------------------------------------------------------------------

    #[test]
    fn pairwise_overlapping_disjunctions_fall_through_to_minimize() -> TestResult {
        // A = {1000, 2000}, B = {2000, 3000}, C = {1000, 3000}: each pair shares
        // one year (overlaps), all three share none (meet empty). No disjoint
        // pair exists, so the backstop must attribute all three together.
        let a = year(1000)?.join(&year(2000)?);
        let b = year(2000)?.join(&year(3000)?);
        let c = year(1000)?.join(&year(3000)?);
        assert!(
            a.overlaps(&b) && a.overlaps(&c) && b.overlaps(&c),
            "pairwise overlap"
        );

        let premises = vec![
            (FactId::new(1), a),
            (FactId::new(2), b),
            (FactId::new(3), c),
        ];
        let sets = minimal_fighting_sets(&premises);
        assert_eq!(
            sets.len(),
            1,
            "no disjoint pair, so the backstop fires once"
        );
        let contributing: BTreeSet<FactId> =
            sets.first().ok_or("no set")?.iter().copied().collect();
        assert_eq!(
            contributing,
            BTreeSet::from([FactId::new(1), FactId::new(2), FactId::new(3)]),
            "every disjunction is essential to the empty meet"
        );
        Ok(())
    }
}
