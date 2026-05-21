//! Event cluster — interior-lifetime event facts plus the cross-event
//! gap primitive.
//!
//! Cluster module for the `Event` variant of
//! [`crate::facts::assertions::FactualAssertion`]. Covers interior
//! lifetime events keyed by
//! [`crate::facts::ids::LifetimeEventId`]: dates (durational or point),
//! locations, damage causes, move methods, usage transitions,
//! designations, descriptions, and cross-event temporal gaps.
//!
//! [`OrderableEvent`] and [`GapBounds`] live here too. They underpin the
//! [`Fact::Gap`] variant — sources sometimes claim a relative temporal
//! relationship between two events without absolute dates ("Y happened
//! after X", "Y happened exactly five years after X"), and the gap shape
//! captures all of these uniformly.
//!
//! | Source claim                                     | `GapBounds` shape                               |
//! |--------------------------------------------------|-------------------------------------------------|
//! | "Y happened after X" (pure ordering)             | `min: Some(0), max: None`                       |
//! | "Y happened exactly five years after X"          | `min: Some(5y), max: Some(5y)`                  |
//! | "Y happened no more than five years after X"     | `min: Some(0), max: Some(5y)`                   |
//! | "Y happened at least five years after X"         | `min: Some(5y), max: None`                      |
//!
//! [`OrderableEvent`] includes only the lifetime-event endpoints that
//! have honest cross-boundary ordering use cases: a lifetime event by
//! reference, the construction *completion* point (when the building
//! first existed), and the demolition *start* point (when its existence
//! ended). The other bookend slots don't appear in real cross-boundary
//! ordering claims and are intentionally absent. We can add others if
//! the need arises.
//!
//! # Idiomatic partial-event patterns
//!
//! Sources don't always describe a lifetime event with both a kind and a
//! date. The grammar accommodates two partial shapes by composition
//! rather than by adding dedicated variants, because each piece —
//! "something happened on this entity at this time" or "this kind of
//! event happened, time unknown" — is a useful claim on its own.
//!
//! ## Typeless event (date, no kind-implying payload)
//!
//! An event id with a [`Fact::PointDate`] (or
//! [`Fact::DurationalDate`]) and no payload that implies a kind — no
//! [`Fact::DamageCause`], [`Fact::MoveMethod`], [`Fact::UsageChange`],
//! or [`Fact::Designation`] — represents "something happened on this
//! entity in this interval, we don't know what." This is the natural
//! shape for era-overlap claims: "the 19th-century X" mints a fresh
//! [`crate::facts::ids::LifetimeEventId`] on the entity and attaches a
//! single `PointDate { bound: <1800-01-01 .. 1900-01-01> }`. The
//! event's existence plus its date implies the entity was alive
//! somewhere in that span, which is exactly what the source claim
//! warrants.
//!
//! ## Undated event (kind-implying payload, no date)
//!
//! An event id with a kind-implying payload but no date represents
//! "this kind of event happened at some unknown time." "The X hotel"
//! mints a fresh [`crate::facts::ids::LifetimeEventId`] and attaches
//! a single [`Fact::UsageChange`] with `new_usages: {Hotel}` — the
//! usage transition is recorded without committing to when the
//! transition happened. The kind is whatever the payload implies
//! ([`crate::facts::lifecycle::LifetimeEventKind::UsageChanged`] in
//! this case); later sources may add a [`Fact::PointDate`] when the
//! date becomes known.
//!
//! # Error states (rejected at submit time)
//!
//! Error states are combinations the grammar permits structurally but
//! the fact-store layer rejects at submit time. They model bugs in
//! caller code, not outside-world uncertainty.
//!
//! ## Per-kind field availability
//!
//! Any [`Fact`] variant may attach to any event id at the type level;
//! the submit layer rejects facts whose variant doesn't match the
//! referenced event's
//! [`crate::facts::lifecycle::LifetimeEventKind`]. Rules consult the
//! event's own kind claim before applying.
//!
//! - [`Fact::DurationalDate`] is valid only when the referenced event's
//!   kind is durational (`Modified`, `Damaged`, `Repaired`, `Moved`).
//!   Point events use [`Fact::PointDate`].
//! - [`Fact::PointDate`] is valid only when the referenced event's kind
//!   is point (`UsageChanged`, `Designated`). Durational events use
//!   [`Fact::DurationalDate`] paired with a
//!   [`crate::facts::lifecycle::DurationalRole`].
//! - [`Fact::DamageCause`] is valid only when the referenced event's
//!   kind is `Damaged`.
//! - [`Fact::MoveMethod`] is valid only when the referenced event's
//!   kind is `Moved`.
//! - [`Fact::UsageChange`] is valid only when the referenced event's
//!   kind is `UsageChanged`.
//! - [`Fact::Designation`] is valid only when the referenced event's
//!   kind is `Designated`.
//! - [`Fact::Description`] is valid on any interior event kind.
//! - [`Fact::MovedToLocation`] is valid only when the referenced
//!   event's kind is `Moved` — a fire happens *at* a building but
//!   doesn't change its location, so any spatial fact about other
//!   interior events should live on the entity (via bookends) or on a
//!   depiction. Construction location lives on the bookend cluster (see
//!   [`crate::facts::bookend::Fact::Location`]); demolition location is
//!   derived (see [`crate::facts::bookend`]'s error-states section).
//!
//! ## Reference invariants
//!
//! - [`Fact::Gap`] requires both `from` and `to` endpoints to reference
//!   events that actually exist by the time projection runs. The
//!   wire-boundary smart constructor [`GapBounds::new`] enforces only
//!   the local invariants (at least one bound present, `min <= max`);
//!   the existence check is a submit-layer rule.
//!
//! # Conflicts (surfaced at projection time)
//!
//! Conflicts are claims the wire grammar admits and the submit layer
//! accepts; the projection / solver layers surface them as
//! user-resolvable contradictions.
//!
//! ## Slot semantics (per event id)
//!
//! Each rule below describes a *slot* per event id. Multiple facts at
//! the same slot are not a cardinality violation; what happens to them
//! depends on whether the slot's value type unifies under a lattice or
//! is a discrete value.
//!
//! **Lattice-valued slots (multiple facts unify; conflict is empty
//! meet):**
//!
//! - [`Fact::DurationalDate`] keyed by `(event, role)` — multiple bound
//!   intervals unify via interval meet. Source A claiming "1900-1910"
//!   and source B claiming "late 1900s, 1905-1908" yields a unified
//!   1905-1908. A `Temporal` conflict surfaces only when the meet is
//!   empty.
//! - [`Fact::PointDate`] keyed by `event` — same shape, single slot per
//!   event (no role).
//! - [`Fact::MovedToLocation`] keyed by `event` — multiple location
//!   claims unify via the [`crate::location::Location`] subsumption
//!   lattice. Disjoint locations produce a `UnionOf`; contradictions
//!   are visible as the `UnionOf` widening rather than collapsing.
//!
//! **Discrete-valued slots (multiple facts must agree; disagreement is
//! a conflict):**
//!
//! - [`Fact::DamageCause`] keyed by `event` — sources must agree on
//!   the cause. Disagreement (`Fire` vs `Flood`) is a conflict the
//!   solver surfaces, not a stored multi-value.
//! - [`Fact::MoveMethod`] keyed by `event` — sources must agree.
//! - [`Fact::UsageChange`] keyed by `event` — sources must agree on
//!   the post-event usage set. The set itself absorbs multi-use
//!   buildings (one fact with `new_usages: {R, C}`); but two facts
//!   asserting different sets are a contradiction, not a union.
//! - [`Fact::Designation`] keyed by `event` — sources must agree on
//!   the designation string. Multiple distinct designations model as
//!   multiple `Designated` events, each with its own id.
//!
//! ## Ordering and lifetime-window violations
//!
//! - For a durational event with both `Started` and `Completed`
//!   [`Fact::DurationalDate`] bounds, the `Started` interval must not
//!   end strictly after the `Completed` interval ends. Equal years
//!   collapse to a tie and are allowed — the check uses
//!   latest-vs-earliest, not strict containment.
//! - Every interior event keyed by an event id must fall within
//!   the entity's lifetime window: not before the entity's
//!   [`crate::facts::bookend::Fact::Started`] for construction, not
//!   after its [`crate::facts::bookend::Fact::Started`] for demolition.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::date::UncertainDate;
use crate::facts::lifecycle::{DamageCause, DurationalRole, MoveMethod, Usage};
use crate::location::UnresolvedLocation;

