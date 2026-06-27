//! The commutative monoid — the algebraic floor under the value lattices.
//!
//! A [`CommutativeMonoid`] is an [`identity`](CommutativeMonoid::identity) and an
//! associative, commutative [`combine`](CommutativeMonoid::combine). The
//! semilattices in [`super::lattice`] add idempotence on top.

/// A commutative monoid: an [`identity`](Self::identity) and an associative,
/// commutative [`combine`](Self::combine).
///
/// The laws, for all `a`, `b`, `c`:
/// - associativity: `a.combine(b).combine(c) == a.combine(b.combine(c))`
/// - commutativity: `a.combine(b) == b.combine(a)`
/// - identity: `a.combine(Self::identity()) == a`
///
/// `combine` consumes both operands so each impl can reuse their allocations
/// instead of cloning. A caller that still needs an operand afterward clones it
/// explicitly.
pub trait CommutativeMonoid: Sized {
    /// The identity element — `a.combine(identity()) == a`.
    fn identity() -> Self;

    /// Combine two values under the monoid operation, consuming both.
    fn combine(self, other: Self) -> Self;
}

/// The commutative-monoid laws — associativity, commutativity, identity — over
/// [`combine`](CommutativeMonoid::combine) and
/// [`identity`](CommutativeMonoid::identity), asserted up to `$eq`.
///
/// `$ty` is the carrier, `$strat` a `Strategy<Value = $ty>`, `$eq` the
/// equivalence the laws hold under (see [`lattice_laws`](crate::lattice_laws) for
/// the `$eq` contract). [`lattice_laws`](crate::lattice_laws) reuses this block
/// and adds the idempotence and meet/bounds laws on top. Requires `$ty:
/// CommutativeMonoid + Clone + Debug` and `proptest::prelude::*` in scope.
#[cfg(test)]
#[macro_export]
macro_rules! commutative_monoid_laws {
    ($ty:ty, $strat:expr, $eq:expr) => {
        proptest! {
            #[test]
            fn combine_commutative(a in $strat, b in $strat) {
                let eq = $eq;
                prop_assert!(eq(&a.clone().combine(b.clone()), &b.combine(a)));
            }

            #[test]
            fn combine_associative(a in $strat, b in $strat, c in $strat) {
                let eq = $eq;
                prop_assert!(eq(
                    &a.clone().combine(b.clone()).combine(c.clone()),
                    &a.combine(b.combine(c)),
                ));
            }

            #[test]
            fn combine_identity(a in $strat) {
                let eq = $eq;
                let id = <$ty as $crate::algebra::monoid::CommutativeMonoid>::identity();
                prop_assert!(eq(&a.clone().combine(id), &a));
            }
        }
    };
}
