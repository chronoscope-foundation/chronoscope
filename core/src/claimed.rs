//! `Claimed<A>` — the value lattice of "which values have been asserted".
//!
//! [`Of`](Claimed::Of) holds the exact set of asserted values; [`Any`](Claimed::Any)
//! is a symbolic ⊤ standing for "every value in the domain". `Any` stays
//! symbolic so the lattice works over open domains (free-text designations,
//! `Other(String)` causes) with no enumerable universe — it is never expanded
//! into a concrete set.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::algebra::lattice::{JoinSemilattice, MeetSemilattice};
use crate::algebra::monoid::CommutativeMonoid;

/// A claim over a domain `A`: either an explicit set of asserted values or the
/// symbolic top ⊤ ([`Any`](Claimed::Any)).
///
/// The typed projection serializes this into the read-side surface — the
/// settled-value wart (a singleton `Of { values: {x} }`) and `Any` reach the
/// consumer as the value lattice. The derived `Serialize` tags each variant with
/// an internal `"type"` field (`of` / `any`) for the grammar's uniform
/// internal-tag convention.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[serde(bound(deserialize = "A: ::serde::de::DeserializeOwned + Ord"))]
pub enum Claimed<A: Ord> {
    /// Exactly these values have been asserted. The empty set is ⊥.
    Of { values: BTreeSet<A> },
    /// Every value in the domain — the symbolic ⊤, identity under meet.
    Any,
}

impl<A: Ord> CommutativeMonoid for Claimed<A> {
    fn identity() -> Self {
        Claimed::Of {
            values: BTreeSet::new(),
        }
    }

    fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Claimed::Any, _) | (_, Claimed::Any) => Claimed::Any,
            // ⊥ (the empty set) is the join identity: return the other operand
            // untouched rather than rebuilding the union element by element.
            (Claimed::Of { values }, other) if values.is_empty() => other,
            (one, Claimed::Of { values }) if values.is_empty() => one,
            (Claimed::Of { values: mut a }, Claimed::Of { values: b }) => {
                a.extend(b);
                Claimed::Of { values: a }
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
            (Claimed::Of { values: a }, Claimed::Of { values: b }) => {
                // Intersection keeps the smaller set's surviving elements; retain
                // on whichever is smaller avoids a clone of either operand.
                let (mut keep, probe) = if a.len() <= b.len() { (a, b) } else { (b, a) };
                keep.retain(|x| probe.contains(x));
                Claimed::Of { values: keep }
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
            6 => prop::collection::btree_set(0u8..=8, 0..=5)
                .prop_map(|values| Claimed::Of { values }),
            1 => Just(Claimed::Any),
        ]
    }

    crate::lattice_laws!(
        lattice_laws,
        Claimed<u8>,
        arb_claimed_u8(),
        |a: &Claimed<u8>, b: &Claimed<u8>| a == b
    );

    #[test]
    fn serializes_with_internal_type_tag() -> Result<(), serde_json::Error> {
        let of: Claimed<u8> = Claimed::Of {
            values: [1, 2].into_iter().collect(),
        };
        assert_eq!(
            serde_json::to_value(&of)?,
            serde_json::json!({ "type": "of", "values": [1, 2] }),
            "Of tags its values array with an internal type field"
        );
        let any: Claimed<u8> = Claimed::Any;
        assert_eq!(
            serde_json::to_value(&any)?,
            serde_json::json!({ "type": "any" }),
            "Any is a bare internally-tagged object"
        );
        Ok(())
    }
}
