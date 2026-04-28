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
use crate::location::UnresolvedLocation;

/// Core entity representing a building, infrastructure, natural feature, or area.
///
/// Generic over:
/// - `E` — entity reference type (e.g., `EntityId` for stored data, `EntityIdx` for ingestion)
/// - `S` — source reference type (e.g., `SourceId` for stored data, `SourceIdx` for ingestion)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(bound(deserialize = "E: serde::de::DeserializeOwned, S: serde::de::DeserializeOwned"))]
pub struct Entity<E, S> {
    pub names: Vec<Cited<EntityName, S>>,
    pub transitions: Vec<EntityTransition<E, S>>,
}

impl<E, S> Entity<E, S> {
    /// Pick the best display name: prefer names whose language tag starts with `lang_prefix`,
    /// fall back to first available.
    #[must_use]
    pub fn best_name(&self, lang_prefix: &str) -> Option<&str> {
        let preferred = self
            .names
            .iter()
            .find(|n| n.value.language.as_str().starts_with(lang_prefix));
        preferred
            .or_else(|| self.names.first())
            .map(|n| n.value.name.as_str())
    }
}

/// A name for an entity with temporal validity.
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EntityName {
    pub name: String,
    pub name_type: NameType,
    /// BCP 47 language tag (supports contemporary and historical languages).
    #[schemars(with = "String")]
    pub language: LanguageTag<String>,

    pub valid_from: Option<UncertainDate>,
    pub valid_to: Option<UncertainDate>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
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
///
/// Generic over:
/// - `E` — entity reference type (used by `UnresolvedLocation<E>::NearEntity`)
/// - `S` — source reference type (used by `Cited<T, S>` for evidence)
#[serde_with::skip_serializing_none]
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, strum::Display, strum::AsRefStr,
)]
#[serde(tag = "type", rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
#[serde(bound(deserialize = "E: serde::de::DeserializeOwned, S: serde::de::DeserializeOwned"))]
pub enum EntityTransition<E, S> {
    Constructed {
        started_at: Option<Cited<UncertainDate, S>>,
        completed_at: Option<Cited<UncertainDate, S>>,
        location: Option<Cited<UnresolvedLocation<E>, S>>,
        trigger_event: Option<TriggerEventId>,
    },
    Modified {
        started_at: Option<Cited<UncertainDate, S>>,
        completed_at: Option<Cited<UncertainDate, S>>,
        description: Option<String>,
        trigger_event: Option<TriggerEventId>,
    },
    Damaged {
        occurred_at: Option<Cited<UncertainDate, S>>,
        cause: Option<DamageCause>,
        description: Option<String>,
        trigger_event: Option<TriggerEventId>,
    },
    Repaired {
        started_at: Option<Cited<UncertainDate, S>>,
        completed_at: Option<Cited<UncertainDate, S>>,
        description: Option<String>,
        trigger_event: Option<TriggerEventId>,
    },
    Moved {
        occurred_at: Option<Cited<UncertainDate, S>>,
        location: Option<Cited<UnresolvedLocation<E>, S>>,
        cause: Option<String>,
        method: Option<MoveMethod>,
        trigger_event: Option<TriggerEventId>,
    },
    Demolished {
        started_at: Option<Cited<UncertainDate, S>>,
        completed_at: Option<Cited<UncertainDate, S>>,
        cause: Option<String>,
        trigger_event: Option<TriggerEventId>,
    },
    UsageModified {
        occurred_at: Option<Cited<UncertainDate, S>>,
        new_usages: std::collections::BTreeSet<Usage>,
        description: Option<String>,
        trigger_event: Option<TriggerEventId>,
    },
    Designated {
        occurred_at: Option<Cited<UncertainDate, S>>,
        designation: String,
        description: Option<String>,
        trigger_event: Option<TriggerEventId>,
    },
}

/// Start/end date pair for a transition.
pub type DateRange<'a, S> = (
    Option<&'a Cited<UncertainDate, S>>,
    Option<&'a Cited<UncertainDate, S>>,
);

