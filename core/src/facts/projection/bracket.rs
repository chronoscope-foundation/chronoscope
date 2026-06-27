//! [`Bracket`] — a restrictive estimate as a certain/possible interval over a
//! value lattice, with provenance threaded per bound.
//!
//! A bracket pairs two derivations of one field: the **consensus** (the meet of
//! every claim — the value all sources agree on) and the **extent** (their join
//! — the value any source allows). Together they bound the truth from both
//! sides, the certain/possible interval of incomplete-information databases
//! (Imieliński & Lipski 1984; Libkin 2011). Over-determination — the consensus
//! collapsing to ⊥ — is the bilattice's ⊤, a paraconsistent conflict rather than
//! an error (Belnap 1977; Ginsberg 1988; Fitting 1991).

use crate::algebra::lattice::BoundedLattice;
use crate::algebra::monoid::CommutativeMonoid;
use crate::algebra::semiring::Semiring;
use crate::claimed::Claimed;
use crate::date::UncertainDate;
use crate::location::{ConflictStatus, UnresolvedLocation};

use super::provenance::Cited;

/// A restrictive field: the consensus (meet, `·`) and extent (join, `+`) of
/// every claim that spoke to it.
///
/// Combining brackets meets the consensus and joins the extent, each carrying
/// its support by the matching semiring op. The field conflicts when the
/// consensus bottoms out — every source agreeing on nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bracket<V, T> {
    /// What all claims agree on (the meet). Bottoms out under conflict.
    pub consensus: Cited<V, T>,
    /// What any claim allows (the join).
    pub extent: Cited<V, T>,
}

/// A single claim: both bounds are the value itself, each backed by `support`.
/// The consensus and extent of one claim coincide; they only part once a second
/// claim combines in.
impl<V: Clone, T: Clone> From<(V, T)> for Bracket<V, T> {
    fn from((value, support): (V, T)) -> Self {
        Self {
            consensus: Cited {
                value: value.clone(),
                support: support.clone(),
            },
            extent: Cited { value, support },
        }
    }
}

/// The knowledge-order merge: consensus ⊤ / extent ⊥ as the seed, and a combine
/// that narrows the consensus by meet and widens the extent by join, each
/// support combined by the matching semiring op. A lawful semilattice when `T`
/// is idempotent (the lineage instance), so the whole-entity fold rides on it.
impl<V, T> CommutativeMonoid for Bracket<V, T>
where
    V: BoundedLattice,
    T: Semiring,
{
    fn identity() -> Self {
        Self {
            consensus: Cited {
                value: V::top(),
                support: T::one(),
            },
            extent: Cited {
                value: V::bottom(),
                support: T::zero(),
            },
        }
    }

    fn combine(self, other: Self) -> Self {
        Self {
            consensus: Cited {
                value: self.consensus.value.meet(other.consensus.value),
                support: self.consensus.support.times(other.consensus.support),
            },
            extent: Cited {
                value: self.extent.value.join(other.extent.value),
                support: self.extent.support.plus(other.extent.support),
            },
        }
    }
}

/// Whether a lattice value, read as a bracket's consensus, denotes
/// over-determination — and, for locations, whether resolution might still
/// settle it.
///
/// Structural for dates and value-mode payloads (the consensus is ⊥ exactly when
/// it admits nothing); geometric and 3-valued for locations, where an unresolved
/// reference leaves the verdict [`Pending`](ConflictStatus::Pending).
pub trait ConsensusConflict {
    /// The conflict verdict for this consensus value.
    fn conflict(&self) -> ConflictStatus;
}

impl ConsensusConflict for UncertainDate {
    fn conflict(&self) -> ConflictStatus {
        if self.intervals().is_empty() {
            ConflictStatus::Conflict
        } else {
            ConflictStatus::Consistent
        }
    }
}

impl<A: Ord> ConsensusConflict for Claimed<A> {
    fn conflict(&self) -> ConflictStatus {
        match self {
            Claimed::Of(values) if values.is_empty() => ConflictStatus::Conflict,
            Claimed::Of(_) | Claimed::Any => ConflictStatus::Consistent,
        }
    }
}

