//! `Claimed<A>` — the value lattice of "which values have been asserted".
//!
//! [`Of`](Claimed::Of) holds the exact set of asserted values; [`Any`](Claimed::Any)
//! is a symbolic ⊤ standing for "every value in the domain". `Any` stays
//! symbolic so the lattice works over open domains (free-text designations,
//! `Other(String)` causes) with no enumerable universe — it is never expanded
//! into a concrete set.

use std::collections::BTreeSet;

use crate::algebra::lattice::{JoinSemilattice, MeetSemilattice};
use crate::algebra::monoid::CommutativeMonoid;

/// A claim over a domain `A`: either an explicit set of asserted values or the
/// symbolic top ⊤ ([`Any`](Claimed::Any)).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Claimed<A: Ord> {
    /// Exactly these values have been asserted. The empty set is ⊥.
    Of(BTreeSet<A>),
    /// Every value in the domain — the symbolic ⊤, identity under meet.
    Any,
}

impl<A: Ord> CommutativeMonoid for Claimed<A> {
    fn identity() -> Self {
        Claimed::Of(BTreeSet::new())
    }

    fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Claimed::Any, _) | (_, Claimed::Any) => Claimed::Any,
            // ⊥ (the empty set) is the join identity: return the other operand
            // untouched rather than rebuilding the union element by element.
            (Claimed::Of(a), other) if a.is_empty() => other,
            (one, Claimed::Of(b)) if b.is_empty() => one,
            (Claimed::Of(mut a), Claimed::Of(b)) => {
                a.extend(b);
                Claimed::Of(a)
            }
        }
    }
}

impl<A: Ord> JoinSemilattice for Claimed<A> {}

impl<A: Ord> MeetSemilattice for Claimed<A> {
    fn top() -> Self {
        Claimed::Any
    }

    fn meet(self, other: Self) -> Self {
        match (self, other) {
            (Claimed::Any, x) | (x, Claimed::Any) => x,
            (Claimed::Of(a), Claimed::Of(b)) => {
                // Intersection keeps the smaller set's surviving elements; retain
                // on whichever is smaller avoids a clone of either operand.
                let (mut keep, probe) = if a.len() <= b.len() { (a, b) } else { (b, a) };
                keep.retain(|x| probe.contains(x));
                Claimed::Of(keep)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Small `u8` claims keep set ops cheap and force frequent overlap. `Any`
    /// stays rare so the ⊤ paths are sampled without swamping the `Of` algebra.
    fn arb_claimed_u8() -> impl Strategy<Value = Claimed<u8>> {
        prop_oneof![
            6 => prop::collection::btree_set(0u8..=8, 0..=5).prop_map(Claimed::Of),
            1 => Just(Claimed::Any),
        ]
    }

    crate::lattice_laws!(
        lattice_laws,
        Claimed<u8>,
        arb_claimed_u8(),
        |a: &Claimed<u8>, b: &Claimed<u8>| a == b
    );
}
