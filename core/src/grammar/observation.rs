//! Observation cluster — feature and spatial-relation claims.
//!
//! Cluster module for the `Observation` variant of
//! [`crate::grammar::assertions::JudgmentAssertion`]. Both variants assert claims
//! about reality (this building has a gabled roof; these two buildings are
//! adjacent). The evidential basis — image observation, text source, KB link,
//! or a researcher's personal observation — lives in the citation, not the
//! fact. See [`crate::grammar::citations::JudgmentSource::ImageObservation`] for
//! the image-grounded citation flavor.
//!
//! # Error states (rejected at submit time)
//!
//! Combinations the grammar permits structurally but the fact-store layer
//! rejects at submit time — bugs in caller code, not outside-world uncertainty.
//!
//! - **Missing depiction pairing.** Every entity referenced inside an
//!   observation fact must also have a [`crate::grammar::depiction::Fact`] tying it
//!   to the image the
//!   observation was made against. An observation describes a feature or
//!   relation seen in an image, so the entity reference is meaningless without
//!   the depiction link. The two facts typically arrive in the same commit; the
//!   submit layer rejects observations whose entity isn't paired with a
//!   depiction.
//!
//! # Conflicts (surfaced at projection time)
//!
//! Observation facts are interpretive claims; multiple sources can reach
//! different conclusions about the same feature or relation. The solver
//! surfaces these as user-resolvable conflicts rather than forcing a consensus
//! — observations of a building's condition or its topological relation to a
//! neighbor are source-dependent enough that picking a winner without human
//! review would lose signal.

use chronoscope_macros::grammar_type;

use crate::grammar::features::Feature;
use crate::grammar::ids::IdScheme;
use crate::grammar::spatial::TopologicalRel;

/// Observation-cluster fact.
///
/// Generic over one id scheme `R: IdScheme`, reading only `R::Entity`.
///
/// The `Spatial` variant carries a
/// [`crate::grammar::identity::DistinctPair<R::Entity>`] so the directional
/// `from != to` invariant is structurally enforced: `from` is the subject
/// (`a` side), `to` is the object (`b` side). Topological relations like
/// `PartOf` and `Surrounds` are directional; preserving the order is the whole
/// point of using `DistinctPair` rather than
/// [`crate::grammar::identity::OrderedDistinctPair`].
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(bound(serialize = "R: IdScheme", deserialize = "R: IdScheme"))]
#[schemars(bound = "R: IdScheme + ::schemars::JsonSchema")]
pub enum Fact<R: IdScheme> {
    /// A claim that an entity has a particular feature. The evidence —
    /// which image grounded the observation, which region of that image,
    /// who or what observer made it — lives in the citation.
    Feature { entity: R::Entity, feature: Feature },
    /// A claim that two entities stand in a topological relation. The
    /// evidence — which image grounded the observation, which text
    /// source described it, who made the call — lives in the citation.
    Spatial {
        /// The two entities, structurally distinct, in subject-object
        /// order for directional relations.
        pair: crate::grammar::identity::DistinctPair<R::Entity>,
        relation: TopologicalRel<R>,
    },
}

impl<R: IdScheme> Fact<R> {
    /// Construct a `Spatial`, rejecting `a == b`.
    pub fn spatial(
        a: R::Entity,
        b: R::Entity,
        relation: TopologicalRel<R>,
    ) -> Result<Self, crate::grammar::identity::SelfPairError<R::Entity>> {
        let pair = crate::grammar::identity::DistinctPair::new(a, b)?;
        Ok(Self::Spatial { pair, relation })
    }
}

impl<R: IdScheme> Fact<R> {
    /// Visit every entity id this fact mentions.
    ///
    /// The `Spatial` arm threads the closure into both the `from`/`to` pair
    /// (visited first) and the relation's separator / axis (visited second).
    pub fn for_each_id(&self, fe: &mut impl FnMut(&R::Entity)) {
        match self {
            Self::Feature { entity, .. } => fe(entity),
            Self::Spatial { pair, relation } => {
                pair.for_each_id(fe);
                relation.for_each_id(fe);
            }
        }
    }

    /// Relabel every entity id through the fallible closure, producing a
    /// `Fact<E2>`.
    ///
    /// The `Spatial` arm maps the pair (before the relation) and a post-map
    /// collision is handed to `on_self_loop`, which the caller supplies so
    /// the resulting error carries the right
    /// [`crate::grammar::identity::SelfLoop`] variant (the
    /// [`JudgmentAssertion`](crate::grammar::assertions::JudgmentAssertion)
    /// dispatch wraps it as [`crate::grammar::identity::SelfLoop::Spatial`]).
    ///
    /// Generic over the error type `Err`: both the leaf closure `fe` and
    /// the `on_self_loop` collapse closure produce `Err`, so the cluster
    /// never names the concrete error the assertion layer chooses.
    pub fn try_map_ids<R2: IdScheme, Err>(
        &self,
        fe: &mut impl FnMut(&R::Entity) -> Result<R2::Entity, Err>,
        on_self_loop: impl FnOnce(R2::Entity) -> Err,
    ) -> Result<Fact<R2>, Err> {
        match self {
            Self::Feature { entity, feature } => Ok(Fact::Feature {
                entity: fe(entity)?,
                feature: feature.clone(),
            }),
            Self::Spatial { pair, relation } => {
                let pair = pair.try_map_ids(fe, on_self_loop)?;
                let relation = relation.try_map_ids(fe)?;
                Ok(Fact::Spatial { pair, relation })
            }
        }
    }
}
