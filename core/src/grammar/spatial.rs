//! Topological relation vocabulary for spatial claims about entity pairs.
//!
//! Spatial relations are claims about reality — two buildings touch, one is
//! part of another, they sit on opposite sides of a street. The vocabulary
//! covers what observers reliably report: adjacency, mereological part-of,
//! surrounds, and the ternary separator-anchored relations (`AcrossFrom`,
//! `SameSide`, `LinedAlong`).
//!
//! Variants are viewpoint-independent: each is a claim about the entities, not
//! about how they look from a vantage. The supporting evidence — an image
//! observation, a text source, a KB topology link, a walk-by — lives in the
//! citation. See [`crate::grammar::observation::Fact::Spatial`] for the fact
//! shape and [`crate::grammar::citations::JudgmentSource`] for the citation
//! flavors.
//!
//! Two omissions: bare co-presence ("both visible in the same image") is
//! derivable from two [`crate::grammar::depiction::Fact`] facts sharing an image;
//! and viewpoint-dependent occlusion is deferred until a use case demands it (its
//! home would be a separate observation variant carrying the image).
//!
//! Metric reasoning (distances, coordinates) isn't here — topology comes from
//! observers, metric calibration from external knowledge in a later layer.

use chronoscope_macros::grammar_type;

/// A qualitative topological relation between two entities.
///
/// Generic over `EntId` so the ternary variants (`AcrossFrom`, `SameSide`,
/// `LinedAlong`) carry an `EntId` separator or axis. Every involved entity has
/// a minted id by the time a topological observation is recorded, so the
/// relation never carries a description-shaped reference.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(bound(
    serialize = "EntId: ::serde::Serialize",
    deserialize = "EntId: ::serde::de::DeserializeOwned"
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

impl<EntId> TopologicalRel<EntId> {
    /// Visit the separator / axis entity id this relation carries, if any.
    ///
    /// `AcrossFrom` / `SameSide` carry a `separator`, `LinedAlong` an `axis`;
    /// `Adjacent` / `PartOf` / `Surrounds` carry none. Takes only the entity
    /// closure — the relation carries no other id kind.
    pub fn for_each_id(&self, fe: &mut impl FnMut(&EntId)) {
        match self {
            Self::Adjacent | Self::PartOf | Self::Surrounds => {}
            Self::AcrossFrom { separator } | Self::SameSide { separator } => fe(separator),
            Self::LinedAlong { axis } => fe(axis),
        }
    }

    /// Relabel the separator / axis entity id (if any) through the fallible
    /// closure, producing a `TopologicalRel<E2>`.
    pub fn try_map_ids<E2, Err>(
        &self,
        fe: &mut impl FnMut(&EntId) -> Result<E2, Err>,
    ) -> Result<TopologicalRel<E2>, Err> {
        match self {
            Self::Adjacent => Ok(TopologicalRel::Adjacent),
            Self::PartOf => Ok(TopologicalRel::PartOf),
            Self::Surrounds => Ok(TopologicalRel::Surrounds),
            Self::AcrossFrom { separator } => Ok(TopologicalRel::AcrossFrom {
                separator: fe(separator)?,
            }),
            Self::SameSide { separator } => Ok(TopologicalRel::SameSide {
                separator: fe(separator)?,
            }),
            Self::LinedAlong { axis } => Ok(TopologicalRel::LinedAlong { axis: fe(axis)? }),
        }
    }
}