/// Event-cluster fact.
///
/// Generic over the entity reference type `EntId` and the lifetime-event
/// reference type `EvtId`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[serde(bound(
    serialize = "EntId: Serialize, EvtId: Serialize",
    deserialize = "EntId: serde::de::DeserializeOwned, EvtId: serde::de::DeserializeOwned"
))]
#[schemars(bound = "EntId: JsonSchema, EvtId: JsonSchema")]
pub enum Fact<EntId, EvtId> {
    /// An uncertain interval for one end (start or completion) of a
    /// durational lifetime event (`Modified`, `Damaged`, `Repaired`,
    /// `Moved`).
    DurationalDate {
        /// Which event the bound applies to.
        event: EvtId,
        /// Whether the bound describes the start or completion of the
        /// event's duration.
        role: DurationalRole,
        /// The source-claimed interval for that endpoint.
        bound: UncertainDate,
    },
    /// An uncertain interval for a point lifetime event
    /// (`UsageChanged`, `Designated`).
    PointDate {
        /// Which event the bound applies to.
        event: EvtId,
        /// The source-claimed interval for the event.
        bound: UncertainDate,
    },
    /// Where a `Moved` event landed the structure.
    MovedToLocation {
        /// Which event the location applies to.
        event: EvtId,
        /// The destination location of the move.
        location: UnresolvedLocation,
    },
    /// What caused a [`crate::facts::lifecycle::LifetimeEventKind::Damaged`]
    /// event.
    DamageCause {
        /// Which event the cause applies to.
        event: EvtId,
        /// The damage cause.
        cause: DamageCause,
    },
    /// How a [`crate::facts::lifecycle::LifetimeEventKind::Moved`] event
    /// was carried out.
    MoveMethod {
        /// Which event the method applies to.
        event: EvtId,
        /// The move method.
        method: MoveMethod,
    },
    /// New set of active uses after a
    /// [`crate::facts::lifecycle::LifetimeEventKind::UsageChanged`] event.
    UsageChange {
        /// Which event the change applies to.
        event: EvtId,
        /// The post-event usage set; empty represents closure or vacancy.
        new_usages: BTreeSet<Usage>,
    },
    /// Designation text for a
    /// [`crate::facts::lifecycle::LifetimeEventKind::Designated`] event.
    Designation {
        /// Which event the designation applies to.
        event: EvtId,
        /// The designation as the source phrased it.
        designation: String,
    },
    /// Free-form descriptive text attached to a lifetime event.
    Description {
        /// Which event the description applies to.
        event: EvtId,
        /// The descriptive text.
        text: String,
    },
    /// A claim about the temporal gap between two orderable events.
    /// Covers pure ordering ("Y after X"), bounded gaps, and exact gaps
    /// in a single shape — see the module docs for the encoding table.
    Gap(GapBounds<EntId, EvtId>),
}

