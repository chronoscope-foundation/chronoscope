//! The conflict report model — the content-addressed [`ConflictId`], the
//! structured [`ConflictLocation`], the [`Resolution`] menu, the
//! [`ConflictKind`] trait binding a kind's data to its action, and the
//! [`AnyConflictReport`] sum the API serves.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::grammar::ids::FactId;
use crate::nonempty::NonEmptyVec;

// ============================================================================
// ConflictId — content-derived stable identifier
// ============================================================================

/// Stable identifier for one conflict: the hex SHA-256 over the kind tag and
/// the sorted contributing fact ids.
///
/// Sorting makes the id order-free — the detector's discovery order can't
/// change it. Two consequences: the id is stable while the same facts fight
/// (a frontend URL survives re-projection), and a conflict whose contributor
/// set changes gets a new id, which is correct — its evidence changed, so it
/// is operationally a different conflict.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct ConflictId(String);

impl ConflictId {
    /// Derive a `ConflictId` from a kind tag and the contributing fact ids.
    /// Ids are sorted and deduped before hashing so the digest depends on the
    /// set, not the order or multiplicity.
    pub fn from_facts(kind_tag: &str, facts: impl IntoIterator<Item = FactId>) -> Self {
        let sorted: BTreeSet<u64> = facts.into_iter().map(FactId::get).collect();
        let mut hasher = Sha256::new();
        hasher.update(kind_tag.as_bytes());
        for id in sorted {
            hasher.update(b":");
            hasher.update(id.to_le_bytes());
        }
        Self(hex::encode(hasher.finalize()))
    }
}

// ============================================================================
// ConflictLocation — where in the projection a conflict surfaces
// ============================================================================

/// The bracket endpoint a lifecycle bookend (construction / demolition)
/// conflict anchors to.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum BookendEndpoint {
    Started,
    Completed,
}

/// The bracket endpoint an interior event's date conflict anchors to.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum EventEndpoint {
    Started,
    Completed,
    Occurred,
}

/// Where inside a projected entity a conflict surfaces. The frontend renders a
/// disputed indicator at this slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConflictPath<EvtId> {
    /// The entity's construction bookend at `endpoint`.
    Construction { endpoint: BookendEndpoint },
    /// The entity's demolition bookend at `endpoint`.
    Demolition { endpoint: BookendEndpoint },
    /// An interior event's date at `endpoint`.
    EventDate {
        event: EvtId,
        endpoint: EventEndpoint,
    },
}

/// The entity a conflict surfaces in, plus the slot within its projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConflictLocation<EntId, EvtId> {
    pub entity: EntId,
    pub path: ConflictPath<EvtId>,
}

// ============================================================================
// Resolution — curator-actionable remedies
// ============================================================================

/// A curator-actionable resolution for a conflict, generic over the per-kind
/// action `A`.
///
/// `Retract` is universal — every conflict admits removing one of its
/// contributing facts, and the detector emits one `Retract` per contributor.
/// `Custom` carries the kind-specific remedy; kinds with no extra options use
/// [`Uninhabited`], making `Custom` a variant that compiles but can't be built.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Resolution<A> {
    /// Retract one contributing fact.
    Retract { fact: FactId },
    /// A kind-specific remedy.
    Custom { action: A },
}

/// The empty action type for kinds whose only remedy is the universal
/// `Retract` — [`Resolution::Custom`] over it is uninhabited.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Uninhabited {}

// ============================================================================
// ConflictKind — ties a kind's data to its action type and id tag
// ============================================================================

/// A conflict kind: its per-kind resolution `Action` and the `KIND_TAG` that
/// namespaces its [`ConflictId`]s. The kind's structured data is the type that
/// implements this trait, so a report can't pair the wrong data with the wrong
/// action.
///
/// `Action` is bound to the read-side value traits every resolution action
/// carries (it is served over the API and compared in tests); that lets
/// [`ConflictReport`] derive its own value traits without hand-written bounds
/// on the associated type.
pub trait ConflictKind {
    type Action: std::fmt::Debug
        + Clone
        + PartialEq
        + Serialize
        + serde::de::DeserializeOwned
        + JsonSchema;
    const KIND_TAG: &'static str;
}

/// A set of date claims that meet to an empty interval — one minimal fighting
/// set, as returned by [`minimize::minimize`]. Retracting the whole set
/// resolves the contradiction; retracting a single member may not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DateConflict {
    pub contributing: NonEmptyVec<FactId>,
}

impl ConflictKind for DateConflict {
    type Action = Uninhabited;
    const KIND_TAG: &'static str = "date";
}

// ============================================================================
// ConflictReport — the typed envelope for one conflict
// ============================================================================

