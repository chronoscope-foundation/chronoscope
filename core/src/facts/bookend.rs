//! Bookend cluster — flat per-entity construction and demolition facts.
//!
//! Shared between the `Construction` and `Demolition` variants of
//! [`crate::facts::assertions::FactualAssertion`]: both bookends carry
//! the same shape (start date, completion date, location), so a single
//! cluster module covers both. The outer variant tag distinguishes the
//! two phases.
//!
//! Bookends are flat per-entity facts rather than event-mediated. Once-ness
//! for these slots is structural — an entity has at most one construction
//! and at most one demolition, both modeled directly without minting a
//! [`crate::facts::ids::LifetimeEventId`].
//!
//! # Error states (rejected at submit time)
//!
//! Error states are combinations the grammar permits structurally but
//! the fact-store layer rejects at submit time. They model bugs in
//! caller code, not outside-world uncertainty.
//!
//! - **`Demolition::Location` is invalid.** The variant is structurally
//!   reachable (bookend [`Fact`] is shared between phases) but the
//!   submit layer rejects any [`Fact::Location`] whose outer variant is
//!   `FactualAssertion::Demolition`. Demolition location is derived
//!   from the entity's last known location — the most recent
//!   [`crate::facts::event::Fact::MovedToLocation`], falling back to
//!   [`Fact::Location`] on the `Construction` phase. A separately
//!   asserted demolition location would either duplicate that derivation
//!   (when it agrees) or contradict it (when it doesn't), and neither is
//!   a useful slot to maintain.
//!
//! # Conflicts (surfaced at projection time)
//!
//! Conflicts are claims the wire grammar admits and the submit layer
//! accepts; the projection / solver layers surface them as
//! user-resolvable contradictions. These model the outside world being
//! uncertain about history, not bugs in caller code.
//!
//! Each rule applies independently within the `Construction` and
//! `Demolition` outer-variant scopes on
//! [`crate::facts::assertions::FactualAssertion`] — an entity can carry
//! both a `Construction::*` and a `Demolition::*` slot.
//!
//! ## Slot unification
//!
//! Two or more [`Fact::Started`] facts on the same entity-phase pair are
//! **not** a cardinality conflict. The projection unifies their bounds
//! via interval meet (the intersection of the source-claimed intervals).
//! A `Temporal` conflict surfaces only when the meet is empty — i.e.
//! when the source claims are mutually contradictory. Same for
//! [`Fact::Completed`].
//!
//! [`Fact::Location`] (on the `Construction` phase only) unifies via
//! the [`crate::location::Location`] subsumption lattice: containment
//! collapses to the tighter region; disjoint regions produce a
//! `UnionOf` ("one of these is true").
//!
//! ## Intra-phase ordering
//!
//! - For a single phase, [`Fact::Started`]'s unified interval must not
//!   end strictly after [`Fact::Completed`]'s unified interval ends.
//!
//! ## Inter-phase ordering
//!
//! - `Construction::Started` must precede `Construction::Completed`
//!   (intra-phase, above).
//! - `Construction::Completed` must precede every interior
//!   [`crate::facts::event::Fact`] keyed on the entity, and must precede
//!   `Demolition::Started`.
//! - `Demolition::Started` must follow every interior
//!   [`crate::facts::event::Fact`] keyed on the entity. Date evidence
//!   that contradicts the structural order is a violation, not a
//!   sort-key override.
//! - `Demolition::Started` must precede `Demolition::Completed`
//!   (intra-phase).
//!
//! ## Location semantics
//!
//! - `Construction::Location` records *where the entity was built*. It
//!   is the default location for the entity until a subsequent
//!   [`crate::facts::event::Fact::MovedToLocation`] on a `Moved` event
//!   overrides it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::date::UncertainDate;
use crate::location::UnresolvedLocation;

/// Bookend-cluster fact (construction or demolition; the phase is the
/// outer variant tag on
/// [`crate::facts::assertions::FactualAssertion`]).
///
/// Generic over the entity reference type `EntId`. See the module-level
/// "Error states" section for the `Demolition::Location` rejection rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[serde(bound(
    serialize = "EntId: Serialize",
    deserialize = "EntId: serde::de::DeserializeOwned"
))]
#[schemars(bound = "EntId: JsonSchema")]
pub enum Fact<EntId> {
    /// When the bookend phase started, as an uncertain interval.
    Started {
        /// The entity the bookend applies to.
        entity: EntId,
        /// The source-claimed interval for the start.
        bound: UncertainDate,
    },
    /// When the bookend phase completed, as an uncertain interval.
    Completed {
        /// The entity the bookend applies to.
        entity: EntId,
        /// The source-claimed interval for the completion.
        bound: UncertainDate,
    },
    /// Where the bookend phase took place. Valid only on the
    /// `Construction` outer variant — see the module-level
    /// "Error states" section for the demolition rejection rule.
    Location {
        /// The entity the bookend applies to.
        entity: EntId,
        /// The bookend location.
        location: UnresolvedLocation,
    },
}
