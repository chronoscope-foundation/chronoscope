//! Lifetime-event vocabulary and supporting domain enums.
//!
//! The fact store separates a building's life into two regions. The
//! *bookends* — construction and demolition — hang directly off the entity
//! as flat per-entity facts; they are structurally once-only and don't
//! carry a [`LifetimeEventId`](crate::facts::ids::LifetimeEventId).
//! The *interior* of the lifetime is a sequence of events with their own
//! identity: a single significant change like a renovation, fire, or
//! adaptive-reuse moment is a [`LifetimeEventKind`] keyed by event id.
//!
//! Each date-bearing fact carries the source-claimed interval as an
//! [`crate::date::UncertainDate`]. Durational events additionally carry
//! a [`DurationalRole`] to distinguish the start of the duration from
//! its completion; point events do not.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Role for a date bound on a durational lifetime event.
///
/// Durational events ([`LifetimeEventKind::Modified`],
/// [`LifetimeEventKind::Damaged`], [`LifetimeEventKind::Repaired`],
/// [`LifetimeEventKind::Moved`]) span an interval rather than a single
/// instant; the role distinguishes the start of the duration from its
/// end. Point events ([`LifetimeEventKind::UsageChanged`],
/// [`LifetimeEventKind::Designated`]) describe a single instant and
/// don't carry a role.
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

/// Lifetime-event categories carried by interior lifecycle facts.
///
/// Construction and demolition are deliberately absent: they live as flat
/// per-entity bookend facts on
/// [`crate::facts::assertions::FactualAssertion`], not as values inside an
/// event reference. Once-ness for the bookends is structural rather than
/// rule-enforced.
///
/// Some kinds are *durational* (their facts use
/// [`crate::facts::event::Fact::DurationalDate`] with a
/// [`DurationalRole`]); others are *point* events (their facts use
/// [`crate::facts::event::Fact::PointDate`]).
/// `Damaged` and `Moved` are durational because significant damage and
/// physical relocation typically span days to months and benefit from
/// start/completion bookends. Point events describe a single instant;
/// the source-claimed interval is the [`crate::date::UncertainDate`]
/// they carry. A durational event may carry only one of its two bounds
/// (e.g. a `Damaged` with `Started` but no `Completed`) when the source
/// only supplies one endpoint.
///
/// # Solver rules
///
/// The kind serves as the dispatch tag the solver uses to validate
/// per-kind field availability on event-cluster facts. See the
/// "Solver rules" section of [`crate::facts::event`] for the full
/// per-variant matrix. The kind also bounds the [`DurationalRole`]
/// requirement on dates: durational kinds must carry a role on every
/// date fact; point kinds must not.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LifetimeEventKind {
    /// Durational — a renovation, expansion, or other physical change to
    /// an existing structure.
    Modified,
    /// Durational — damage from fire, flood, earthquake, war, neglect, or
    /// other cause. Carries a damage cause via
    /// [`crate::facts::event::Fact::DamageCause`].
    Damaged,
    /// Durational — repair work after damage.
    Repaired,
    /// Durational — physical relocation of the structure. Carries the
    /// method via [`crate::facts::event::Fact::MoveMethod`].
    Moved,
    /// Point — a change in the structure's active uses. Carries the new
    /// usage set via [`crate::facts::event::Fact::UsageChange`].
    UsageChanged,
    /// Point — designation as a landmark, historic register listing, or
    /// other named status. Carries the designation text via
    /// [`crate::facts::event::Fact::Designation`].
    Designated,
}

/// Cause of damage to a structure, carried by an [`LifetimeEventKind::Damaged`]
/// event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
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

/// Method used to relocate a structure, carried by an
/// [`LifetimeEventKind::Moved`] event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(tag = "type", rename_all = "snake_case")]
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