impl<E, S> EntityTransition<E, S> {
    /// The earliest cited date known for this transition, across all of its
    /// date fields. Returns `None` only when no dates are set at all.
    ///
    /// Relies on the domain invariant that `started_at ≤ completed_at` for
    /// durational transitions; violations are surfaced separately by
    /// [`crate::consistency::ConsistencyWarning::CompletionBeforeStart`].
    #[must_use]
    pub fn earliest_known_date(&self) -> Option<&Cited<UncertainDate, S>> {
        let (start, end) = self.date_range();
        start.or(end)
    }

    /// The start/end date pair for transitions with duration.
    ///
    /// Returns `(started_at, completed_at)` for durational transitions (Constructed,
    /// Modified, Repaired, Demolished), or `(occurred_at, None)` for point events.
    #[must_use]
    pub fn date_range(&self) -> DateRange<'_, S> {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(feature = "sqlx", sqlx(type_name = "TEXT", rename_all = "snake_case"))]
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
/// Generic over:
/// - `E` — entity reference type
/// - `S` — source reference type (for evidence)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(bound(deserialize = "E: serde::de::DeserializeOwned, S: serde::de::DeserializeOwned"))]
pub struct EntityRelation<E, S> {
    pub from_entity: E,
    pub to_entity: E,
    pub relation_type: EntityRelationType,
    pub evidence: Vec<crate::evidence::Evidence<S>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::Cited;
    use chrono::NaiveDate;

    type T = EntityTransition<(), ()>;
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn d(year: i32) -> Result<Cited<UncertainDate, ()>, Box<dyn std::error::Error>> {
        let date = NaiveDate::from_ymd_opt(year, 1, 1).ok_or("invalid date")?;
        Ok(Cited::uncited(UncertainDate::with_precision(
            date,
            crate::date::DatePrecision::Year,
        )?))
    }

    /// Pin the four meaningful date-field combinations on a durational
    /// variant (`Constructed` is representative) plus the present/absent pair
    /// for a point variant. The lossy `event_date` predecessor returned
    /// `started_at` only and silently dropped `(None, Some)` durational
    /// cases — the regression guard is the `(None, Some)` row below.
    #[test]
    fn earliest_known_date() -> TestResult {
        let cases: Vec<(&str, T, Option<i32>)> = vec![
            (
                "Constructed: both unknown",
                T::Constructed {
                    started_at: None,
                    completed_at: None,
                    location: None,
                    trigger_event: None,
                },
                None,
            ),
            (
                "Constructed: started only",
                T::Constructed {
                    started_at: Some(d(1900)?),
                    completed_at: None,
                    location: None,
                    trigger_event: None,
                },
                Some(1900),
            ),
            (
                "Constructed: completed only — regression case",
                T::Constructed {
                    started_at: None,
                    completed_at: Some(d(1889)?),
                    location: None,
                    trigger_event: None,
                },
                Some(1889),
            ),
            (
                "Constructed: both — started wins by domain invariant",
                T::Constructed {
                    started_at: Some(d(1880)?),
                    completed_at: Some(d(1889)?),
                    location: None,
                    trigger_event: None,
                },
                Some(1880),
            ),
            (
                "UsageModified: present",
                T::UsageModified {
                    occurred_at: Some(d(1888)?),
                    new_usages: Default::default(),
                    description: None,
                    trigger_event: None,
                },
                Some(1888),
            ),
            (
                "UsageModified: absent",
                T::UsageModified {
                    occurred_at: None,
                    new_usages: Default::default(),
                    description: None,
                    trigger_event: None,
                },
                None,
            ),
        ];

        for (desc, t, expected_year) in cases {
            let got = t
                .earliest_known_date()
                .and_then(|c| c.value.earliest())
                .map(|d| d.format("%Y").to_string());
            let want = expected_year.map(|y| y.to_string());
            assert_eq!(got, want, "{desc}");
        }
        Ok(())
    }
}
