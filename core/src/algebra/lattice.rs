//! The bounded-lattice abstractions shared by the value lattices.
//!
//! A join-semilattice is a [`CommutativeMonoid`]
//! whose `combine` is also idempotent. [`JoinSemilattice`] names that floor — its
//! ⊥ ([`bottom`](JoinSemilattice::bottom)) and ⊔
//! ([`join`](JoinSemilattice::join)) are the monoid's `identity` and `combine`
//! under lattice names. Recognizing ⊥ is
//! [`is_bottom`](JoinSemilattice::is_bottom), a predicate each lattice supplies
//! for its own ⊥ — a canonical value, a family of empties, or a geometric
//! emptiness routine. Each value lattice supplies its own
//! canonicalizing `combine`. [`join_all`](JoinSemilattice::join_all) is a convenience for folding
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
/// monoid. The join monoid's `identity` is ⊥ and its `combine` is ⊔; the one
/// required method is [`is_bottom`](Self::is_bottom), the ⊥-recognition predicate.
pub trait JoinSemilattice: CommutativeMonoid {
    /// The least element ⊥ — the join identity, `a ⊔ ⊥ == a`.
    fn bottom() -> Self {
        Self::identity()
    }

    /// Whether this value is ⊥ — the least element, denoting nothing / a
    /// contradiction. A predicate, so each lattice recognizes its own ⊥ its own
    /// way: a canonical value, a family of empties, or a geometric emptiness
    /// routine.
    fn is_bottom(&self) -> bool;

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

/// The meet-semilattice law suite in one block, asserted up to `$eq`.
///
/// The dual of [`join_semilattice_laws`](crate::join_semilattice_laws):
/// commutativity, associativity, and idempotence of ⊓, plus the ⊤-identity
/// `a ⊓ ⊤ == a`. Meet is a commutative monoid too, but a type's one
/// [`CommutativeMonoid`] impl is its join, so these are spelled out here rather
/// than reused from [`commutative_monoid_laws`](crate::commutative_monoid_laws).
///
/// `$eq` is an `impl Fn(&$ty, &$ty) -> bool`, the equivalence the laws are
/// checked against; see [`lattice_laws`](crate::lattice_laws) for the `$eq`
/// contract and the discriminator obligation a denotational oracle carries.
///
/// `$ty` is the carrier; `$strat` a `Strategy<Value = $ty>` invoked fresh per
/// generator. The block names itself `$name`. Requires `$ty: MeetSemilattice +
/// Clone + Debug` and `proptest::prelude::*` in scope.
#[cfg(test)]
#[macro_export]
macro_rules! meet_semilattice_laws {
    ($name:ident, $ty:ty, $strat:expr, $eq:expr) => {
        mod $name {
            use super::*;

            $crate::meet_semilattice_laws!(@items $ty, $strat, $eq);
        }
    };
    (@items $ty:ty, $strat:expr, $eq:expr) => {
        use $crate::algebra::lattice::MeetSemilattice;

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

            /// ⊤ is the meet unit, `a ⊓ ⊤ == a`.
            #[test]
            fn meet_identity(a in $strat) {
                let eq = $eq;
                let top = <$ty as MeetSemilattice>::top();
                prop_assert!(eq(&a.clone().meet(top), &a));
            }
        }
    };
}

