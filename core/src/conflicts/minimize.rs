//! Minimal-unsatisfiable-subset extraction over a meet-semilattice: the
//! deletion-based primitive the detector uses to reduce an over-determined
//! slot's fact set to a minimal fighting set.
//!
//! Given premises whose joint meet is ⊥, [`minimize`] returns one
//! deletion-minimal subset that still meets to ⊥ — a set where retracting any
//! single member pulls the meet off ⊥. The detector attributes exactly that
//! set as the conflict's contributors, so no redundant premise rides along.

use std::collections::BTreeSet;

use crate::algebra::lattice::{BoundedLattice, JoinSemilattice, MeetSemilattice};
use crate::grammar::ids::FactId;
use crate::nonempty::NonEmptyVec;

/// Reduce premises whose joint meet is ⊥ to one minimal unsatisfiable subset
/// (MUS): an irreducible fact set that still meets to ⊥, where retracting any
/// single member lifts the meet off ⊥.
///
/// Returns `None` when the joint meet of the premises is not ⊥ — the slot is
/// consistent, so there is no fighting set to attribute. `Some` carries the
/// fighting set: a subset of the input fact ids where removing any one member
/// lifts the meet off ⊥.
///
/// Deterministic: premises iterate in [`FactId`] order, so the MUS the deletion
/// pass settles on is canonical regardless of the order the caller found them
/// in. Two callers who discover the same premises in different orders get
/// identical results.
///
/// Algorithm: deletion-based. Walk the survivors in id order; for each premise,
/// meet the remainder without it — if that meet is still ⊥ the premise was
/// redundant and stays out, otherwise it is essential and stays. `O(n²)` meets
/// in the worst case, fine at the handful-of-premises scale a single
/// contradiction carries.
///
/// Minimal, not minimum: the result is irreducible, not smallest-cardinality,
/// and different deletion orders can settle on different irreducible sets. Any
/// irreducible set is a valid explanation — retract its members and the
/// conflict clears — and the id order makes the choice canonical. For
/// single-interval dates, Helly's theorem in one dimension forces an empty
/// total intersection to already contain a disjoint *pair*, so the MUS is
/// exactly size 2; larger irreducible sets arise only for multi-interval
/// (disjunctive) values. The `keep.len() == 1` floor is the general case — you
/// can't drop the last survivor — though a single real date is never ⊥ alone,
/// so results are ≥ 2 in practice.
pub fn minimize<T>(premises: &[(FactId, T)]) -> Option<NonEmptyVec<FactId>>
where
    T: BoundedLattice + Clone,
{
    let mut keep: BTreeSet<FactId> = premises.iter().map(|(fact, _)| *fact).collect();

    // Meet is monotone: if the full set doesn't reach ⊥, no subset can, and the
    // deletion pass below would drop nothing and hand back the whole input as a
    // bogus "minimal" set. Reject the non-over-determined slot instead.
    if !is_bottom(&meet_of(premises, &keep, None)) {
        return None;
    }

    // Snapshot the survivors up front: stable, `FactId`-sorted iteration order,
    // and immune to the removals the loop makes to `keep`.
    for x in keep.iter().copied().collect::<Vec<_>>() {
        // Never drop the last survivor — a single-premise set is minimal.
        if keep.len() == 1 {
            break;
        }
        // Test the removal against a lazy filtered meet; commit only when it
        // stays ⊥, so `x` never has to be speculatively reinserted.
        if is_bottom(&meet_of(premises, &keep, Some(x))) {
            keep.remove(&x);
        }
    }

    NonEmptyVec::try_from_vec(keep.into_iter().collect()).ok()
}

/// A value has collapsed to ⊥, by the lattice's own `is_bottom` predicate.
fn is_bottom<T: JoinSemilattice>(v: &T) -> bool {
    v.is_bottom()
}

