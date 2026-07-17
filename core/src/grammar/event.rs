//! Event cluster — interior-lifetime event facts plus the cross-event
//! gap primitive.
//!
//! Cluster module for the `Event` variant of
//! [`crate::grammar::assertions::FactualAssertion`]. Covers interior
//! lifetime events keyed by a lifetime-event id: dates (durational or point),
//! locations, damage causes, move methods, usage transitions,
//! designations, descriptions, and cross-event temporal gaps.
//!
//! [`OrderableEvent`] and [`GapBounds`] live here too. They underpin the
//! [`crate::grammar::assertions::FactualAssertion::Gap`] variant — sources
//! sometimes claim a relative temporal relationship between two events without
//! absolute dates ("Y happened after X", "Y happened exactly five years after
//! X"), and the gap shape captures all of these uniformly. A gap is an ordering
//! relationship over events and entity bookends, not an event with one subject,
//! so it sits beside the event cluster rather than inside [`Fact`].
//!
//! | Source claim                                     | `GapBounds` shape                               |
//! |--------------------------------------------------|-------------------------------------------------|
//! | "Y happened after X" (pure ordering)             | `min: Some(0), max: None`                       |
//! | "Y happened exactly five years after X"          | `min: Some(5y), max: Some(5y)`                  |
//! | "Y happened no more than five years after X"     | `min: Some(0), max: Some(5y)`                   |
//! | "Y happened at least five years after X"         | `min: Some(5y), max: None`                      |
//!
//! [`OrderableEvent`] includes only the lifetime-event endpoints with
//! cross-boundary ordering use cases: a lifetime event by reference, the
//! construction completion point (when the building first existed), and the
//! demolition start point (when its existence ended). The other bookend slots
//! don't appear in real cross-boundary ordering claims and are absent; we can
//! add others if the need arises.
//!
//! # Typing: every event declares its kind
//!
//! A [`Fact::HasEvent`] ties an event id to its one subject entity and declares
//! its [`crate::grammar::lifecycle::LifetimeEventKind`]. The submit layer requires
//! exactly one per event id, so an event is never typeless and the kind is a
//! stored claim rather than an inference off the payloads. The payload facts
//! (`DamageCause`, `MoveMethod`, `UsageChange`, `Designation`) carry pure data.
//!
//! A date may still be unknown: a source noting a building became a hotel, with
//! no date attached, mints an event with `HasEvent { kind: Point(UsageChanged)
//! }` and a [`Fact::UsageChange`]; a later source adds a [`Fact::PointDate`]
//! when the date surfaces.
//!
//! # Per-kind field availability (enforced at submit time)
//!
//! A payload or date fact must suit the kind its event's `HasEvent` declares —
//! a `DamageCause` only on a `Damaged` event, a `DurationalDate` only on a
//! durational kind, and so on. [`Fact::kind_constraints`] is the authoritative
//! mapping; the submit layer intersects it against the declared kind. Any
//! [`Fact`] variant attaches to any event id at the type level, so the guard
//! lives in the submit layer rather than the grammar.
//!
//! [`Fact::MovedToLocation`] carries rationale beyond the mapping: a fire
//! happens *at* a building but doesn't change its location, so spatial facts
//! about other interior events belong on the entity (via bookends) or a
//! depiction. Construction location lives on the bookend cluster (see
//! [`crate::grammar::bookend::ConstructionFact::Location`]); demolition location
//! is derived (see [`crate::grammar::bookend`]).
//!
//! ## Reference invariants
//!
//! - [`crate::grammar::assertions::FactualAssertion::Gap`] requires both `from`
//!   and `to` endpoints to reference events that actually exist by the time
//!   projection runs. The wire-boundary smart constructor [`GapBounds::new`]
//!   enforces only the local invariants (at least one bound present,
//!   `min <= max`); the existence check is a submit-layer rule.
//!
//! # Conflicts (surfaced at projection time)
//!
//! Claims the wire grammar admits and the submit layer accepts; the projection
//! / solver layers surface them as user-resolvable contradictions.
//!
//! ## Slot semantics (per event id)
//!
//! Each rule below describes a slot per event id. Multiple facts at the same
//! slot are not a cardinality violation; what happens to them depends on whether
//! the slot's value type unifies under a lattice or is discrete.
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
//!   lattice. Disjoint locations produce a `OneOf`; contradictions
//!   are visible as the `OneOf` widening rather than collapsing.
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
//!   [`crate::grammar::bookend::ConstructionFact::Started`], not after its
//!   [`crate::grammar::bookend::DemolitionFact::Started`].

