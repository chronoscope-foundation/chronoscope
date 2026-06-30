//! Lifetime-event vocabulary and supporting domain enums.
//!
//! The fact store separates a building's life into two regions. The
//! *bookends* — construction and demolition — hang directly off the entity
//! as flat per-entity facts; they are structurally once-only and don't
//! carry a lifetime-event id.
//! The *interior* of the lifetime is a sequence of events with their own
//! identity: a single significant change like a renovation, fire, or
//! adaptive-reuse moment is a [`LifetimeEventKind`] keyed by event id.
//!
//! Each date-bearing fact carries the source-claimed interval as an
//! [`crate::date::UncertainDate`]. Durational events additionally carry
//! a [`DurationalRole`] to distinguish the start of the duration from
//! its completion; point events do not.

use chronoscope_macros::grammar_type;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use strum::IntoEnumIterator;

/// Role for a date bound on a durational lifetime event.
///
/// Durational events ([`DurationalKind`]) span an interval rather than a
/// single instant; the role distinguishes the start of the duration from its
/// end. Point events ([`PointKind`]) describe a single instant and don't carry
/// a role.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DurationalRole {
    /// Bound applies to the start of the event's duration.
    Started,
    /// Bound applies to the completion of the event's duration.
    Completed,
}

/// A durational lifetime event — one that spans an interval. Its facts use
/// [`crate::facts::event::Fact::DurationalDate`] with a [`DurationalRole`].
/// `Damaged` and `Moved` are durational because significant damage and physical
/// relocation span days to months and benefit from start/completion bookends. A
/// durational event may carry only one of its two bounds (e.g. a `Damaged` with
/// `Started` but no `Completed`) when the source supplies one endpoint.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
    strum::EnumIter,
    strum::Display,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum DurationalKind {
    /// A renovation, expansion, or other physical change to an existing
    /// structure.
    Modified,
    /// Damage from fire, flood, earthquake, war, neglect, or other cause.
    /// Carries a damage cause via [`crate::facts::event::Fact::DamageCause`].
    Damaged,
    /// Repair work after damage.
    Repaired,
    /// Physical relocation of the structure. Carries the method via
    /// [`crate::facts::event::Fact::MoveMethod`].
    Moved,
}

/// A point lifetime event — one that describes a single instant. Its facts use
/// [`crate::facts::event::Fact::PointDate`] and carry no [`DurationalRole`].
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
    strum::EnumIter,
    strum::Display,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum PointKind {
    /// A change in the structure's active uses. Carries the new usage set via
    /// [`crate::facts::event::Fact::UsageChange`].
    UsageChanged,
    /// Designation as a landmark, historic register listing, or other named
    /// status. Carries the designation text via
    /// [`crate::facts::event::Fact::Designation`].
    Designated,
}

/// The declared kind of an interior lifetime event, split along its category
/// boundary: a [`DurationalKind`] spanning an interval, or a [`PointKind`] at a
/// single instant.
///
/// Construction and demolition are absent: they live as flat per-entity bookend
/// facts on [`crate::facts::assertions::FactualAssertion`], not as values inside
/// an event reference. Once-ness for the bookends is structural rather than
/// rule-enforced.
///
/// An event declares its kind through a
/// [`crate::facts::event::Fact::HasEvent`] claim. The submit layer requires each
/// minted event to carry one; sources that disagree on the kind of a merged
/// event store several, which the projection collapses to `None`.
#[grammar_type]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LifetimeEventKind {
    /// A durational event spanning an interval.
    Durational {
        /// The specific durational kind.
        kind: DurationalKind,
    },
    /// A point event at a single instant.
    Point {
        /// The specific point kind.
        kind: PointKind,
    },
}

impl LifetimeEventKind {
    /// Every durational kind, wrapped. Built from [`DurationalKind`]'s variants so
    /// a new variant flows in automatically.
    pub fn durational_kinds() -> impl Iterator<Item = Self> {
        DurationalKind::iter().map(|kind| Self::Durational { kind })
    }

    /// Every point kind, wrapped.
    pub fn point_kinds() -> impl Iterator<Item = Self> {
        PointKind::iter().map(|kind| Self::Point { kind })
    }
}

impl std::fmt::Display for LifetimeEventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Durational { kind } => write!(f, "{kind}"),
            Self::Point { kind } => write!(f, "{kind}"),
        }
    }
}

/// Cause of damage to a structure, carried by a
/// [`DurationalKind::Damaged`] event.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DamageCause {
    /// Seismic damage.
    Earthquake,
    /// Fire damage.
    Fire,
    /// Flood or water damage.
    Flood,
    /// Damage from sustained lack of maintenance.
    Neglect,
    /// Structural failure (collapse, foundation issues).
    Structural,
    /// Deliberate damage by people.
    Vandalism,
    /// War or armed conflict.
    War,
    /// Weather event other than flood (wind, storm, ice).
    Weather,
    /// Cause not covered by the named variants, with a freeform
    /// description.
    Other {
        /// Human-readable description of the cause.
        description: String,
    },
}

/// Method used to relocate a structure, carried by a
/// [`DurationalKind::Moved`] event.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum MoveMethod {
    /// Disassembled at the original site and reassembled at the new one.
    Disassembled,
    /// Moved intact (on rails, rollers, trucks, or barges) without being
    /// taken apart.
    Whole,
}

/// Usage category for a building or structure.
///
/// A structure's active uses are recorded as a `BTreeSet<Usage>` so a
/// single building can simultaneously hold (for example) commercial and
/// residential uses. An empty set denotes vacancy or closure;
/// [`Usage::Unknown`] denotes "in active use but the purpose is not
/// documented".
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Usage {
    /// In active use; the specific purpose is not documented.
    Unknown,
    /// Barns, silos, granaries.
    Agricultural,
    /// Shops, offices, warehouses.
    Commercial,
    /// Museums, theaters, galleries.
    Cultural,
    /// Schools, universities, libraries.
    Educational,
    /// Hospitals, clinics.
    Healthcare,
    /// Factories, plants, mills.
    Industrial,
    /// Utilities, water treatment, power generation.
    Infrastructure,
    /// Government buildings, courthouses.
    Institutional,
    /// Military bases, fortifications.
    Military,
    /// Parks, stadiums, pools.
    Recreational,
    /// Churches, mosques, temples.
    Religious,
    /// Apartments, houses, dormitories.
    Residential,
    /// Stations, airports, ports.
    Transportation,
    /// Use not covered by the named variants, with a freeform description.
    Other {
        /// Human-readable description of the use.
        description: String,
    },
}