// ============================================================================
// Days
// ============================================================================

/// Non-negative duration in days.
///
/// `Days` is a `u64` newtype — negative durations are structurally
/// impossible. Solver code that needs a signed quantity (e.g. when a
/// subtraction would go negative) flips operand order or projects into
/// an `i64` locally; the public type never exposes a sign.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct Days(u64);

impl Days {
    /// The zero-day distance.
    pub const ZERO: Self = Self(0);

    /// `u64::MAX / 4` in days — saturating ceiling with arithmetic
    /// headroom. Two such values can be added or tripled in
    /// constraint-propagation steps without overflowing `u64`.
    pub const MAX: Self = Self(u64::MAX / 4);

    /// Wrap a raw `u64` as a duration in days. Infallible — every `u64`
    /// is a well-formed non-negative duration.
    #[must_use]
    pub fn new(days: u64) -> Self {
        Self(days)
    }

    /// The underlying integer.
    #[must_use]
    pub fn as_u64(self) -> u64 {
        self.0
    }

    /// Saturating addition.
    #[must_use]
    pub fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }

    /// Saturating subtraction. Underflow saturates to zero.
    #[must_use]
    pub fn saturating_sub(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }
}

impl std::ops::Add for Days {
    type Output = Self;
    fn add(self, other: Self) -> Self {
        self.saturating_add(other)
    }
}

impl std::ops::Sub for Days {
    type Output = Self;
    fn sub(self, other: Self) -> Self {
        self.saturating_sub(other)
    }
}

// ============================================================================
// OrderableEvent
// ============================================================================