use std::collections::BTreeSet;

use chronoscope_macros::{IdWalk, grammar_type};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::date::UncertainDate;
use crate::grammar::ids::IdScheme;
use crate::grammar::lifecycle::{
    DamageCause, DurationalRole, LifetimeEventKind, MoveMethod, Usage,
};
use crate::location::UnresolvedLocation;

/// Event-cluster fact.
///
/// Generic over one id scheme `R: IdScheme`, reading the entity reference type
/// `R::Entity` and the lifetime-event reference type `R::Event`.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum Fact<R: IdScheme> {
    /// Ties an interior event to its subject entity and declares its kind.
    /// Each minted event carries exactly one; a `SameEvent` class may carry
    /// several when its sources disagree on the kind, a stored conflict the
    /// projection reads back as an undetermined kind.
    HasEvent {
        /// The entity whose lifetime this event belongs to.
        entity: R::Entity,
        /// The event being typed.
        event: R::Event,
        /// The event's declared kind.
        kind: LifetimeEventKind,
    },
    /// An uncertain interval for one end (start or completion) of a
    /// durational lifetime event (`Modified`, `Damaged`, `Repaired`,
    /// `Moved`).
    DurationalDate {
        event: R::Event,
        /// Whether the bound describes the start or completion of the
        /// event's duration.
        role: DurationalRole,
        /// The source-claimed interval for that endpoint.
        bound: UncertainDate,
    },
    /// An uncertain interval for a point lifetime event
    /// (`UsageChanged`, `Designated`).
    PointDate {
        event: R::Event,
        /// The source-claimed interval for the event.
        bound: UncertainDate,
    },
    /// Where a `Moved` event landed the structure.
    MovedToLocation {
        event: R::Event,
        /// The destination location of the move.
        location: UnresolvedLocation,
    },
    /// What caused a [`crate::grammar::lifecycle::DurationalKind::Damaged`]
    /// event.
    DamageCause { event: R::Event, cause: DamageCause },
    /// How a [`crate::grammar::lifecycle::DurationalKind::Moved`] event
    /// was carried out.
    MoveMethod { event: R::Event, method: MoveMethod },
    /// New set of active uses after a
    /// [`crate::grammar::lifecycle::PointKind::UsageChanged`] event.
    ///
    /// Operational status — opened, closed, repurposed — is modeled as a
    /// point-in-time transition. A fuller model would carry explicit in-use and
    /// out-of-use intervals; the transition points approximate those spans at
    /// the resolution sources supply.
    UsageChange {
        event: R::Event,
        /// The post-event usage set; empty represents closure or vacancy.
        new_usages: BTreeSet<Usage>,
    },
    /// Designation text for a
    /// [`crate::grammar::lifecycle::PointKind::Designated`] event.
    Designation {
        event: R::Event,
        /// The designation as the source phrased it.
        designation: String,
    },
    /// Free-form descriptive text attached to a lifetime event.
    Description { event: R::Event, text: String },
}

impl<R: IdScheme> Fact<R> {
    /// The single lifetime-event id this fact mentions as its subject.
    pub fn subject(&self) -> &R::Event {
        match self {
            Self::HasEvent { event, .. }
            | Self::DurationalDate { event, .. }
            | Self::PointDate { event, .. }
            | Self::MovedToLocation { event, .. }
            | Self::DamageCause { event, .. }
            | Self::MoveMethod { event, .. }
            | Self::UsageChange { event, .. }
            | Self::Designation { event, .. }
            | Self::Description { event, .. } => event,
        }
    }

