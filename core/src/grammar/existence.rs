//! Existence cluster — a per-entity existence witness.
//!
//! Cluster module for the `Existence` variant of
//! [`crate::grammar::assertions::FactualAssertion`]. One witness asserts that the
//! entity provably existed at a date — a founding record, a first-mention, any
//! evidence that fixes a moment the entity was already there.
//!
//! A witness dates *presence* — evidence the entity was already there. It
//! constrains the construction bookend: a witness falls within the lifetime
//! window `construction-start ≤ at ≤ demolition`, and a witness before the
//! construction start is a contradiction surfaced at read time
//! ([`crate::solvers::temporal_conflicts`]), the outside world uncertain about
//! history rather than a caller bug.

use chronoscope_macros::grammar_type;

use crate::date::UncertainDate;
use crate::grammar::ids::IdScheme;

/// Existence-cluster fact — the entity provably existed at `at`.
///
/// Generic over one id scheme `R: IdScheme`, reading `R::Entity`. Backs
/// [`FactualAssertion::Existence`](crate::grammar::assertions::FactualAssertion::Existence).
/// A single grammar struct (a product): the outer variant wrapper tags it, so the
/// inner fact needs no tag of its own.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fact<R: IdScheme> {
    /// The entity the witness is about.
    pub entity: R::Entity,
    /// A date the entity is attested to have existed at, as an uncertain interval.
    pub at: UncertainDate,
}

impl<R: IdScheme> Fact<R> {
    /// The entity this witness is a claim about.
    pub fn subject(&self) -> &R::Entity {
        &self.entity
    }
}
