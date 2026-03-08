//! Entity types and transitions.
//!
//! Core types for representing buildings, infrastructure, and other spatial entities
//! along with their lifecycle transitions.

use oxilangtag::LanguageTag;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::date::UncertainDate;
use crate::evidence::Cited;
use crate::ids::TriggerEventId;
use crate::location::UncertainLocation;

/// Core entity representing a building, infrastructure, natural feature, or area.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Entity {
    pub entity_type: EntityType,
    pub names: Vec<Cited<EntityName>>,
    pub transitions: Vec<EntityTransition>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EntityType {
    Area,
    Building,
    Infrastructure,
    Monument,
    NaturalFeature,
}

/// A name for an entity with temporal validity.
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EntityName {
    pub name: String,
    pub name_type: NameType,
    /// BCP 47 language tag (supports contemporary and historical languages).
    #[schemars(with = "String")]
    pub language: LanguageTag<String>,

    pub valid_from: Option<UncertainDate>,
    pub valid_to: Option<UncertainDate>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NameType {
    Official,
    Common,
    Historical,
}

/// Usage category for a building or structure.
///
/// An empty `BTreeSet<Usage>` represents closure/vacancy (no active use).
/// `Usage::Unknown` represents "in use but we don't know what for".
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Usage {
    /// In use but purpose unknown
    Unknown,
    /// Barns, silos, granaries
    Agricultural,
    /// Shops, offices, warehouses
    Commercial,
    /// Museums, theaters, galleries
    Cultural,
    /// Schools, universities, libraries
    Educational,
    /// Hospitals, clinics
    Healthcare,
    /// Factories, plants, mills
    Industrial,
    /// Utilities, water treatment, power
    Infrastructure,
    /// Government buildings, courthouses
    Institutional,
    /// Bases, fortifications
    Military,
    /// Parks, stadiums, pools
    Recreational,
    /// Churches, mosques, temples
    Religious,
    /// Apartments, houses, dormitories
    Residential,
    /// Stations, airports, ports
    Transportation,
    /// Other use with freeform description
    Other { description: String },
}

/// Cause of damage to a structure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DamageCause {
    Earthquake,
    Fire,
    Flood,
    Neglect,
    Structural,
    Vandalism,
    War,
    Weather,
    Other { description: String },
}

/// Method used to move a structure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MoveMethod {
    /// Structure was disassembled and reassembled at new location
    Disassembled,
    /// Structure was moved intact (on rails, trucks, etc.)
    Whole,
}

/// Entity transition events.
///
/// Each transition represents a significant change in the entity's lifecycle.
/// Dates and locations are wrapped in `Option<Cited<...>>` where:
/// - `None` = unknown (no citation needed)
/// - `Some(Cited { value, evidence })` = known value with optional supporting evidence
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EntityTransition {
    Constructed {
        started_at: Option<Cited<UncertainDate>>,
        completed_at: Option<Cited<UncertainDate>>,
        location: Option<Cited<UncertainLocation>>,
        trigger_event: Option<TriggerEventId>,
    },
    Modified {
        started_at: Option<Cited<UncertainDate>>,
        completed_at: Option<Cited<UncertainDate>>,
        description: Option<String>,
        trigger_event: Option<TriggerEventId>,
    },
    Damaged {
        occurred_at: Option<Cited<UncertainDate>>,
        cause: Option<DamageCause>,
        description: Option<String>,
        trigger_event: Option<TriggerEventId>,
    },
    Repaired {
        started_at: Option<Cited<UncertainDate>>,
        completed_at: Option<Cited<UncertainDate>>,
        description: Option<String>,
        trigger_event: Option<TriggerEventId>,
    },
    Moved {
        occurred_at: Option<Cited<UncertainDate>>,
        location: Option<Cited<UncertainLocation>>,
        cause: Option<String>,
        method: Option<MoveMethod>,
        trigger_event: Option<TriggerEventId>,
    },
    Demolished {
        started_at: Option<Cited<UncertainDate>>,
        completed_at: Option<Cited<UncertainDate>>,
        cause: Option<String>,
        trigger_event: Option<TriggerEventId>,
    },
    UsageModified {
        occurred_at: Option<Cited<UncertainDate>>,
        new_usages: std::collections::BTreeSet<Usage>,
        description: Option<String>,
        trigger_event: Option<TriggerEventId>,
    },
    Designated {
        occurred_at: Option<Cited<UncertainDate>>,
        designation: String,
        description: Option<String>,
        trigger_event: Option<TriggerEventId>,
    },
}

impl EntityTransition {
    /// The primary event date (`started_at` for durational transitions, `occurred_at` for point events).
    #[must_use]
    pub fn event_date(&self) -> Option<&Cited<UncertainDate>> {
        match self {
            Self::Constructed { started_at, .. }
            | Self::Modified { started_at, .. }
            | Self::Repaired { started_at, .. }
            | Self::Demolished { started_at, .. } => started_at.as_ref(),
            Self::Damaged { occurred_at, .. }
            | Self::Moved { occurred_at, .. }
            | Self::UsageModified { occurred_at, .. }
            | Self::Designated { occurred_at, .. } => occurred_at.as_ref(),
        }
    }

    /// The start/end date pair for transitions with duration.
    ///
    /// Returns `(started_at, completed_at)` for durational transitions (Constructed,
    /// Modified, Repaired, Demolished), or `(occurred_at, None)` for point events.
    #[must_use]
    pub fn date_range(&self) -> (Option<&Cited<UncertainDate>>, Option<&Cited<UncertainDate>>) {
        match self {
            Self::Constructed {
                started_at,
                completed_at,
                ..
            }
            | Self::Modified {
                started_at,
                completed_at,
                ..
            }
            | Self::Repaired {
                started_at,
                completed_at,
                ..
            }
            | Self::Demolished {
                started_at,
                completed_at,
                ..
            } => (started_at.as_ref(), completed_at.as_ref()),
            Self::Damaged { occurred_at, .. }
            | Self::Moved { occurred_at, .. }
            | Self::UsageModified { occurred_at, .. }
            | Self::Designated { occurred_at, .. } => (occurred_at.as_ref(), None),
        }
    }
}

/// Type of relationship between two entities.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EntityRelationType {
    /// This entity replaces the target (e.g., new building on same site after demolition)
    Replaces,
    /// This entity contains the target (e.g., complex contains individual buildings)
    Contains,
    /// This entity was formed by merging the target into it
    MergedFrom,
    /// This entity was split off from the target
    SplitFrom,
}

/// A relationship between two entities.
///
/// Generic over the entity reference type:
/// - For ingestion bundles: `EntityRelation<EntityIdx>` (typed indices)
/// - For test fixtures: `EntityRelation<&str>` (readable keys)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(bound(deserialize = "E: serde::de::DeserializeOwned"))]
pub struct EntityRelation<E> {
    pub from_entity: E,
    pub to_entity: E,
    pub relation_type: EntityRelationType,
    pub evidence: Vec<crate::evidence::Evidence>,
}

/// Relation using ingestion-time entity keys.
pub type IngestionRelation = EntityRelation<crate::ids::EntityIdx>;
