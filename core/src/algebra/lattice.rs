//! The bounded-lattice abstractions shared by the value lattices.
//!
//! A join-semilattice is a [`CommutativeMonoid`](super::monoid::CommutativeMonoid)
//! whose `combine` is also idempotent. [`JoinSemilattice`] names that floor — its
//! ⊥ ([`bottom`](JoinSemilattice::bottom)) and ⊔
//! ([`join`](JoinSemilattice::join)) are the monoid's `identity` and `combine`
//! under lattice names. Each value lattice supplies its own canonicalizing
//! `combine`. [`join_all`](JoinSemilattice::join_all) is a convenience for folding
//! a whole stream through `combine` from ⊥ (empty → ⊥, singleton → itself); a
//! consumer that merges incrementally just calls `combine`.
//!
//! [`MeetSemilattice`] is the dual: a greatest element ⊤
//! ([`top`](MeetSemilattice::top)) and a meet ⊓ ([`meet`](MeetSemilattice::meet)),
//! with [`meet_all`](MeetSemilattice::meet_all) folding from ⊤.
//!
//! A type with both halves is a [`BoundedLattice`] — the blanket impl wires it
//! up, so implementing both semilattices is all a value lattice needs to do.

use super::monoid::CommutativeMonoid;

/// A join-semilattice: a [`CommutativeMonoid`] whose
/// [`combine`](CommutativeMonoid::combine) is idempotent, surfaced under lattice
/// names as ⊥ and ⊔.
///
/// The supertrait carries the operation; this trait adds one law — idempotence,
/// `a ⊔ a == a` — that distinguishes a join-semilattice from a bare commutative
/// monoid. It has no required methods: the join monoid's `identity` is ⊥ and its
/// `combine` is ⊔.
pub trait JoinSemilattice: CommutativeMonoid {
    /// The least element ⊥ — the join identity, `a ⊔ ⊥ == a`.
    fn bottom() -> Self {
        Self::identity()
    }

    /// The join ⊔ of two values, consuming both.
    fn join(self, other: Self) -> Self {
        self.combine(other)
    }

    /// The join of a stream, folded from ⊥. An empty stream yields ⊥; a
    /// singleton yields itself. Any per-type canonicalization lives in
    /// [`join`](Self::join), so the fold needs no override.
    fn join_all(values: impl IntoIterator<Item = Self>) -> Self {
        values.into_iter().fold(Self::bottom(), Self::join)
    }
}

/// A meet-semilattice: a greatest element ⊤ and an idempotent, commutative,
/// associative meet ⊓.
///
/// Meet is a commutative monoid too (⊤ is its identity), so it could subtrait
/// [`CommutativeMonoid`] the way [`JoinSemilattice`] does — but a type gets only
/// one [`CommutativeMonoid`] impl, which is its join, and routing meet through it
/// as well would take a newtype per lattice to carry the second monoid. Nothing
/// folds generically over meet (it's always called by name), so rather than add
/// that newtype noise we subtrait only on the join side and keep ⊓/⊤ as plain
/// methods here.
pub trait MeetSemilattice: Sized {
    /// The greatest element ⊤ — the meet identity, `a ⊓ ⊤ == a`.
    fn top() -> Self;

    /// The meet ⊓ of two values, consuming both.
    fn meet(self, other: Self) -> Self;

    /// The meet of a stream, folded from ⊤. An empty stream yields ⊤; a
    /// singleton yields itself. Any per-type canonicalization lives in
    /// [`meet`](Self::meet), so the fold needs no override.
    fn meet_all(values: impl IntoIterator<Item = Self>) -> Self {
        values.into_iter().fold(Self::top(), Self::meet)
    }
}

/// A bounded lattice: both semilattice halves over the same carrier, so ⊥, ⊤,
/// ⊔, and ⊓ all live together.
pub trait BoundedLattice: JoinSemilattice + MeetSemilattice {}

impl<T: JoinSemilattice + MeetSemilattice> BoundedLattice for T {}

/// The join-semilattice law suite in one block, asserted up to `$eq`.
///
/// The join half a generic fold rides on: the commutative-monoid laws
/// (commutativity, associativity, identity, via
/// [`commutative_monoid_laws`](crate::commutative_monoid_laws)) plus join
/// idempotence, `a ⊔ a == a`. No meet, absorption, distributivity, or bounds —
/// for a carrier whose only consumed operation is `combine`, this is the whole
/// contract.
///
/// `$eq` is an `impl Fn(&$ty, &$ty) -> bool`, the equivalence the laws are
/// checked against; see [`lattice_laws`](crate::lattice_laws) for the `$eq`
/// contract and the discriminator obligation a denotational oracle carries.
///
/// `$ty` is the carrier; `$strat` a `Strategy<Value = $ty>` invoked fresh per
/// generator. The block names itself `$name`. Requires `$ty: CommutativeMonoid +
/// Clone + Debug` and `proptest::prelude::*` in scope.
#[cfg(test)]
#[macro_export]
macro_rules! join_semilattice_laws {
    ($name:ident, $ty:ty, $strat:expr, $eq:expr) => {
        mod $name {
            use super::*;

            $crate::join_semilattice_laws!(@items $ty, $strat, $eq);
        }
    };
    (@items $ty:ty, $strat:expr, $eq:expr) => {
        use $crate::algebra::monoid::CommutativeMonoid;

        // Commutativity, associativity, and identity of the join monoid's
        // `combine`; idempotence — the law that lifts it to a semilattice —
        // follows.
        $crate::commutative_monoid_laws!($ty, $strat, $eq);

        proptest! {
            #[test]
            fn join_idempotent(a in $strat) {
                let eq = $eq;
                prop_assert!(eq(&a.clone().combine(a.clone()), &a));
            }
        }
    };
}

