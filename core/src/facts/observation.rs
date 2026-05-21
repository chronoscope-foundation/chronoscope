//! Observation cluster — feature and spatial-relation claims.
//!
//! Cluster module for the `Observation` variant of
//! [`crate::facts::assertions::JudgmentAssertion`]. Both variants assert
//! claims about reality (this building has a gabled roof; these two
//! buildings are adjacent). The evidential basis — whether the claim
//! came from an image observation, a text source, a KB link, or a
//! researcher's personal observation — lives in the citation, not in
//! the fact. See
//! [`crate::facts::citations::JudgmentSource::ImageObservation`] for the
//! image-grounded citation flavor.
//!
//! # Error states (rejected at submit time)
//!
//! Error states are combinations the grammar permits structurally but
//! the fact-store layer rejects at submit time. They model bugs in
//! caller code, not outside-world uncertainty.
//!
//! - **Missing depiction pairing.** Every entity referenced inside an
//!   observation fact must also have a corresponding
//!   [`crate::facts::depiction::Fact::InPicture`] or
//!   [`crate::facts::depiction::Fact::OnMap`] fact tying that entity to
//!   the image the observation was made against. An observation
//!   describes a feature or relation observed in an image, so the
//!   entity reference is meaningless without the depiction link that
//!   says "this entity was seen in image X." The two facts typically
//!   arrive in the same commit; the submit layer rejects observations
//!   whose entity isn't paired with a depiction.
//!
//! # Conflicts (surfaced at projection time)
//!
//! Observation facts are interpretive claims; multiple sources can
//! reach different conclusions about the same feature or relation. The
//! solver surfaces these as user-resolvable conflicts rather than
//! merging them into a forced consensus — observations of a building's
//! condition or its topological relation to a neighbor are
//! source-dependent enough that picking a winner without human review
//! would lose signal.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::facts::features::Feature;
use crate::facts::spatial::TopologicalRel;

/// Observation-cluster fact.
///
/// Generic only over the entity reference type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[serde(bound(
    serialize = "EntId: Serialize",
    deserialize = "EntId: serde::de::DeserializeOwned"
))]
#[schemars(bound = "EntId: JsonSchema")]
pub enum Fact<EntId> {
    /// A claim that an entity has a particular feature. The evidence —
    /// which image grounded the observation, which region of that image,
    /// who or what observer made it — lives in the citation.
    Feature {
        /// The entity the claim is about.
        entity: EntId,
        /// The observed feature.
        feature: Feature,
    },
    /// A claim that two entities stand in a topological relation. The
    /// evidence — which image grounded the observation, which text
    /// source described it, who made the call — lives in the citation.
    Spatial {
        /// One side of the relation. For directional variants
        /// (`PartOf`, `Surrounds`), the subject.
        a: EntId,
        /// The other side. For directional variants, the object.
        b: EntId,
        /// The observed topological relation.
        relation: TopologicalRel<EntId>,
    },
}
