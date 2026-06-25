//! The bounded-lattice abstractions shared by the value lattices.
//!
//! A join-semilattice has a least element ⊥ ([`bottom`](JoinSemilattice::bottom))
//! and an idempotent, commutative, associative join ⊔
//! ([`join`](JoinSemilattice::join)). [`join_all`](JoinSemilattice::join_all)
//! folds a stream from ⊥, so an empty stream yields ⊥ and a singleton yields
//! itself. Each value lattice supplies its own canonicalizing `join`; the fold
//! rides on top.
//!
//! [`MeetSemilattice`] is the dual: a greatest element ⊤
//! ([`top`](MeetSemilattice::top)) and a meet ⊓ ([`meet`](MeetSemilattice::meet)),
//! with [`meet_all`](MeetSemilattice::meet_all) folding from ⊤.
//!
//! A type with both halves is a [`BoundedLattice`] — the blanket impl wires it
//! up, so implementing both semilattices is all a value lattice needs to do.

/// A join-semilattice: a least element ⊥ and an idempotent, commutative,
/// associative join ⊔.
pub trait JoinSemilattice: Sized {
    /// The least element ⊥ — the join identity, `a ⊔ ⊥ == a`.
    fn bottom() -> Self;

    /// The join ⊔ of two values.
    fn join(&self, other: &Self) -> Self;

    /// The join of a stream, folded from ⊥. An empty stream yields ⊥; a
    /// singleton yields itself. Any per-type canonicalization lives in
    /// [`join`](Self::join), so the fold needs no override.
    fn join_all(values: impl IntoIterator<Item = Self>) -> Self {
        values
            .into_iter()
            .fold(Self::bottom(), |acc, v| acc.join(&v))
    }
}

/// A meet-semilattice: a greatest element ⊤ and an idempotent, commutative,
/// associative meet ⊓.
pub trait MeetSemilattice: Sized {
    /// The greatest element ⊤ — the meet identity, `a ⊓ ⊤ == a`.
    fn top() -> Self;

    /// The meet ⊓ of two values.
    fn meet(&self, other: &Self) -> Self;

    /// The meet of a stream, folded from ⊤. An empty stream yields ⊤; a
    /// singleton yields itself. Any per-type canonicalization lives in
    /// [`meet`](Self::meet), so the fold needs no override.
    fn meet_all(values: impl IntoIterator<Item = Self>) -> Self {
        values.into_iter().fold(Self::top(), |acc, v| acc.meet(&v))
    }
}

/// A bounded lattice: both semilattice halves over the same carrier, so ⊥, ⊤,
/// ⊔, and ⊓ all live together.
pub trait BoundedLattice: JoinSemilattice + MeetSemilattice {}

impl<T: JoinSemilattice + MeetSemilattice> BoundedLattice for T {}

/// Emit the bounded-lattice law suite for a type and a proptest strategy.
///
/// `$ty` is the carrier; `$strat` is a `Strategy<Value = $ty>` expression
/// invoked fresh per generator. The block names itself `$name`, so several
/// instantiations coexist in one test module. Requires `$ty: BoundedLattice +
/// Clone + Debug + PartialEq` and `proptest::prelude::*` in scope.
#[cfg(test)]
#[macro_export]
macro_rules! bounded_lattice_laws {
    ($name:ident, $ty:ty, $strat:expr) => {
        mod $name {
            use super::*;
            use $crate::lattice::{JoinSemilattice, MeetSemilattice};

            proptest! {
                #[test]
                fn meet_commutative(a in $strat, b in $strat) {
                    prop_assert_eq!(a.meet(&b), b.meet(&a));
                }

                #[test]
                fn join_commutative(a in $strat, b in $strat) {
                    prop_assert_eq!(a.join(&b), b.join(&a));
                }

                #[test]
                fn meet_associative(a in $strat, b in $strat, c in $strat) {
                    prop_assert_eq!(a.meet(&b).meet(&c), a.meet(&b.meet(&c)));
                }

                #[test]
                fn join_associative(a in $strat, b in $strat, c in $strat) {
                    prop_assert_eq!(a.join(&b).join(&c), a.join(&b.join(&c)));
                }

                #[test]
                fn meet_idempotent(a in $strat) {
                    prop_assert_eq!(a.meet(&a), a.clone());
                }

                #[test]
                fn join_idempotent(a in $strat) {
                    prop_assert_eq!(a.join(&a), a.clone());
                }

                /// Absorption ties meet and join into one lattice.
                #[test]
                fn absorption(a in $strat, b in $strat) {
                    prop_assert_eq!(a.meet(&a.join(&b)), a.clone());
                    prop_assert_eq!(a.join(&a.meet(&b)), a.clone());
                }

                /// ⊤ is the meet unit, ⊥ the join unit, and each is the other
                /// op's absorbing element.
                #[test]
                fn bounded_identities(a in $strat) {
                    let top = <$ty as MeetSemilattice>::top();
                    let bottom = <$ty as JoinSemilattice>::bottom();
                    prop_assert_eq!(a.meet(&top), a.clone());
                    prop_assert_eq!(a.join(&bottom), a.clone());
                    prop_assert_eq!(a.meet(&bottom), bottom.clone());
                    prop_assert_eq!(a.join(&top), top);
                }

                /// Both lattices are distributive — meet distributes over join
                /// and the dual.
                #[test]
                fn distributive(a in $strat, b in $strat, c in $strat) {
                    prop_assert_eq!(
                        a.meet(&b.join(&c)),
                        a.meet(&b).join(&a.meet(&c)),
                    );
                    prop_assert_eq!(
                        a.join(&b.meet(&c)),
                        a.join(&b).meet(&a.join(&c)),
                    );
                }

                /// Over a nonempty sample, ⊓ of all sits below ⊔ of all, where
                /// `x ⊑ y` is `x ⊔ y == y`. (Empty would invert this: ⊓ of
                /// nothing is ⊤, ⊔ of nothing is ⊥.)
                #[test]
                fn meet_all_below_join_all(
                    xs in prop::collection::vec($strat, 1..=6),
                ) {
                    let m = <$ty as MeetSemilattice>::meet_all(xs.iter().cloned());
                    let j = <$ty as JoinSemilattice>::join_all(xs.iter().cloned());
                    prop_assert_eq!(m.join(&j), j);
                }
            }
        }
    };
}
