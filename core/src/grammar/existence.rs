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

/// Existence-cluster fact — the entity provably existed at `at`.
///
/// Generic over the entity reference type `EntId`. Backs
/// [`FactualAssertion::Existence`](crate::grammar::assertions::FactualAssertion::Existence).
/// A single grammar struct (a product): the outer variant wrapper tags it, so the
/// inner fact needs no tag of its own.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(bound(
    serialize = "EntId: ::serde::Serialize",
    deserialize = "EntId: ::serde::de::DeserializeOwned"
))]
#[schemars(bound = "EntId: ::schemars::JsonSchema")]
pub struct Fact<EntId> {
    /// The entity the witness is about.
    pub entity: EntId,
    /// A date the entity is attested to have existed at, as an uncertain interval.
    pub at: UncertainDate,
}

impl<EntId> Fact<EntId> {
    /// The entity this witness is a claim about.
    pub fn subject(&self) -> &EntId {
        &self.entity
    }

    /// Visit the single entity id this fact mentions.
    pub fn for_each_id(&self, fe: &mut impl FnMut(&EntId)) {
        fe(&self.entity);
    }

    /// Relabel the single entity id through the fallible closure, producing a
    /// `Fact<E2>`. The only failure is the leaf closure rejecting a reference.
    pub fn try_map_ids<E2, Err>(
        &self,
        fe: &mut impl FnMut(&EntId) -> Result<E2, Err>,
    ) -> Result<Fact<E2>, Err> {
        Ok(Fact {
            entity: fe(&self.entity)?,
            at: self.at.clone(),
        })
    }
}