impl ConsensusConflict for UnresolvedLocation {
    /// The geometric candidate-point routine, capped so a resolved reference
    /// can't drive the cubic emptiness check past
    /// [`MAX_PROJECTED_LOCATION_CIRCLES`]. Over the cap the verdict is left
    /// [`Pending`](ConflictStatus::Pending): a too-complex region isn't a
    /// decided conflict, it is one this layer declines to decide.
    fn conflict(&self) -> ConflictStatus {
        if self.circle_count() > MAX_PROJECTED_LOCATION_CIRCLES {
            return ConflictStatus::Pending;
        }
        self.conflict_status()
    }
}

/// The number of circles a merged location may carry before the projection
/// declines to run its emptiness check.
///
/// Submit caps a *stored* location at
/// [`MAX_LOCATION_CIRCLES`](crate::facts::submit::pipeline::MAX_LOCATION_CIRCLES),
/// but the consensus bound is the meet of many stored claims and a `Reference`
/// resolves to arbitrary geometry, so the merged circle count is unbounded by
/// the submit cap alone. This bound guards the `O(m²·|expr|)` candidate-point
/// cost the consensus check reuses. Set to admit the meet of a generous handful
/// of cap-bounded claims.
pub const MAX_PROJECTED_LOCATION_CIRCLES: usize = 1_000;

#[cfg(test)]
mod laws {
    use proptest::prelude::*;

    use super::*;
    use crate::algebra::semiring::Lineage;
    use crate::claimed::Claimed;
    use crate::facts::projection::slot::Slot;

    type Support = Lineage<u8>;
    type B = Bracket<Claimed<u8>, Support>;

    fn arb_claim() -> impl Strategy<Value = Claimed<u8>> {
        prop_oneof![
            6 => prop::collection::btree_set(0u8..=6, 0..=4).prop_map(Claimed::Of),
            1 => Just(Claimed::Any),
        ]
    }

    fn arb_support() -> impl Strategy<Value = Support> {
        prop_oneof![
            1 => Just(Lineage::Bottom),
            4 => prop::collection::btree_set(0u8..=8, 0..=3).prop_map(Lineage::Of),
        ]
    }

    fn arb_bracket() -> impl Strategy<Value = B> {
        (arb_claim(), arb_support()).prop_map(Bracket::from)
    }

    // `combine` is the join the whole-entity fold rides on, support and all, so
    // we test it as a join-semilattice under structural equality. The bracket's
    // knowledge order also admits a dual meet — a real bilattice op — but
    // nothing consumes it, so we don't assert it.
    crate::join_semilattice_laws!(
        bracket_is_a_join_semilattice,
        B,
        arb_bracket(),
        |a: &B, b: &B| a == b
    );

    proptest! {
        /// Combining more claims only narrows the consensus, so conflict is
        /// monotone: once a field conflicts, no further combine clears it.
        #[test]
        fn conflict_is_monotone_under_combine(a in arb_bracket(), b in arb_bracket()) {
            if a.conflict() == ConflictStatus::Conflict {
                prop_assert_eq!(a.combine(b).conflict(), ConflictStatus::Conflict);
            }
        }

        /// A single claim never conflicts (its consensus is the claim itself);
        /// combining two disjoint value-mode claims does (the meet empties).
        #[test]
        fn disjoint_value_claims_conflict(x in 0u8..=6, y in 0u8..=6) {
            let cx = B::from((Claimed::Of([x].into_iter().collect()), Lineage::one()));
            prop_assert_eq!(cx.conflict(), ConflictStatus::Consistent);
            let cy = B::from((Claimed::Of([y].into_iter().collect()), Lineage::one()));
            let joined = cx.combine(cy);
            let expected = if x == y {
                ConflictStatus::Consistent
            } else {
                ConflictStatus::Conflict
            };
            prop_assert_eq!(joined.conflict(), expected);
        }
    }
}