/// A typed report for one conflict instance: its content-derived id, the
/// kind-specific data, the location it surfaces at, and the curator's
/// resolutions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ConflictReport<K: ConflictKind, EntId, EvtId> {
    pub id: ConflictId,
    pub data: K,
    pub location: ConflictLocation<EntId, EvtId>,
    pub resolutions: Vec<Resolution<K::Action>>,
}

impl<EntId, EvtId> ConflictReport<DateConflict, EntId, EvtId> {
    /// Build a date-conflict report from its location and one minimal fighting
    /// set. Derives the [`ConflictId`] from the contributing facts and offers
    /// one [`Resolution::Retract`] per contributor.
    pub fn date_conflict(
        location: ConflictLocation<EntId, EvtId>,
        contributing: NonEmptyVec<FactId>,
    ) -> Self {
        let id = ConflictId::from_facts(DateConflict::KIND_TAG, contributing.iter().copied());
        let resolutions = contributing
            .iter()
            .copied()
            .map(|fact| Resolution::Retract { fact })
            .collect();
        Self {
            id,
            data: DateConflict { contributing },
            location,
            resolutions,
        }
    }
}

// ============================================================================
// AnyConflictReport — the heterogeneous sum the API serves
// ============================================================================

/// The closed sum over every conflict kind, one variant apiece. Not
/// `#[non_exhaustive]`: a new kind should force every consumer to adapt at
/// compile time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AnyConflictReport<EntId, EvtId> {
    Date(ConflictReport<DateConflict, EntId, EvtId>),
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn facts(ids: &[u64]) -> Result<NonEmptyVec<FactId>, Box<dyn std::error::Error>> {
        Ok(NonEmptyVec::try_from_vec(
            ids.iter().copied().map(FactId::new).collect(),
        )?)
    }

    // -- ConflictId ------------------------------------------------------

    #[test]
    fn conflict_id_is_order_free() {
        let a = ConflictId::from_facts("date", [FactId::new(3), FactId::new(1), FactId::new(2)]);
        let b = ConflictId::from_facts("date", [FactId::new(1), FactId::new(2), FactId::new(3)]);
        assert_eq!(a, b, "fact-id order must not change the id");
    }

    #[test]
    fn conflict_id_distinguishes_kind_and_fact_set() {
        let base = ConflictId::from_facts("date", [FactId::new(1), FactId::new(2)]);
        let other_kind = ConflictId::from_facts("spatial", [FactId::new(1), FactId::new(2)]);
        let other_facts = ConflictId::from_facts("date", [FactId::new(1), FactId::new(3)]);
        assert_ne!(base, other_kind, "the kind tag must change the id");
        assert_ne!(base, other_facts, "the fact set must change the id");
    }

    // -- Constructor -----------------------------------------------------

    #[test]
    fn date_conflict_derives_id_and_one_retract_per_fact() -> TestResult {
        let location = ConflictLocation {
            entity: 42u64,
            path: ConflictPath::EventDate {
                event: 7u64,
                endpoint: EventEndpoint::Occurred,
            },
        };
        let report = ConflictReport::date_conflict(location, facts(&[5, 9])?);

        assert_eq!(
            report.id,
            ConflictId::from_facts("date", [FactId::new(5), FactId::new(9)]),
            "the id derives from the contributing facts under the date tag"
        );
        assert_eq!(report.resolutions.len(), 2, "one retract per contributor");
        let retracted: Vec<FactId> = report
            .resolutions
            .iter()
            .filter_map(|r| match r {
                Resolution::Retract { fact } => Some(*fact),
                Resolution::Custom { .. } => None,
            })
            .collect();
        assert_eq!(retracted, vec![FactId::new(5), FactId::new(9)]);
        Ok(())
    }

    // -- Serde round-trip over the served sum ----------------------------

    #[test]
    fn any_conflict_report_round_trips() -> TestResult {
        let report = AnyConflictReport::Date(ConflictReport::date_conflict(
            ConflictLocation {
                entity: 1u64,
                path: ConflictPath::Construction {
                    endpoint: BookendEndpoint::Started,
                },
            },
            facts(&[3, 4])?,
        ));
        let json = serde_json::to_string(&report)?;
        let back: AnyConflictReport<u64, u64> = serde_json::from_str(&json)?;
        assert_eq!(report, back, "the served sum must round-trip through JSON");
        Ok(())
    }

    #[test]
    fn any_conflict_report_has_a_schema() {
        // Exercises the JsonSchema derive across the whole tree, including the
        // uninhabited action leaf.
        let schema = schemars::schema_for!(AnyConflictReport<u64, u64>);
        assert!(
            serde_json::to_value(&schema).is_ok(),
            "the schema must serialize"
        );
    }
}