    /// The lifetime-event kinds this fact admits for its event — the data the
    /// kind typecheck intersects against the declared kind. `HasEvent` pins its
    /// one declared kind; a description admits all; every payload delegates to
    /// [`EventPayload::allowed_kinds`], the shared payload→kinds rule.
    pub fn kind_constraints(&self) -> BTreeSet<LifetimeEventKind> {
        match self {
            Self::HasEvent { kind, .. } => BTreeSet::from([*kind]),
            Self::Description { .. } => LifetimeEventKind::durational_kinds()
                .chain(LifetimeEventKind::point_kinds())
                .collect(),
            Self::DurationalDate { .. } => EventPayload::DurationalDate.allowed_kinds(),
            Self::PointDate { .. } => EventPayload::PointDate.allowed_kinds(),
            Self::MovedToLocation { .. } => EventPayload::MovedToLocation.allowed_kinds(),
            Self::DamageCause { .. } => EventPayload::DamageCause.allowed_kinds(),
            Self::MoveMethod { .. } => EventPayload::MoveMethod.allowed_kinds(),
            Self::UsageChange { .. } => EventPayload::UsageChange.allowed_kinds(),
            Self::Designation { .. } => EventPayload::Designation.allowed_kinds(),
        }
    }
}

/// The payload categories an event fact carries, each admitting the lifetime
/// event kind(s) it suits. One source of the payload→kinds rule: the submit
/// kind typecheck reads it through [`Fact::kind_constraints`], and the typed
/// off-kind guard reads it directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EventPayload {
    /// A start/completion bound on a durational event.
    DurationalDate,
    /// A point event's date.
    PointDate,
    /// A `Moved` event's destination.
    MovedToLocation,
    /// A `Damaged` event's cause.
    DamageCause,
    /// A `Moved` event's method.
    MoveMethod,
    /// A `UsageChanged` event's post-event usage set.
    UsageChange,
    /// A `Designated` event's designation text.
    Designation,
}

impl EventPayload {
    /// The lifetime-event kinds this payload suits — the durational or point
    /// family for the date payloads, a single kind for the rest.
    pub fn allowed_kinds(self) -> BTreeSet<LifetimeEventKind> {
        use crate::grammar::lifecycle::{DurationalKind, PointKind};
        match self {
            Self::DurationalDate => LifetimeEventKind::durational_kinds().collect(),
            Self::PointDate => LifetimeEventKind::point_kinds().collect(),
            Self::MovedToLocation | Self::MoveMethod => {
                BTreeSet::from([LifetimeEventKind::Durational {
                    kind: DurationalKind::Moved,
                }])
            }
            Self::DamageCause => BTreeSet::from([LifetimeEventKind::Durational {
                kind: DurationalKind::Damaged,
            }]),
            Self::UsageChange => BTreeSet::from([LifetimeEventKind::Point {
                kind: PointKind::UsageChanged,
            }]),
            Self::Designation => BTreeSet::from([LifetimeEventKind::Point {
                kind: PointKind::Designated,
            }]),
        }
    }
}

// ============================================================================
// Days
// ============================================================================

/// Non-negative duration in days.
///
/// `Days` is a `u64` newtype — negative durations are unrepresentable. Solver
/// code that needs a signed quantity (e.g. when a subtraction would go
/// negative) flips operand order or projects into an `i64` locally; the public
/// type never exposes a sign.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct Days(u64);

impl Days {
    pub const ZERO: Self = Self(0);

    /// `u64::MAX / 4` in days — saturating ceiling with arithmetic headroom, so
    /// two such values can be added or tripled in constraint-propagation steps
    /// without overflowing `u64`.
    pub const MAX: Self = Self(u64::MAX / 4);

