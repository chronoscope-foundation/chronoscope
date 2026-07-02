//! Bookend cluster — flat per-entity construction and demolition facts.
//!
//! [`ConstructionFact`] backs
//! [`FactualAssertion::Construction`](crate::facts::assertions::FactualAssertion::Construction)
//! and [`DemolitionFact`] backs
//! [`FactualAssertion::Demolition`](crate::facts::assertions::FactualAssertion::Demolition).
//! Construction carries a start date, a completion date, and a location;
//! demolition carries a start and a completion date. Demolition location is
//! derived from the entity's last known location — the most recent
//! [`crate::facts::event::Fact::MovedToLocation`], falling back to
//! [`ConstructionFact::Location`] — so the two phases are distinct types free to
//! evolve apart.
//!
//! Bookends are flat per-entity facts rather than event-mediated. Once-ness for
//! these slots is structural — an entity has at most one construction and at
//! most one demolition, modeled directly without minting a lifetime-event id.
//!
//! # Conflicts (surfaced at projection time)
//!
//! Claims the wire grammar admits and the submit layer accepts; the projection
//! / solver layers surface them as user-resolvable contradictions — the outside
//! world being uncertain about history, not bugs in caller code.
//!
//! Each rule applies independently within the `Construction` and `Demolition`
//! outer-variant scopes on [`crate::facts::assertions::FactualAssertion`] — an
//! entity can carry both a `Construction::*` and a `Demolition::*` slot.
//!
//! ## Slot unification
//!
//! Two or more `Started` facts on the same entity-phase pair are not a
//! cardinality conflict. The projection unifies their bounds via interval meet
//! (the intersection of the source-claimed intervals). A `Temporal` conflict
//! surfaces only when the meet is empty — when the source claims are mutually
//! contradictory. Same for `Completed`.
//!
//! [`ConstructionFact::Location`] unifies via the
//! [`crate::location::Location`] subsumption lattice: containment collapses to
//! the tighter region; disjoint regions produce a `OneOf` ("one of these is
//! true").
//!
//! ## Intra-phase ordering
//!
//! - For a single phase, `Started`'s unified interval must not end strictly
//!   after `Completed`'s unified interval ends.
//!
//! ## Inter-phase ordering
//!
//! - `Construction::Started` must precede `Construction::Completed`
//!   (intra-phase, above).
//! - `Construction::Completed` must precede every interior
//!   [`crate::facts::event::Fact`] keyed on the entity, and must precede
//!   `Demolition::Started`.
//! - `Demolition::Started` must follow every interior
//!   [`crate::facts::event::Fact`] keyed on the entity. Date evidence that
//!   contradicts the structural order is a violation, not a sort-key override.
//! - `Demolition::Started` must precede `Demolition::Completed` (intra-phase).
//!
//! ## Location semantics
//!
//! - `Construction::Location` records where the entity was built. It is the
//!   default location for the entity until a subsequent
//!   [`crate::facts::event::Fact::MovedToLocation`] on a `Moved` event overrides
//!   it.

use chronoscope_macros::grammar_type;

use crate::date::UncertainDate;
use crate::location::UnresolvedLocation;

/// Construction bookend fact — start date, completion date, or location.
///
/// Generic over the entity reference type `EntId`. Backs
/// [`FactualAssertion::Construction`](crate::facts::assertions::FactualAssertion::Construction).
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(bound(
    serialize = "EntId: ::serde::Serialize",
    deserialize = "EntId: ::serde::de::DeserializeOwned"
))]
#[schemars(bound = "EntId: ::schemars::JsonSchema")]
pub enum ConstructionFact<EntId> {
    /// When construction started, as an uncertain interval.
    Started {
        entity: EntId,
        /// The source-claimed interval for the start.
        bound: UncertainDate,
    },
    /// When construction completed, as an uncertain interval.
    Completed {
        entity: EntId,
        /// The source-claimed interval for the completion.
        bound: UncertainDate,
    },
    /// Where the entity was built — its default location until a subsequent
    /// `Moved` event overrides it.
    Location {
        entity: EntId,
        location: UnresolvedLocation,
    },
}

