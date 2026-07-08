//! [`Slot`] — the knowledge-order monoid every projected field is.
//!
//! A projected entity is a product of slots; the merge is one fold,
//! `facts.map(inject).fold(identity, combine)`. Product and map of monoids are
//! monoids, so per-slot-type laws buy whole-entity correctness — the record
//! structs combine fieldwise, the maps key-wise.
//!
//! Two slot shapes carry the two modes of a claim. A [`Bracket`] is restrictive
//! (the value narrows; conflict when over-determined). A [`FactMap`] is additive
//! (membership unions; keys never conflict — open-world). The CRDT split of a
//! grow-only set against a multi-value register (Shapiro et al. 2011).

use std::collections::BTreeMap;

use crate::algebra::lattice::BoundedLattice;
use crate::algebra::monoid::CommutativeMonoid;
use crate::algebra::semiring::Semiring;
use crate::location::ConflictStatus;

use super::bracket::{Bracket, ConsensusConflict};
use super::provenance::Cited;

/// An additive keyed collection: each key carries its own value slot. Keys union
/// on combine (membership, no key-level conflict); each entry's value combines as
/// a slot. The `T` rides on the key's presence.
pub type FactMap<K, V, T> = BTreeMap<K, Cited<V, T>>;

/// A bare membership set: each element maps to its support, its value slot the
/// trivial `()`. The grow-only set — present elements only, never a conflict.
pub type FactSet<E, T> = FactMap<E, (), T>;

/// A projected field: a [`CommutativeMonoid`] plus the field's conflict verdict.
/// The monoid's `identity`/`combine` carry the empty slot and the knowledge-order
/// merge; `conflict` reads whether the slot is over-determined.
pub trait Slot: CommutativeMonoid {
    /// Whether the slot's content is over-determined.
    fn conflict(&self) -> ConflictStatus;
}

/// A membership presence with no attributes — never a conflict.
impl CommutativeMonoid for () {
    fn identity() -> Self {}

    fn combine(self, (): Self) -> Self {}
}

impl Slot for () {
    fn conflict(&self) -> ConflictStatus {
        ConflictStatus::Consistent
    }
}

impl<K, V, T> CommutativeMonoid for FactMap<K, V, T>
where
    K: Ord,
    V: CommutativeMonoid,
    T: Semiring,
{
    fn identity() -> Self {
        BTreeMap::new()
    }

    /// Keys union; a key in both combines its value slots and its support. A key
    /// in one only carries through unchanged.
    fn combine(self, other: Self) -> Self {
        let mut merged = self;
        for (key, incoming) in other {
            match merged.remove(&key) {
                Some(existing) => {
                    merged.insert(
                        key,
                        Cited {
                            value: existing.value.combine(incoming.value),
                            support: existing.support.plus(incoming.support),
                        },
                    );
                }
                None => {
                    merged.insert(key, incoming);
                }
            }
        }
        merged
    }
}

impl<K, V, T> Slot for FactMap<K, V, T>
where
    K: Ord,
    V: Slot,
    T: Semiring,
{
    /// Membership never conflicts; each entry's value slot might. A bare set
    /// (`V = ()`) is therefore always consistent.
    fn conflict(&self) -> ConflictStatus {
        self.values()
            .map(|entry| entry.value.conflict())
            .max()
            .unwrap_or(ConflictStatus::Consistent)
    }
}

impl<V, T> Slot for Bracket<V, T>
where
    V: BoundedLattice + ConsensusConflict,
    T: Semiring,
{
    fn conflict(&self) -> ConflictStatus {
        self.consensus.value.conflict()
    }
}

/// Derive a record struct's fieldwise [`CommutativeMonoid`] and [`Slot`]:
/// `combine` runs the field monoids in parallel (consuming both records, moving
/// each field into its slot's `combine`), `conflict` takes the worst field
/// verdict. Product of monoids is a monoid; this is that theorem, mechanized.
///
/// Each generic parameter carries a single trait bound, so the macro spans both
/// the single-`Semiring` records and the multi-parameter [`Entity`]
/// (`EntId: Ord, EvtId: Ord, T: Semiring`) over the one impl shape.
macro_rules! derive_slot {
    ($ty:ident<$($g:ident: $bound:path),+ $(,)?>, { $($field:ident),+ $(,)? }) => {
        impl<$($g: $bound),+> $crate::algebra::monoid::CommutativeMonoid for $ty<$($g),+> {
            fn identity() -> Self {
                Self {
                    $($field: $crate::algebra::monoid::CommutativeMonoid::identity()),+
                }
            }

            fn combine(self, other: Self) -> Self {
                Self {
                    $($field: $crate::algebra::monoid::CommutativeMonoid::combine(
                        self.$field, other.$field
                    )),+
                }
            }
        }

        impl<$($g: $bound),+> $crate::projection::slot::Slot for $ty<$($g),+> {
            fn conflict(&self) -> $crate::location::ConflictStatus {
                $crate::location::ConflictStatus::Consistent
                    $(.max($crate::projection::slot::Slot::conflict(&self.$field)))+
            }
        }
    };
}

pub(super) use derive_slot;

#[cfg(test)]
mod laws {
    use std::collections::BTreeSet;

    use proptest::prelude::*;

    use super::*;
    use crate::algebra::semiring::Lineage;
    use crate::projection::Claimed;

    type Support = Lineage<u8>;
    type ValueSlot = Bracket<Claimed<u8>, Support>;
    type Map = FactMap<u8, ValueSlot, Support>;

    fn arb_support() -> impl Strategy<Value = Support> {
        prop_oneof![
            1 => Just(Lineage::Bottom),
            4 => prop::collection::btree_set(0u8..=8, 0..=2).prop_map(Lineage::Of),
        ]
    }

    fn arb_value_slot() -> impl Strategy<Value = ValueSlot> {
        (prop::collection::btree_set(0u8..=4, 0..=3), arb_support())
            .prop_map(|(values, support)| Bracket::from((Claimed::Of { values }, support)))
    }

    fn arb_map() -> impl Strategy<Value = Map> {
        prop::collection::vec((0u8..=4, arb_value_slot(), arb_support()), 0..=4).prop_map(
            |entries| {
                entries
                    .into_iter()
                    .map(|(key, value, support)| (key, Cited { value, support }))
                    .collect()
            },
        )
    }

    proptest! {
        /// The keyed combine is a commutative monoid: keys union, shared keys
        /// combine their value slots. Map of monoids is a monoid — this is that
        /// lift.
        #[test]
        fn map_combine_is_a_monoid(a in arb_map(), b in arb_map(), c in arb_map()) {
            prop_assert_eq!(a.clone().combine(b.clone()), b.clone().combine(a.clone()));
            prop_assert_eq!(
                a.clone().combine(b.clone()).combine(c.clone()),
                a.clone().combine(b.combine(c)),
            );
            prop_assert_eq!(Map::identity().combine(a.clone()), a.clone());
            prop_assert_eq!(a.clone().combine(Map::identity()), a);
        }

        /// A bare membership set (`V = ()`) is always consistent — additive
        /// fields never produce a conflict, no matter how they merge.
        #[test]
        fn membership_never_conflicts(elements in prop::collection::vec(0u8..=8, 0..=6)) {
            let set: FactSet<u8, Support> = elements
                .into_iter()
                .map(|e| (e, Cited { value: (), support: Lineage::Of(BTreeSet::new()) }))
                .collect();
            prop_assert_eq!(set.conflict(), ConflictStatus::Consistent);
        }
    }
}