    /// Wrap a raw `u64` as a duration in days. Infallible — every `u64`
    /// is a well-formed non-negative duration.
    pub fn new(days: u64) -> Self {
        Self(days)
    }

    pub fn as_u64(self) -> u64 {
        self.0
    }

    pub fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }

    /// Underflow saturates to zero.
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

/// Endpoint of a [`crate::grammar::assertions::FactualAssertion::Gap`].
///
/// Three shapes are orderable across entity boundaries: a lifetime event by
/// reference (anywhere inside the entity's lifetime), the completion of
/// construction (the first moment the entity existed), and the start of
/// demolition (the last moment it existed).
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OrderableEvent<R: IdScheme> {
    /// A lifetime event inside the entity's lifetime, by id.
    Event { event: R::Event },
    /// The completion of an entity's construction — the first moment the
    /// entity existed.
    ConstructionCompletion {
        /// The entity whose construction completed.
        entity: R::Entity,
    },
    /// The start of an entity's demolition — the last moment it existed.
    DemolitionStart {
        /// The entity whose demolition started.
        entity: R::Entity,
    },
}

// ============================================================================
// GapBounds
// ============================================================================

/// A cross-event temporal gap claim.
///
/// `from` is conventionally the temporally-earlier endpoint; the gap is
/// `to - from`, expressed as `[min_days, max_days]` (either side may be `None`
/// to leave it open). The smart constructor [`GapBounds::new`] enforces:
///
/// - At least one of `min_days` / `max_days` is `Some` (otherwise the gap is
///   unconstrained and the fact says nothing).
/// - When both are present, `min_days <= max_days`.
///
/// Non-negativity is structural — [`Days`] is a `u64` newtype.
///
/// `Deserialize` routes through [`GapBounds::new`] via
/// [`serde(try_from)`](https://serde.rs/container-attrs.html#try_from) over
/// `RawGapBounds`, so the cross-field invariants (at least one bound present;
/// `min <= max`) are enforced at the parse boundary rather than materializing an
/// unconstrained gap. The mirror copies the derived serialize shape, so the two
/// directions can't drift.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema, IdWalk,
)]
#[serde(bound(serialize = "R: IdScheme"))]
#[serde(bound(deserialize = "R: IdScheme"), try_from = "RawGapBounds<R>")]
#[schemars(bound = "R: IdScheme + JsonSchema")]
pub struct GapBounds<R: IdScheme> {
    /// The earlier endpoint.
    from: OrderableEvent<R>,
    /// The later endpoint.
    to: OrderableEvent<R>,
    /// Minimum gap in days; `None` leaves the lower side open.
    min_days: Option<Days>,
    /// Maximum gap in days; `None` leaves the upper side open.
    max_days: Option<Days>,
}

/// Deserialize mirror for [`GapBounds`]: the field shape the struct's derived
/// `Serialize` emits, with no invariant. `TryFrom` re-imposes the bound
/// invariants through [`GapBounds::new`]. The single deserialize entry point,
/// mirroring the serialize fields, so the two directions can't drift.
#[derive(Deserialize)]
#[serde(bound(deserialize = "R: IdScheme"))]
#[serde(deny_unknown_fields)]
struct RawGapBounds<R: IdScheme> {
    from: OrderableEvent<R>,
    to: OrderableEvent<R>,
    min_days: Option<Days>,
    max_days: Option<Days>,
}

impl<R: IdScheme> TryFrom<RawGapBounds<R>> for GapBounds<R> {
    type Error = GapBoundsError;

    fn try_from(raw: RawGapBounds<R>) -> Result<Self, Self::Error> {
        GapBounds::new(raw.from, raw.to, raw.min_days, raw.max_days)
    }
}