/// Meet the premise values that survive `keep` and aren't `skip`, folding
/// through [`MeetSemilattice::meet_all`] from ⊤. The lazy filter tests a
/// candidate removal without mutating `keep`. An empty selection yields ⊤ (the
/// vacuously-satisfiable meet), which [`minimize`] never asks for.
fn meet_of<T>(premises: &[(FactId, T)], keep: &BTreeSet<FactId>, skip: Option<FactId>) -> T
where
    T: MeetSemilattice + Clone,
{
    T::meet_all(
        premises
            .iter()
            .filter(|(fact, _)| keep.contains(fact) && Some(*fact) != skip)
            .map(|(_, value)| value.clone()),
    )
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use proptest::prelude::*;

    use crate::algebra::lattice::JoinSemilattice;
    use crate::algebra::monoid::CommutativeMonoid;
    use crate::date::{DatePrecision, UncertainDate};
    use crate::projection::Claimed;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn year(y: i32) -> Result<UncertainDate, Box<dyn std::error::Error>> {
        Ok(UncertainDate::with_precision(
            NaiveDate::from_ymd_opt(y, 1, 1).ok_or("date")?,
            DatePrecision::Year,
        )?)
    }

    // -- UncertainDate-specific behaviour ---------------------------------

    #[test]
    fn disjoint_years_minimize_to_both_facts() -> TestResult {
        let d1850 = year(1850)?;
        let d1860 = year(1860)?;
        assert!(
            is_bottom(&UncertainDate::meet(&d1850, &d1860)),
            "two disjoint years must meet to bottom"
        );
        let premises = vec![(FactId::new(1), d1850), (FactId::new(2), d1860)];
        let keep = minimize(&premises).ok_or("expected Some")?;
        assert_eq!(
            keep.into_vec(),
            vec![FactId::new(1), FactId::new(2)],
            "each disjoint year is essential, so both facts survive"
        );
        Ok(())
    }

    // -- The Claimed set lattice ------------------------------------------

    /// `minimize` over the `Claimed` set lattice: two premises with disjoint
    /// sets meet (set intersection) to the empty `Of`, which `is_bottom`
    /// recognizes as ⊥. Each premise is essential — dropping either lifts the
    /// meet off ⊥ — so both facts survive, driving the deletion pass through
    /// `is_bottom` on a set carrier.
    #[test]
    fn disjoint_claimed_sets_minimize_to_both_facts() -> TestResult {
        let left: Claimed<u8> = Claimed::Of {
            values: [1, 2].into_iter().collect(),
        };
        let right: Claimed<u8> = Claimed::Of {
            values: [3].into_iter().collect(),
        };
        let premises = vec![(FactId::new(1), left), (FactId::new(2), right)];
        assert!(
            is_bottom(&meet_of(&premises, &all_facts_claimed(&premises), None)),
            "disjoint claimed sets meet to the empty Of, which is bottom"
        );
        let keep = minimize(&premises).ok_or("expected Some")?;
        assert_eq!(
            keep.into_vec(),
            vec![FactId::new(1), FactId::new(2)],
            "each disjoint claim is essential, so both facts survive"
        );
        Ok(())
    }

    fn all_facts_claimed(premises: &[(FactId, Claimed<u8>)]) -> BTreeSet<FactId> {
        premises.iter().map(|(fact, _)| *fact).collect()
    }

    // -- Bitset lattice for the property suite ----------------------------

    /// A `u8`-backed bounded lattice: ⊤ is all bits, meet is bitwise AND; ⊥ is
    /// zero, join is bitwise OR. Small enough to enumerate contradictions by
    /// construction.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Bitset(u8);

    impl CommutativeMonoid for Bitset {
        fn identity() -> Self {
            Self(0)
        }
        fn combine(self, other: Self) -> Self {
            Self(self.0 | other.0)
        }
    }

    impl JoinSemilattice for Bitset {
        fn is_bottom(&self) -> bool {
            self.0 == 0
        }
    }

    impl MeetSemilattice for Bitset {
        fn top() -> Self {
            Self(0xFF)
        }
        fn meet(self, other: Self) -> Self {
            Self(self.0 & other.0)
        }
    }

    /// Deterministic Fisher-Yates over a seed — a permutation without an RNG
    /// dependency, used to assert order-independence.
    fn shuffle<T>(values: &mut [T], seed: u64) {
        let mut state = seed.wrapping_add(0x9E37_79B9_7F4A_7C15) | 1;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for i in (1..values.len()).rev() {
            let j = (next() as usize) % (i + 1);
            values.swap(i, j);
        }
    }

    /// Generate premises whose joint meet is ⊥ where every essential premise
    /// clears a bit no other premise clears, alongside a batch of redundant
    /// padding premises the deletion pass must discard.
    ///
    /// Construction: pick `k` distinct bits and let `base` be their OR. The
    /// `k` essential premises are each `base` with one distinct bit cleared —
    /// their bitwise AND zeroes every chosen bit (⊥), and restoring any one
    /// premise leaves that premise's bit set (non-⊥), so each is essential.
    /// The padding premises are supersets of `base` (every chosen bit set plus
    /// noise outside `base`), so they clear nothing and are redundant under
    /// AND. Fact ids are assigned by shuffled position; the second element
    /// records which fact ids hold padding.
    fn arb_bottom_premises_with_padding()
    -> impl Strategy<Value = (Vec<(FactId, Bitset)>, Vec<FactId>)> {
        (2usize..=6usize)
            .prop_flat_map(|k| {
                let bits = prop::collection::vec(0u8..8u8, k).prop_filter("distinct bits", |v| {
                    let mut s = v.clone();
                    s.sort_unstable();
                    s.dedup();
                    s.len() == v.len()
                });
                let pad_seeds =
                    (0usize..=4usize).prop_flat_map(|p| prop::collection::vec(any::<u8>(), p));
                let shuffle_seed = any::<u64>();
                (bits, pad_seeds, shuffle_seed)
            })
            .prop_map(|(bits, pad_seeds, shuffle_seed)| {
                let base: u8 = bits.iter().fold(0u8, |acc, &b| acc | (1u8 << b));
                let essentials: Vec<Bitset> =
                    bits.iter().map(|&b| Bitset(base & !(1u8 << b))).collect();
                let padding: Vec<Bitset> = pad_seeds
                    .iter()
                    .map(|seed| Bitset(base | (*seed & !base)))
                    .collect();

                // Tag essentials `false`, padding `true`; shuffle so padding is
                // interleaved rather than trailing.
                let mut tagged: Vec<(bool, Bitset)> =
                    essentials.into_iter().map(|v| (false, v)).collect();
                tagged.extend(padding.into_iter().map(|v| (true, v)));
                shuffle(&mut tagged, shuffle_seed);

                let mut padding_facts: Vec<FactId> = Vec::new();
                let mut premises: Vec<(FactId, Bitset)> = Vec::with_capacity(tagged.len());
                for (idx, (is_padding, value)) in tagged.into_iter().enumerate() {
                    let fact = FactId::new(idx as u64 + 1);
                    premises.push((fact, value));
                    if is_padding {
                        padding_facts.push(fact);
                    }
                }
                (premises, padding_facts)
            })
    }

    /// Companion strategy dropping the padding witness, for properties that
    /// only care about the premise vector.
    fn arb_bottom_premises() -> impl Strategy<Value = Vec<(FactId, Bitset)>> {
        arb_bottom_premises_with_padding().prop_map(|(premises, _)| premises)
    }

    fn all_facts(premises: &[(FactId, Bitset)]) -> BTreeSet<FactId> {
        premises.iter().map(|(fact, _)| *fact).collect()
    }

    #[test]
    fn non_bottom_meet_yields_none() {
        // 0b1100 AND 0b0110 = 0b0100: the premises still agree on a value, so
        // the slot is consistent and there is no fighting set to attribute.
        let premises = vec![
            (FactId::new(1), Bitset(0b1100)),
            (FactId::new(2), Bitset(0b0110)),
        ];
        assert!(
            !is_bottom(&meet_of(&premises, &all_facts(&premises), None)),
            "the joint meet is 0b0100, which is not bottom"
        );
        assert!(
            minimize(&premises).is_none(),
            "a non-bottom joint meet has no minimal fighting set"
        );
    }

    proptest! {
        #[test]
        fn prop_minimize_returns_unique_subset(premises in arb_bottom_premises()) {
            let inputs = all_facts(&premises);
            let Some(keep) = minimize(&premises) else {
                prop_assert!(false, "generator guarantees a bottom meet");
                return Ok(());
            };
            let keep = keep.into_vec();
            for fact in &keep {
                prop_assert!(inputs.contains(fact), "minimize returned a foreign fact id");
            }
            let unique: BTreeSet<FactId> = keep.iter().copied().collect();
            prop_assert_eq!(unique.len(), keep.len(), "minimize returned duplicates");
        }

        #[test]
        fn prop_minimize_result_is_still_bottom(premises in arb_bottom_premises()) {
            // The generator always bottoms out; assert the premise anyway so a
            // future generator change can't silently void the property.
            prop_assume!(is_bottom(&meet_of(&premises, &all_facts(&premises), None)));
            let Some(keep) = minimize(&premises) else {
                prop_assert!(false, "generator guarantees a bottom meet");
                return Ok(());
            };
            let keep = keep.into_vec();
            prop_assert!(!keep.is_empty(), "a bottom input yields a non-empty MUS");
            let kept: BTreeSet<FactId> = keep.iter().copied().collect();
            prop_assert!(
                is_bottom(&meet_of(&premises, &kept, None)),
                "the minimized subset must still meet to bottom"
            );
        }

        #[test]
        fn prop_minimize_is_minimal(premises in arb_bottom_premises()) {
            prop_assume!(is_bottom(&meet_of(&premises, &all_facts(&premises), None)));
            let Some(keep) = minimize(&premises) else {
                prop_assert!(false, "generator guarantees a bottom meet");
                return Ok(());
            };
            let keep = keep.into_vec();
            // A one-element MUS is minimal outright.
            prop_assume!(keep.len() >= 2);
            let kept: BTreeSet<FactId> = keep.iter().copied().collect();
            for &drop in &keep {
                prop_assert!(
                    !is_bottom(&meet_of(&premises, &kept, Some(drop))),
                    "removing a kept fact must lift the meet off bottom"
                );
            }
        }

        #[test]
        fn prop_minimize_drops_padding(
            (premises, padding) in arb_bottom_premises_with_padding(),
        ) {
            prop_assume!(is_bottom(&meet_of(&premises, &all_facts(&premises), None)));
            let Some(keep) = minimize(&premises) else {
                prop_assert!(false, "generator guarantees a bottom meet");
                return Ok(());
            };
            let keep = keep.into_vec();
            for pad in &padding {
                prop_assert!(
                    !keep.contains(pad),
                    "minimize kept redundant padding fact {pad}"
                );
            }
        }

        #[test]
        fn prop_minimize_is_order_independent(
            premises in arb_bottom_premises(),
            seed in any::<u64>(),
        ) {
            let mut shuffled = premises.clone();
            shuffle(&mut shuffled, seed);
            prop_assert_eq!(
                minimize(&premises),
                minimize(&shuffled),
                "shuffling the premise order must not change the MUS"
            );
        }
    }
}
