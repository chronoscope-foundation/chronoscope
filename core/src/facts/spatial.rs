//! Topological relation vocabulary for spatial claims about entity pairs.
//!
//! Spatial relations are claims about reality — that two buildings touch,
//! that one is mereologically part of another, that they sit on opposite
//! sides of a street. The vocabulary covers the topological structure
//! observers reliably report: adjacency, mereological part-of, surrounds,
//! and the ternary separator-anchored relations (`AcrossFrom`,
//! `SameSide`, `LinedAlong`) that anchor a pair to a shared third party.
//!
//! Variants are viewpoint-independent: each relation makes a claim about
//! the entities in reality, not about how they look from a particular
//! vantage. The same claim can be supported by an image observation, a
//! text source, a KB topology link, or a researcher's personal walk-by
//! — the evidence lives in the citation, not the fact. See
//! [`crate::facts::observation::Fact::Spatial`] for the fact shape and
//! [`crate::facts::citations::JudgmentSource`] for the citation flavors.
//!
//! Two notable omissions: bare co-presence ("both visible in the same
//! image, no further structure observed") is derivable from two
//! [`crate::facts::depiction::Fact::InImage`] facts sharing an image;
//! viewpoint-dependent occlusion claims aren't in this vocabulary
//! either, deferred until a use case demands them — when they arrive,
//! their natural home is a separate observation-cluster variant whose
//! image is part of the claim's meaning.
//!
//! Metric reasoning (distance bounds, coordinates) is not in this
//! vocabulary. Topology comes from observers; metric calibration comes
//! from external knowledge in a later layer.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A qualitative topological relation between two entities.
///
/// Generic over the entity reference type `EntId` so the ternary variants
/// (`AcrossFrom`, `SameSide`, `LinedAlong`) carry an `EntId` for their
/// separator or axis. By the time a topological observation is recorded,
/// every involved entity already has a minted id (either previously
/// known or freshly minted at submission for newly-mentioned entities);
/// the relation never carries a description-shaped reference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[serde(bound(
    serialize = "EntId: Serialize",
    deserialize = "EntId: serde::de::DeserializeOwned"
))]
pub enum TopologicalRel<EntId> {
    /// The two entities share a boundary, or sit boundary-to-boundary,
    /// in real space — "next to each other".
    Adjacent,
    /// The two entities are on opposite sides of `separator` (a street,
    /// a wall, a river).
    AcrossFrom {
        /// The shared third party that divides them.
        separator: EntId,
    },
    /// The first entity (`a` on the wrapper) is a component of the
    /// second (`b` on the wrapper) — mereological, not set-theoretic.
    /// The spire of a church is `PartOf` the church.
    PartOf,
    /// Both entities are on the same side of `separator`.
    SameSide {
        /// The shared third party they're both on one side of.
        separator: EntId,
    },
    /// The two entities are arranged along the named linear feature
    /// `axis` (a street, a riverfront, a wall).
    LinedAlong {
        /// The linear feature they're aligned along.
        axis: EntId,
    },
    /// The first entity (`a` on the wrapper) borders the second
    /// (`b` on the wrapper) on multiple sides — containment-flavored,
    /// without making a strict mereological claim.
    Surrounds,
}