impl<R: IdScheme> GapBounds<R> {
    /// Construct a gap, validating the bound invariants.
    pub fn new(
        from: OrderableEvent<R>,
        to: OrderableEvent<R>,
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

    /// The earlier endpoint.
    pub fn from(&self) -> &OrderableEvent<R> {
        &self.from
    }

    /// The later endpoint.
    pub fn to(&self) -> &OrderableEvent<R> {
        &self.to
    }

    /// Minimum gap in days; `None` leaves the lower side open.
    pub fn min_days(&self) -> Option<Days> {
        self.min_days
    }

    /// Maximum gap in days; `None` leaves the upper side open.
    pub fn max_days(&self) -> Option<Days> {
        self.max_days
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
    use crate::grammar::identity::IdMapError;
    use crate::store::memory::{MemoryEntityId, MemoryIds};

    /// A test-only scheme whose three id kinds are all `String`, so a
    /// type-changing relabel can render each visited id into a distinguishable
    /// string and a swapped closure dispatch surfaces as the wrong prefix.
    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
    struct StrIds;

    impl IdScheme for StrIds {
        type Entity = String;
        type Event = String;
        type Image = String;
    }

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn anchor() -> OrderableEvent<MemoryIds> {
        OrderableEvent::ConstructionCompletion {
            entity: MemoryEntityId(1),
        }
    }

    fn other() -> OrderableEvent<MemoryIds> {
        OrderableEvent::DemolitionStart {
            entity: MemoryEntityId(2),
        }
    }

    #[test]
    fn gap_bounds_rejects_both_open() -> TestResult {
        assert_eq!(
            GapBounds::new(anchor(), other(), None, None),
            Err(GapBoundsError::Unconstrained)
        );
        Ok(())
    }

    #[test]
    fn gap_bounds_rejects_min_exceeds_max() -> TestResult {
        assert_eq!(
            GapBounds::new(anchor(), other(), Some(Days::new(10)), Some(Days::new(5))),
            Err(GapBoundsError::MinExceedsMax {
                min_days: 10,
                max_days: 5
            })
        );
        Ok(())
    }

    #[test]
    fn gap_bounds_deserialize_rejects_unconstrained() -> TestResult {
        // `Deserialize` must route through `new` (via `#[serde(try_from)]`): a
        // wire value with both bounds absent is structurally well-formed but
        // invalid, and must fail at the boundary rather than materialize an
        // unconstrained gap. A regression to a plain field-by-field derive —
        // one that dropped the `try_from` and bypassed `new` — would make this
        // parse succeed.
        let valid = GapBounds::new(anchor(), other(), Some(Days::new(1)), None)?;
        let mut wire = serde_json::to_value(&valid)?;
        wire["min_days"] = serde_json::Value::Null;
        wire["max_days"] = serde_json::Value::Null;
        let result: Result<GapBounds<MemoryIds>, _> = serde_json::from_value(wire);
        assert!(
            result.is_err(),
            "both-open gap must fail at the deserialize boundary"
        );
        Ok(())
    }

    #[test]
    fn gap_bounds_round_trips_preserving_all_fields() -> TestResult {
        // Catches a `RawGapBounds` → smart-ctor (`try_from`) wiring bug that
        // dropped or transposed any of {from, to, min_days, max_days}, and
        // pins that the serialize shape the goldens hash still round-trips.
        let gap = GapBounds::new(anchor(), other(), Some(Days::new(2)), Some(Days::new(7)))?;
        let json = serde_json::to_string(&gap)?;
        let parsed: GapBounds<MemoryIds> = serde_json::from_str(&json)?;
        assert_eq!(parsed, gap);
        Ok(())
    }

    // --- id-traversal: exercises `GapBounds::try_map_ids` / `for_each_id` ---
    //
    // Built with the bundle-local index newtypes (`EntityIdx` / `EventIdx`) as
    // the input id types — the realistic pre-substitution shape — and relabels
    // them to `String` so the type-changing relabel is visible. The two
    // endpoints route through both closures (one is an entity, the other an
    // event), so a closure-dispatch swap would fail the assertion.

    use crate::submit::{BundleLocal, EntityIdx, EventIdx, ImageIdx};

    /// A gap whose `from` endpoint carries an entity ref and whose `to`
    /// endpoint carries an event ref — so the relabel touches both id
    /// kinds through the nested recursion.
    fn idx_gap_bounds() -> Result<GapBounds<BundleLocal>, GapBoundsError> {
        GapBounds::new(
            OrderableEvent::ConstructionCompletion {
                entity: EntityIdx(7),
            },
            OrderableEvent::Event { event: EventIdx(3) },
            Some(Days::new(1)),
            Some(Days::new(5)),
        )
    }

    #[test]
    fn try_map_ids_relabels_gap_through_both_closures() -> TestResult {
        let bounds = idx_gap_bounds()?;
        // Distinguishable renderings per kind: a swapped dispatch (entity
        // closure firing on the event ref, or vice versa) would surface as the
        // wrong prefix. The closures never fail, but `try_map_ids` is generic
        // over the error type, so the error is named concretely
        // (`IdMapError<String, String, String>`) to pin inference.
        let mapped: GapBounds<StrIds> = bounds.try_map_ids(
            &mut |e: &EntityIdx| {
                Ok::<_, IdMapError<String, String, String>>(format!("entity-{}", e.0))
            },
            &mut |v: &EventIdx| Ok(format!("event-{}", v.0)),
            &mut |i: &ImageIdx| Ok(format!("image-{}", i.0)),
        )?;
        let expected = GapBounds::new(
            OrderableEvent::ConstructionCompletion {
                entity: "entity-7".to_owned(),
            },
            OrderableEvent::Event {
                event: "event-3".to_owned(),
            },
            Some(Days::new(1)),
            Some(Days::new(5)),
        )?;
        assert_eq!(mapped, expected);
        Ok(())
    }

    #[test]
    fn for_each_id_collects_gap_endpoint_ids() -> TestResult {
        let bounds = idx_gap_bounds()?;
        let mut entities: Vec<EntityIdx> = Vec::new();
        let mut events: Vec<EventIdx> = Vec::new();
        bounds.for_each_id(
            &mut |e: &EntityIdx| entities.push(*e),
            &mut |v: &EventIdx| events.push(*v),
            &mut |_i: &ImageIdx| {},
        );
        // `from` (entity) is visited before `to` (event); the entity
        // endpoint lands in `entities`, the event endpoint in `events`.
        assert_eq!(entities, vec![EntityIdx(7)]);
        assert_eq!(events, vec![EventIdx(3)]);
        Ok(())
    }

    /// The leaf closure can reject a reference; the error propagates up through
    /// the nested recursion as `LeafLookup`. This is the failure shape the
    /// substitute traversal raises for a bundle-local index out of range.
    ///
    /// The error type is named concretely (`IdMapError<String, String, String>`)
    /// because `LeafLookup` carries no typed id, so the generic `try_map_ids`
    /// can't infer the three id params from this call alone.
    #[test]
    fn try_map_ids_propagates_leaf_lookup_failure() -> TestResult {
        use crate::grammar::ids::SubjectKind;
        let bounds = idx_gap_bounds()?;
        let result: Result<GapBounds<StrIds>, IdMapError<String, String, String>> = bounds
            .try_map_ids(
                &mut |e: &EntityIdx| {
                    Err(IdMapError::LeafLookup {
                        kind: SubjectKind::Entity,
                        idx: e.0,
                        decl_count: 1,
                    })
                },
                &mut |v: &EventIdx| Ok(format!("event-{}", v.0)),
                &mut |i: &ImageIdx| Ok(format!("image-{}", i.0)),
            );
        assert!(matches!(
            result,
            Err(IdMapError::LeafLookup {
                kind: SubjectKind::Entity,
                idx: 7,
                decl_count: 1,
            })
        ));
        Ok(())
    }
}