/// The bounded-lattice laws that ride on both bounds, in one block, asserted up
/// to `$eq`.
///
/// On top of the two semilattices: the annihilators (`a ⊓ ⊥ == ⊥`,
/// `a ⊔ ⊤ == ⊤`, each bound absorbing the opposite operation) and `⊓-all ⊑
/// ⊔-all` over a nonempty sample. These fix how the bounds meet the opposite
/// operation without invoking absorption or distributivity, so a carrier whose
/// bounds behave exactly can check this group under structural `==` and defer
/// the cross-operation laws to a denotational oracle.
///
/// `$eq` is an `impl Fn(&$ty, &$ty) -> bool`; see
/// [`lattice_laws`](crate::lattice_laws) for the `$eq` contract.
///
/// The `@items` arm adds no `use` of its own — it reads `JoinSemilattice` and
/// `MeetSemilattice` from the enclosing scope (the standalone arm and
/// [`lattice_laws`](crate::lattice_laws) each bring both in), so the four
/// `@items` compose into one module without duplicate-import collisions.
///
/// `$ty` is the carrier; `$strat` a `Strategy<Value = $ty>` invoked fresh per
/// generator. The block names itself `$name`. Requires `$ty: BoundedLattice +
/// Clone + Debug` and `proptest::prelude::*` in scope.
#[cfg(test)]
#[macro_export]
macro_rules! bounded_lattice_laws {
    ($name:ident, $ty:ty, $strat:expr, $eq:expr) => {
        mod $name {
            use super::*;
            use $crate::algebra::lattice::{JoinSemilattice, MeetSemilattice};

            $crate::bounded_lattice_laws!(@items $ty, $strat, $eq);
        }
    };
    (@items $ty:ty, $strat:expr, $eq:expr) => {
        proptest! {
            /// ⊥ annihilates meet and ⊤ annihilates join — each bound is the
            /// other operation's absorbing element.
            #[test]
            fn bounded_annihilators(a in $strat) {
                let eq = $eq;
                let top = <$ty as MeetSemilattice>::top();
                let bottom = <$ty as JoinSemilattice>::bottom();
                prop_assert!(eq(&a.clone().meet(bottom.clone()), &bottom));
                prop_assert!(eq(&a.join(top.clone()), &top));
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
    };
}

/// The distributive-lattice laws — absorption and distributivity — in one
/// block, asserted up to `$eq`.
///
/// The cross-operation laws binding meet and join: absorption
/// (`a ⊓ (a ⊔ b) == a`, `a ⊔ (a ⊓ b) == a`) and distributivity of each
/// operation over the other. A carrier whose `meet`/`join` keep a symbolic
/// shape rather than collapsing satisfies these set-theoretically, so a
/// denotational `$eq` passes what structural `==` would split.
///
/// `$eq` is an `impl Fn(&$ty, &$ty) -> bool`; see
/// [`lattice_laws`](crate::lattice_laws) for the `$eq` contract and the
/// `*_discriminates` obligation a denotational oracle carries.
///
/// The `@items` arm adds no `use` of its own — it reads `JoinSemilattice` and
/// `MeetSemilattice` from the enclosing scope (the standalone arm and
/// [`lattice_laws`](crate::lattice_laws) each bring both in), so the four
/// `@items` compose into one module without duplicate-import collisions.
///
/// `$ty` is the carrier; `$strat` a `Strategy<Value = $ty>` invoked fresh per
/// generator. The block names itself `$name`. Requires `$ty: BoundedLattice +
/// Clone + Debug` and `proptest::prelude::*` in scope.
#[cfg(test)]
#[macro_export]
macro_rules! distributive_lattice_laws {
    ($name:ident, $ty:ty, $strat:expr, $eq:expr) => {
        mod $name {
            use super::*;
            use $crate::algebra::lattice::{JoinSemilattice, MeetSemilattice};

            $crate::distributive_lattice_laws!(@items $ty, $strat, $eq);
        }
    };
    (@items $ty:ty, $strat:expr, $eq:expr) => {
        proptest! {
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
        }
    };
}

/// The whole bounded-lattice law suite in one block, asserted up to `$eq`.
///
/// One macro, one call per carrier — the four law groups composed via their
/// `@items` arms: both semilattice halves
/// ([`join_semilattice_laws`](crate::join_semilattice_laws) /
/// [`meet_semilattice_laws`](crate::meet_semilattice_laws), each carrier's
/// single-operation laws with its `⊥`/`⊤` identity), the bounded-lattice laws
/// that need both bounds ([`bounded_lattice_laws`](crate::bounded_lattice_laws) —
/// the annihilators `a ⊓ ⊥ == ⊥` / `a ⊔ ⊤ == ⊤` and `⊓-all ⊑ ⊔-all`), and the
/// distributive-lattice laws
/// ([`distributive_lattice_laws`](crate::distributive_lattice_laws) — absorption
/// and distributivity). All four share the single `$eq`, so a carrier can't
/// silently skip a group.
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
            use $crate::algebra::lattice::JoinSemilattice;

            // The four law groups, each reused via its `@items` arm under the
            // single `$eq`. Join brings `CommutativeMonoid` into scope, meet
            // brings `MeetSemilattice`; the bounded and distributive groups add
            // no imports of their own and ride on those two plus the
            // `JoinSemilattice` imported here.
            $crate::join_semilattice_laws!(@items $ty, $strat, $eq);
            $crate::meet_semilattice_laws!(@items $ty, $strat, $eq);
            $crate::bounded_lattice_laws!(@items $ty, $strat, $eq);
            $crate::distributive_lattice_laws!(@items $ty, $strat, $eq);
        }
    };
}