/// Endpoint of a [`Fact::Gap`].
///
/// Three shapes are honestly orderable across entity boundaries: a
/// lifetime event by reference (anywhere inside the entity's lifetime),
/// the *completion* of construction (the first moment the entity
/// existed), and the *start* of demolition (the last moment it existed).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[serde(bound(
    serialize = "EntId: Serialize, EvtId: Serialize",
    deserialize = "EntId: serde::de::DeserializeOwned, EvtId: serde::de::DeserializeOwned"
))]
#[schemars(bound = "EntId: JsonSchema, EvtId: JsonSchema")]
pub enum OrderableEvent<EntId, EvtId> {
    /// A lifetime event inside the entity's lifetime, by id.
    Event {
        /// The event id.
        event: EvtId,
    },
    /// The completion of an entity's construction — the first moment the
    /// entity existed.
    ConstructionCompletion {
        /// The entity whose construction completed.
        entity: EntId,
    },
    /// The start of an entity's demolition — the last moment it existed.
    DemolitionStart {
        /// The entity whose demolition started.
        entity: EntId,
    },
}

// ============================================================================
// GapBounds
// ============================================================================

/// A cross-event temporal gap claim.
///
/// `from` is conventionally the temporally-earlier endpoint; the gap is
/// `to - from`, expressed as `[min_days, max_days]` (either side may be
/// `None` to leave that side open). The smart constructor [`GapBounds::new`]
/// enforces:
///
/// - At least one of `min_days` / `max_days` is `Some` (otherwise the
///   gap is unconstrained and the fact is meaningless).
/// - When both are present, `min_days <= max_days`.
///
/// Non-negativity is structural — [`Days`] is a `u64` newtype.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(bound(
    serialize = "EntId: Serialize, EvtId: Serialize",
    deserialize = "EntId: serde::de::DeserializeOwned, EvtId: serde::de::DeserializeOwned"
))]
#[schemars(bound = "EntId: JsonSchema, EvtId: JsonSchema")]
pub struct GapBounds<EntId, EvtId> {
    /// The earlier endpoint.
    pub from: OrderableEvent<EntId, EvtId>,
    /// The later endpoint.
    pub to: OrderableEvent<EntId, EvtId>,
    /// Minimum gap in days; `None` leaves the lower side open.
    pub min_days: Option<Days>,
    /// Maximum gap in days; `None` leaves the upper side open.
    pub max_days: Option<Days>,
}

impl<EntId, EvtId> GapBounds<EntId, EvtId> {
    /// Construct a gap, validating the bound invariants.
    pub fn new(
        from: OrderableEvent<EntId, EvtId>,
        to: OrderableEvent<EntId, EvtId>,
        min_days: Option<Days>,
        max_days: Option<Days>,
    ) -> Result<Self, GapBoundsError> {
        match (min_days, max_days) {
            (None, None) => return Err(GapBoundsError::Unconstrained),
            (Some(lo), Some(hi)) if lo.as_u64() > hi.as_u64() => {
                return Err(GapBoundsError::MinExceedsMax {
                    min_days: lo.as_u64(),
                    max_days: hi.as_u64(),
                });
            }
            _ => {}
        }
        Ok(Self {
            from,
            to,
            min_days,
            max_days,
        })
    }
}

/// Errors from [`GapBounds::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GapBoundsError {
    /// Both `min_days` and `max_days` were `None`; the gap is meaningless
    /// without at least one bound.
    Unconstrained,
    /// `min_days > max_days` when both are present.
    MinExceedsMax { min_days: u64, max_days: u64 },
}

impl std::fmt::Display for GapBoundsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unconstrained => write!(
                f,
                "event gap must constrain at least one of min_days / max_days"
            ),
            Self::MinExceedsMax { min_days, max_days } => write!(
                f,
                "min_days ({min_days}) must not exceed max_days ({max_days})"
            ),
        }
    }
}

impl std::error::Error for GapBoundsError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::ids::{EntityId, IdError, LifetimeEventId};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn anchor() -> Result<OrderableEvent<EntityId, LifetimeEventId>, IdError> {
        Ok(OrderableEvent::ConstructionCompletion {
            entity: EntityId::new("a")?,
        })
    }

    fn other() -> Result<OrderableEvent<EntityId, LifetimeEventId>, IdError> {
        Ok(OrderableEvent::DemolitionStart {
            entity: EntityId::new("b")?,
        })
    }

    #[test]
    fn gap_bounds_rejects_both_open() -> TestResult {
        assert_eq!(
            GapBounds::new(anchor()?, other()?, None, None),
            Err(GapBoundsError::Unconstrained)
        );
        Ok(())
    }

    #[test]
    fn gap_bounds_rejects_min_exceeds_max() -> TestResult {
        assert_eq!(
            GapBounds::new(anchor()?, other()?, Some(Days::new(10)), Some(Days::new(5))),
            Err(GapBoundsError::MinExceedsMax {
                min_days: 10,
                max_days: 5
            })
        );
        Ok(())
    }
}
