//! Attribute cluster — entity-level attribute facts.
//!
//! Cluster module for the `Attribute` variant of
//! [`crate::facts::assertions::FactualAssertion`]. Holds entity-level attribute
//! claims that don't fit the bookend or interior-lifetime shapes: names (with
//! temporal validity), external-system identifiers, and directed inter-entity
//! relationships.
//!
//! # Error states (rejected at submit time)
//!
//! Combinations the grammar permits structurally but the fact-store layer
//! rejects at submit time — bugs in caller code, not outside-world uncertainty.
//!
//! - **Self-referential relationship.** On [`Fact::Relationship`], `from` and
//!   `to` must refer to distinct entities. A self-pointed relation isn't a
//!   contradiction the user can resolve, it's meaningless data.
//! - **Inverted name window.** On [`Fact::Name`], when both `valid_from` and
//!   `valid_to` are set, `valid_from`'s earliest possible date must not exceed
//!   `valid_to`'s latest possible date. A window that closes before it opens is
//!   malformed, not an uncertainty conflict.
//!
//! # Conflicts (surfaced at projection time)
//!
//! Claims the wire grammar admits and the submit layer accepts; the projection
//! / solver layers surface them as user-resolvable contradictions.
//!
//! ## Name lifetime-window violation
//!
//! - A name's `[valid_from, valid_to]` window must fall within the entity's
//!   lifetime window (`Construction::Started` to `Demolition::Completed`). A
//!   name validity range straddling the lifetime is an outside-world
//!   disagreement to surface, not a submit-time bug.
//!
//! ## Relationship semantics
//!
//! - `Contains`: anti-symmetric (if `A Contains B` then not `B Contains A`) and
//!   transitive (`A Contains B`, `B Contains C` implies `A Contains C`; the
//!   solver enforces the closure when implemented). Sources that disagree (e.g.
//!   by asserting both directions) produce a conflict.
//! - `Replaces`: directed and acyclic across an entity's lifetime. `A Replaces
//!   B` typically pairs with `Demolition::Completed(B)` before
//!   `Construction::Started(A)` at a matching location, but the temporal
//!   relationship is checked against bookends, not enforced by the relation
//!   itself (see the [`EntityRelationType::Replaces`] doc-comment). Cycles are
//!   conflicts the solver surfaces, not submit-layer rejections.
//! - `MergedFrom` / `SplitFrom`: each pair models a single real-world event,
//!   recorded as facts on the surviving entity. The inverse direction is not
//!   separately asserted; an inverse assertion surfaces as a `MergedFrom` /
//!   `SplitFrom` conflict.
//!
//! ## External references
//!
//! - On [`Fact::ExternalReference`], the (system, identifier) pair must be
//!   unique across entities — the same Wikidata QID resolving to two different
//!   `EntId`s in the projection is an identity-merge conflict, not a
//!   duplicate-fact tolerance.

use chronoscope_macros::grammar_type;
use oxilangtag::LanguageTag;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::date::UncertainDate;
use crate::facts::citations::ExternalReference;

/// Attribute-cluster fact.
///
/// The `Relationship` variant carries a [`crate::facts::identity::DistinctPair<EntId>`]
/// so the directional `from != to` invariant is structurally enforced; see the
/// cluster docs for the rule that surfaces this rejection in
/// [`crate::facts::submit::SubmitError`].
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(bound(
    serialize = "EntId: ::serde::Serialize + Ord",
    deserialize = "EntId: ::serde::Deserialize<'de> + Ord + std::fmt::Debug"
))]
#[schemars(bound = "EntId: ::schemars::JsonSchema + Ord")]
pub enum Fact<EntId: Ord> {
    /// A name applied to the entity, with temporal validity bounds.
    Name {
        entity: EntId,
        /// The name as the source used it.
        name: String,
        /// BCP-47 language tag.
        #[schemars(with = "String")]
        language: LanguageTag<String>,
        name_type: NameType,
        /// When this name first applied, when known. The single bound
        /// expresses imprecision.
        valid_from: Option<UncertainDate>,
        /// When this name stopped applying, when known.
        valid_to: Option<UncertainDate>,
    },
    /// The entity has an identifier in an external reference system
    /// (Wikidata QID, OSM element id, Wikipedia title). The identifier
    /// itself lives inline in the [`ExternalReference`] variant.
    ExternalReference {
        entity: EntId,
        /// The typed external reference (system + identifier).
        reference: ExternalReference,
    },
    /// A directed relationship between two entities. The from/to pair is
    /// carried in a [`crate::facts::identity::DistinctPair`] so
    /// self-loops fail at construction; see the cluster docs.
    Relationship {
        /// Source and target entity, structurally distinct.
        pair: crate::facts::identity::DistinctPair<EntId>,
        relation: EntityRelationType,
    },
}