/// The whole bounded-lattice law suite in one block, asserted up to `$eq`.
///
/// One macro, one call per carrier — the join half's
/// [`join_semilattice_laws`](crate::join_semilattice_laws) (commutative-monoid
/// laws plus join idempotence) reused wholesale, then the meet half's
/// commutativity·associativity·idempotence, the four ⊤/⊥ identities, absorption,
/// distributivity, and `⊓-all ⊑ ⊔-all` on top, so a carrier can't silently skip
/// a half.
///
/// `$eq` is an `impl Fn(&$ty, &$ty) -> bool`, the equivalence the laws are
/// checked against. `|a, b| a == b` recovers exact structural equality; a
/// denotational equivalence lets a carrier whose `meet`/`join` keep a symbolic
/// shape (a disjoint meet kept as an intersection rather than collapsed) satisfy
/// the absorption/distributivity laws it obeys only set-theoretically.
///
/// Contract: every non-structural `$eq` (a denotational oracle) must have a
/// companion `*_discriminates` test proving it rejects genuinely different
/// values, else the laws pass vacuously.
///
/// `$ty` is the carrier; `$strat` a `Strategy<Value = $ty>` invoked fresh per
/// generator. The block names itself `$name`. Requires `$ty: BoundedLattice +
/// Clone + Debug` and `proptest::prelude::*` in scope.
#[cfg(test)]
#[macro_export]
macro_rules! lattice_laws {
    ($name:ident, $ty:ty, $strat:expr, $eq:expr) => {
        mod $name {
            use super::*;
            use $crate::algebra::lattice::{JoinSemilattice, MeetSemilattice};

            // The join half — commutative-monoid laws plus join idempotence —
            // reused; the meet/bounds laws follow below. The `@items` arm brings
            // `CommutativeMonoid` into scope for the meet/bounds laws too.
            $crate::join_semilattice_laws!(@items $ty, $strat, $eq);

            proptest! {
                #[test]
                fn meet_commutative(a in $strat, b in $strat) {
                    let eq = $eq;
                    prop_assert!(eq(&a.clone().meet(b.clone()), &b.meet(a)));
                }

                #[test]
                fn meet_associative(a in $strat, b in $strat, c in $strat) {
                    let eq = $eq;
                    prop_assert!(eq(
                        &a.clone().meet(b.clone()).meet(c.clone()),
                        &a.meet(b.meet(c)),
                    ));
                }

                #[test]
                fn meet_idempotent(a in $strat) {
                    let eq = $eq;
                    prop_assert!(eq(&a.clone().meet(a.clone()), &a));
                }

                /// ⊤ is the meet unit, ⊥ the join unit, and each is the other
                /// op's absorbing element.
                #[test]
                fn bounded_identities(a in $strat) {
                    let eq = $eq;
                    let top = <$ty as MeetSemilattice>::top();
                    let bottom = <$ty as JoinSemilattice>::bottom();
                    prop_assert!(eq(&a.clone().meet(top.clone()), &a));
                    prop_assert!(eq(&a.clone().join(bottom.clone()), &a));
                    prop_assert!(eq(&a.clone().meet(bottom.clone()), &bottom));
                    prop_assert!(eq(&a.join(top.clone()), &top));
                }

                /// Absorption ties meet and join into one lattice.
                #[test]
                fn absorption(a in $strat, b in $strat) {
                    let eq = $eq;
                    prop_assert!(eq(&a.clone().meet(a.clone().join(b.clone())), &a));
                    prop_assert!(eq(&a.clone().join(a.clone().meet(b)), &a));
                }

                /// Both lattices are distributive — meet distributes over join
                /// and the dual.
                #[test]
                fn distributive(a in $strat, b in $strat, c in $strat) {
                    let eq = $eq;
                    prop_assert!(eq(
                        &a.clone().meet(b.clone().join(c.clone())),
                        &a.clone().meet(b.clone()).join(a.clone().meet(c.clone())),
                    ));
                    prop_assert!(eq(
                        &a.clone().join(b.clone().meet(c.clone())),
                        &a.clone().join(b).meet(a.join(c)),
                    ));
                }

                /// Over a nonempty sample, ⊓ of all sits below ⊔ of all, where
                /// `x ⊑ y` is `x ⊔ y == y`. (Empty would invert this: ⊓ of
                /// nothing is ⊤, ⊔ of nothing is ⊥.)
                #[test]
                fn meet_all_below_join_all(
                    xs in prop::collection::vec($strat, 1..=6),
                ) {
                    let eq = $eq;
                    let m = <$ty as MeetSemilattice>::meet_all(xs.iter().cloned());
                    let j = <$ty as JoinSemilattice>::join_all(xs.iter().cloned());
                    prop_assert!(eq(&m.join(j.clone()), &j));
                }
            }
        }
    };
}
