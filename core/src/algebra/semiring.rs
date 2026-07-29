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

// ============================================================================
// Label — an ATMS antichain of minimal environments
// ============================================================================

/// A set of atoms that jointly support one derivation of a value — an
/// "environment" in de Kleer's assumption-based TMS (Artificial Intelligence
/// 28, 1986). Keyed on the atom's own `Ord`, so two atoms that differ in any
/// field stay distinct members.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Environment<X>(BTreeSet<X>);

impl<X: Ord + Clone> Environment<X> {
    /// The empty environment — the tautological support.
    fn empty() -> Self {
        Self(BTreeSet::new())
    }

    /// The environment resting on a single atom.
    fn single(atom: X) -> Self {
        Self([atom].into_iter().collect())
    }

    /// The union of two environments — the joint support of using both.
    fn union(&self, other: &Self) -> Self {
        Self(self.0.iter().chain(&other.0).cloned().collect())
    }

    /// Whether every atom of `self` also lies in `other`.
    fn is_subset(&self, other: &Self) -> bool {
        self.0.is_subset(&other.0)
    }
}

/// The provenance carrier for a projected value: the minimal environments under
/// which it holds, kept as an antichain — no environment is a superset of
/// another. A commutative semiring, with `zero` the empty label, `one` the `{∅}`
/// tautology, `times` the cross-union (joint support, the consensus meet), and
/// `plus` the minimized union of environments (alternatives, the extent join).
///
/// The environments live in a `BTreeSet`, so equality is set equality: two
/// labels built by different fold orders compare equal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label<X> {
    environments: BTreeSet<Environment<X>>,
}

impl<X: Ord + Clone> Label<X> {
    /// The empty label — the additive identity; a value no derivation reaches.
    pub fn empty() -> Self {
        Self {
            environments: BTreeSet::new(),
        }
    }

    /// A premise: the value rests on exactly one atom.
    pub fn premise(atom: X) -> Self {
        Self {
            environments: [Environment::single(atom)].into_iter().collect(),
        }
    }

    /// Rewrite every atom through `f`, re-minimizing the result.
    ///
    /// The map can send two atoms to one, which can make an environment a subset
    /// of another, so the antichain is rebuilt rather than carried over. A
    /// producer retagging the atoms it consumed is the one caller.
    pub(crate) fn map_atoms<Y: Ord + Clone>(self, f: impl Fn(X) -> Y) -> Label<Y> {
        let envs: Vec<Environment<Y>> = self
            .environments
            .into_iter()
            .map(|env| Environment(env.0.into_iter().map(&f).collect()))
            .collect();
        Label {
            environments: minimize_environments(envs),
        }
    }

    /// Cross-union: each environment of `self` unioned with each of `other`,
    /// minimized back to an antichain. The multiplicative op — joint support.
    fn cross_union(&self, other: &Self) -> Self {
        let mut envs = Vec::with_capacity(self.environments.len() * other.environments.len());
        for a in &self.environments {
            for b in &other.environments {
                envs.push(a.union(b));
            }
        }
        Self {
            environments: minimize_environments(envs),
        }
    }
}

/// Reduce environments to an antichain: keep only those minimal under subset,
/// dropping every strict (and duplicate) superset. Order-independent, so the
/// resulting set is canonical.
fn minimize_environments<X: Ord + Clone>(envs: Vec<Environment<X>>) -> BTreeSet<Environment<X>> {
    let mut kept: Vec<Environment<X>> = Vec::with_capacity(envs.len());
    'candidate: for env in envs {
        for keep in &kept {
            if keep.is_subset(&env) {
                continue 'candidate;
            }
        }
        kept.retain(|keep| !env.is_subset(keep));
        kept.push(env);
    }
    kept.into_iter().collect()
}

impl<X: Ord + Clone> Semiring for Label<X> {
    fn zero() -> Self {
        Label::empty()
    }

    fn one() -> Self {
        Self {
            environments: [Environment::empty()].into_iter().collect(),
        }
    }

    fn plus(self, other: Self) -> Self {
        let mut envs: Vec<Environment<X>> = self.environments.into_iter().collect();
        envs.extend(other.environments);
        Self {
            environments: minimize_environments(envs),
        }
    }

    fn times(self, other: Self) -> Self {
        self.cross_union(&other)
    }
}

/// A provenance container the typed flatten reads without knowing its shape: the
/// atoms backing a value, and whether the container is the semiring zero (no
/// derivation contributed).
pub trait Support {
    /// The provenance atom the container holds.
    type Atom;

    /// The atoms across every environment, deduplicated.
    ///
    /// This answers what did contribute, never what would change the value. A
    /// container records the atoms present when it was built, so a question about
    /// an atom that does not exist yet has no answer here, and one about removing
    /// an atom is a counterfactual the record cannot settle: a producer gated on
    /// an empty slot fires *because* of a retraction, repopulating the value the
    /// retraction was meant to remove. Anything asking what could change reads the
    /// collections a value was drawn from, not the atoms it drew.
    fn atoms(&self) -> impl Iterator<Item = &Self::Atom>;

    /// Whether this is the additive-identity container — nothing contributed.
    fn is_zero(&self) -> bool;
}

impl<X: Ord> Support for Label<X> {
    type Atom = X;

    fn atoms(&self) -> impl Iterator<Item = &X> {
        self.environments
            .iter()
            .flat_map(|env| env.0.iter())
            .collect::<BTreeSet<_>>()
            .into_iter()
    }

    fn is_zero(&self) -> bool {
        self.environments.is_empty()
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
mod label_laws {
    use super::*;
    use proptest::prelude::*;

    fn arb_environment() -> impl Strategy<Value = Environment<u8>> {
        prop::collection::btree_set(0u8..=6, 0..=3).prop_map(Environment)
    }

    fn arb_label() -> impl Strategy<Value = Label<u8>> {
        prop::collection::vec(arb_environment(), 0..=4).prop_map(|envs| Label {
            environments: minimize_environments(envs),
        })
    }

    // The canonical `BTreeSet` equality is the equivalence the laws hold under,
    // so no custom comparator is needed.
    semiring_laws!(Label<u8>, arb_label(), |a: &Label<u8>, b: &Label<u8>| a
        == b);
}