impl<EntId: Ord> Fact<EntId> {
    /// Construct a `Relationship`, rejecting `from == to`.
    pub fn relationship(
        from: EntId,
        to: EntId,
        relation: EntityRelationType,
    ) -> Result<Self, crate::facts::identity::SelfPairError<EntId>> {
        let pair = crate::facts::identity::DistinctPair::new(from, to)?;
        Ok(Self::Relationship { pair, relation })
    }
}

impl<EntId: Ord> Fact<EntId> {
    /// Visit every entity id this fact mentions.
    pub fn for_each_id(&self, fe: &mut impl FnMut(&EntId)) {
        match self {
            Self::Name { entity, .. } | Self::ExternalReference { entity, .. } => fe(entity),
            Self::Relationship { pair, .. } => pair.for_each_id(fe),
        }
    }

    /// Relabel every entity id through the fallible closure, producing a
    /// `Fact<E2>`.
    ///
    /// The `Relationship` arm rebuilds its `from`/`to` pair through the same
    /// `DistinctPair::new` the wire boundary uses; a post-map collision (two
    /// distinct input ids mapping to one output) goes to `on_self_loop`, which
    /// the caller supplies so the error carries the right
    /// [`crate::facts::identity::SelfLoop`] variant (the
    /// [`FactualAssertion`](crate::facts::assertions::FactualAssertion) dispatch
    /// wraps it as [`crate::facts::identity::SelfLoop::Relationship`]).
    ///
    /// Generic over the error type `Err`: both the leaf closure `fe` and the
    /// `on_self_loop` collapse closure produce `Err`, so the cluster never names
    /// the concrete error the assertion layer chooses.
    pub fn try_map_ids<E2: Ord, Err>(
        &self,
        fe: &mut impl FnMut(&EntId) -> Result<E2, Err>,
        on_self_loop: impl FnOnce(E2) -> Err,
    ) -> Result<Fact<E2>, Err> {
        match self {
            Self::Name {
                entity,
                name,
                language,
                name_type,
                valid_from,
                valid_to,
            } => Ok(Fact::Name {
                entity: fe(entity)?,
                name: name.clone(),
                language: language.clone(),
                name_type: *name_type,
                valid_from: valid_from.clone(),
                valid_to: valid_to.clone(),
            }),
            Self::ExternalReference { entity, reference } => Ok(Fact::ExternalReference {
                entity: fe(entity)?,
                reference: reference.clone(),
            }),
            Self::Relationship { pair, relation } => {
                let pair = pair.try_map_ids(fe, on_self_loop)?;
                Ok(Fact::Relationship {
                    pair,
                    relation: *relation,
                })
            }
        }
    }
}

/// What kind of name a [`Fact::Name`] fact records.
///
/// Sources sometimes distinguish official names (those a government,
/// owner, or canonical registry assigned) from common names (those in
/// general use) from historical names (those used in the past but no
/// longer in active use). [`NameType::Unknown`] applies when the source
/// didn't distinguish; the projector treats it as "common-flavored
/// unless evidence emerges otherwise."
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum NameType {
    /// Official designation (charter, registry, owner).
    Official,
    /// Commonly used name in general circulation.
    Common,
    /// Historical name no longer in active use.
    Historical,
    /// Source didn't distinguish; the projector treats this as
    /// common-flavored.
    Unknown,
}

/// Kind of relationship between two entities, carried by
/// [`Fact::Relationship`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum EntityRelationType {
    /// This entity is the intended continuation of the target — a
    /// rebuilt-or-successor claim, not just "the next thing built there." A
    /// church rebuilt as a new church on the same site is `Replaces`; an
    /// unrelated house built on the cleared lot is not.
    ///
    /// The continuity claim is what distinguishes `Replaces` from a
    /// derivation made out of `Demolition::Completed(from)` plus
    /// `Construction::Started(to)` at matching locations: those facts
    /// say "Y came after X at the same place," `Replaces` adds the
    /// "and Y is meant to be the same thing as X" judgment.
    Replaces,
    /// This entity contains the target (e.g. a complex containing
    /// individual buildings).
    Contains,
    /// This entity was formed by merging the target into it.
    MergedFrom,
    /// This entity was split off from the target.
    SplitFrom,
}