impl<EntId> ConstructionFact<EntId> {
    /// The entity this construction fact is a claim about.
    pub fn subject(&self) -> &EntId {
        match self {
            Self::Started { entity, .. }
            | Self::Completed { entity, .. }
            | Self::Location { entity, .. } => entity,
        }
    }

    /// Visit the single entity id this fact mentions.
    pub fn for_each_id(&self, fe: &mut impl FnMut(&EntId)) {
        match self {
            Self::Started { entity, .. }
            | Self::Completed { entity, .. }
            | Self::Location { entity, .. } => fe(entity),
        }
    }

    /// Relabel the single entity id through the fallible closure, producing a
    /// `ConstructionFact<E2>`.
    ///
    /// No distinct-pair payload, so no `on_self_loop` collapse closure: the only
    /// failure is the leaf closure rejecting a reference. Generic over the error
    /// type `Err` so the cluster never names the concrete error the assertion
    /// layer chooses.
    pub fn try_map_ids<E2, Err>(
        &self,
        fe: &mut impl FnMut(&EntId) -> Result<E2, Err>,
    ) -> Result<ConstructionFact<E2>, Err> {
        match self {
            Self::Started { entity, bound } => Ok(ConstructionFact::Started {
                entity: fe(entity)?,
                bound: bound.clone(),
            }),
            Self::Completed { entity, bound } => Ok(ConstructionFact::Completed {
                entity: fe(entity)?,
                bound: bound.clone(),
            }),
            Self::Location { entity, location } => Ok(ConstructionFact::Location {
                entity: fe(entity)?,
                location: location.clone(),
            }),
        }
    }
}

/// Demolition bookend fact — start date or completion date.
///
/// Generic over the entity reference type `EntId`. Backs
/// [`FactualAssertion::Demolition`](crate::facts::assertions::FactualAssertion::Demolition).
/// Demolition location is derived from the entity's last known location, so it
/// has no location slot.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(bound(
    serialize = "EntId: ::serde::Serialize",
    deserialize = "EntId: ::serde::de::DeserializeOwned"
))]
#[schemars(bound = "EntId: ::schemars::JsonSchema")]
pub enum DemolitionFact<EntId> {
    /// When demolition started, as an uncertain interval.
    Started {
        entity: EntId,
        /// The source-claimed interval for the start.
        bound: UncertainDate,
    },
    /// When demolition completed, as an uncertain interval.
    Completed {
        entity: EntId,
        /// The source-claimed interval for the completion.
        bound: UncertainDate,
    },
}

impl<EntId> DemolitionFact<EntId> {
    /// The entity this demolition fact is a claim about.
    pub fn subject(&self) -> &EntId {
        match self {
            Self::Started { entity, .. } | Self::Completed { entity, .. } => entity,
        }
    }

    /// Visit the single entity id this fact mentions.
    pub fn for_each_id(&self, fe: &mut impl FnMut(&EntId)) {
        match self {
            Self::Started { entity, .. } | Self::Completed { entity, .. } => fe(entity),
        }
    }

    /// Relabel the single entity id through the fallible closure, producing a
    /// `DemolitionFact<E2>`. See [`ConstructionFact::try_map_ids`].
    pub fn try_map_ids<E2, Err>(
        &self,
        fe: &mut impl FnMut(&EntId) -> Result<E2, Err>,
    ) -> Result<DemolitionFact<E2>, Err> {
        match self {
            Self::Started { entity, bound } => Ok(DemolitionFact::Started {
                entity: fe(entity)?,
                bound: bound.clone(),
            }),
            Self::Completed { entity, bound } => Ok(DemolitionFact::Completed {
                entity: fe(entity)?,
                bound: bound.clone(),
            }),
        }
    }
}
