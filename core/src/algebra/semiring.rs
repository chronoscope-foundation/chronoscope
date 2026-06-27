//! The commutative semiring — the algebra provenance accumulates in.
//!
//! A [`Semiring`] is two interlocking commutative monoids over one carrier:
//! `(+, zero)` and `(·, one)`, with `·` distributing over `+` and `zero`
//! annihilating `·`. Provenance rides in a semiring (Green, Karvounarakis &
//! Tannen 2007): `+` records alternatives, `·` joint use, and a richer `T`
//! answers a richer question about the same computation.

use std::collections::BTreeSet;

/// A commutative semiring.
///
/// The laws hold for all `a`, `b`, `c`:
/// - `(+, zero)` is a commutative monoid.
/// - `(·, one)` is a commutative monoid.
/// - `·` distributes over `+`: `a·(b+c) == a·b + a·c`.
/// - `zero` annihilates `·`: `zero·a == zero`.
pub trait Semiring: Sized {
    /// The additive identity, and the multiplicative annihilator.
    fn zero() -> Self;
    /// The multiplicative identity.
    fn one() -> Self;
    /// The additive operation.
    fn plus(self, other: Self) -> Self;
    /// The multiplicative operation.
    fn times(self, other: Self) -> Self;
}

/// The ⊥-lifted lineage semiring over a set of atoms.
///
/// [`Bottom`](Self::Bottom) is `zero` — the additive identity and the
/// multiplicative annihilator. [`Of`](Self::Of) holds a set whose `plus` and
/// `times` both union, with the empty set as `one`. Lifting `zero` out of the
/// set is what makes annihilation hold: a plain set has no element below the
/// empty set to absorb a product.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Lineage<X> {
    /// The additive identity and multiplicative annihilator.
    Bottom,
    /// A set of atoms; `plus` and `times` both union it.
    Of(BTreeSet<X>),
}

impl<X> Lineage<X> {
    /// The atoms this lineage holds — none for [`Bottom`](Self::Bottom), the
    /// set's elements for [`Of`](Self::Of).
    pub fn iter(&self) -> impl Iterator<Item = &X> {
        match self {
            Lineage::Bottom => None,
            Lineage::Of(s) => Some(s),
        }
        .into_iter()
        .flatten()
    }
}

impl<X: Ord> Semiring for Lineage<X> {
    fn zero() -> Self {
        Lineage::Bottom
    }

    fn one() -> Self {
        Lineage::Of(BTreeSet::new())
    }

    fn plus(self, other: Self) -> Self {
        match (self, other) {
            (Lineage::Bottom, x) | (x, Lineage::Bottom) => x,
            (Lineage::Of(mut a), Lineage::Of(b)) => {
                a.extend(b);
                Lineage::Of(a)
            }
        }
    }

    fn times(self, other: Self) -> Self {
        match (self, other) {
            (Lineage::Bottom, _) | (_, Lineage::Bottom) => Lineage::Bottom,
            (Lineage::Of(mut a), Lineage::Of(b)) => {
                a.extend(b);
                Lineage::Of(a)
            }
        }
    }
}

/// The whole commutative-semiring law suite over [`Semiring`], asserted up to
/// `$eq`.
///
/// Both monoids' commutativity·associativity·identity, distributivity, and
/// annihilation — `zero·a == zero`, the law a bare set fails — in one block, so
/// a carrier can't silently skip one. `$ty` is the carrier, `$strat` a
/// `Strategy<Value = $ty>`, `$eq` the equivalence the laws hold under. Requires
/// `$ty: Semiring + Clone + Debug` and `proptest::prelude::*` in scope.
#[cfg(test)]
#[macro_export]
macro_rules! semiring_laws {
    ($ty:ty, $strat:expr, $eq:expr) => {
        proptest! {
            #[test]
            fn plus_commutative(a in $strat, b in $strat) {
                let eq = $eq;
                prop_assert!(eq(&a.clone().plus(b.clone()), &b.plus(a)));
            }

            #[test]
            fn plus_associative(a in $strat, b in $strat, c in $strat) {
                let eq = $eq;
                prop_assert!(eq(
                    &a.clone().plus(b.clone()).plus(c.clone()),
                    &a.plus(b.plus(c)),
                ));
            }

            #[test]
            fn plus_identity(a in $strat) {
                let eq = $eq;
                let zero = <$ty as $crate::algebra::semiring::Semiring>::zero();
                prop_assert!(eq(&a.clone().plus(zero), &a));
            }

            #[test]
            fn times_commutative(a in $strat, b in $strat) {
                let eq = $eq;
                prop_assert!(eq(&a.clone().times(b.clone()), &b.times(a)));
            }

            #[test]
            fn times_associative(a in $strat, b in $strat, c in $strat) {
                let eq = $eq;
                prop_assert!(eq(
                    &a.clone().times(b.clone()).times(c.clone()),
                    &a.times(b.times(c)),
                ));
            }

            #[test]
            fn times_identity(a in $strat) {
                let eq = $eq;
                let one = <$ty as $crate::algebra::semiring::Semiring>::one();
                prop_assert!(eq(&a.clone().times(one), &a));
            }

            #[test]
            fn distributive(a in $strat, b in $strat, c in $strat) {
                let eq = $eq;
                prop_assert!(eq(
                    &a.clone().times(b.clone().plus(c.clone())),
                    &a.clone().times(b).plus(a.times(c)),
                ));
            }

            #[test]
            fn zero_annihilates(a in $strat) {
                let eq = $eq;
                let zero = <$ty as $crate::algebra::semiring::Semiring>::zero();
                prop_assert!(eq(&zero.times(a), &<$ty as $crate::algebra::semiring::Semiring>::zero()));
            }
        }
    };
}

#[cfg(test)]
mod laws {
    use super::*;
    use proptest::prelude::*;

    fn arb_lineage() -> impl Strategy<Value = Lineage<u8>> {
        prop_oneof![
            1 => Just(Lineage::Bottom),
            4 => prop::collection::btree_set(0u8..=8, 0..=4).prop_map(Lineage::Of),
        ]
    }

    semiring_laws!(
        Lineage<u8>,
        arb_lineage(),
        |a: &Lineage<u8>, b: &Lineage<u8>| a == b
    );
}
