//! Attribute cluster — entity-level attribute facts.
//!
//! Cluster module for the `Attribute` variant of
//! [`crate::grammar::assertions::FactualAssertion`]. Holds entity-level attribute
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
//! # External references
//!
//! [`Fact::ExternalReference`] is a lookup pointer several entities may share:
//! an external system's granularity can be coarser than ours — one Wikidata
//! item spans an entity's demolish→rebuild splits — so a by-reference walk is a
//! search over the matching set, and submit imposes no uniqueness rule. The
//! matcher reads multiple hits on one reference as an ambiguous match (mint
//! fresh, surface the candidates), leaving identity to an explicit judgment.

use chronoscope_macros::grammar_type;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::date::UncertainDate;
use crate::grammar::citations::{ExternalReference, Language};

// ============================================================================
// NameText — NFC-canonical name text
// ============================================================================

/// The text of a [`Fact::Name`], stored in Unicode NFC.
///
/// Equality on names is byte equality, so the precomposed and decomposed
/// spellings of one name (`é` vs `e` + combining acute) would otherwise be
/// distinct values. The constructor NFC-normalizes — case and content are
/// untouched — so commits are built and hashed from the canonical form. Wire
/// input must already be NFC: a stored value re-serializes to exactly the
/// bytes its commit was hashed over, so [`NameText::deserialize`] rejects
/// non-NFC input.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct NameText {
    inner: String,
}

impl NameText {
    /// Wrap name text, normalizing it to NFC. Infallible — every string has
    /// an NFC form.
    pub fn new(text: impl AsRef<str>) -> Self {
        Self {
            inner: text.as_ref().nfc().collect(),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.inner
    }
}

impl<'de> Deserialize<'de> for NameText {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        if !unicode_normalization::is_nfc(&s) {
            return Err(serde::de::Error::custom(NameTextError {
                input: s.clone(),
                canonical: s.nfc().collect(),
            }));
        }
        Ok(Self { inner: s })
    }
}

impl std::fmt::Display for NameText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.inner, f)
    }
}

impl AsRef<str> for NameText {
    fn as_ref(&self) -> &str {
        &self.inner
    }
}

/// Error from [`NameText`] deserialization: the wire input was not NFC. The
/// wire form feeds the commit hash, so the boundary rejects it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameTextError {
    pub input: String,
    pub canonical: String,
}

impl std::fmt::Display for NameTextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "name text {:?} is not NFC (canonical form is {:?})",
            self.input, self.canonical
        )
    }
}

impl std::error::Error for NameTextError {}

/// Attribute-cluster fact.
///
/// The `Relationship` variant carries a [`crate::grammar::identity::DistinctPair<EntId>`]
/// so the directional `from != to` invariant is structurally enforced; see the
/// cluster docs for the rule that surfaces this rejection in
/// [`crate::submit::SubmitError`].
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
        /// The name as the source used it, NFC-normalized.
        name: NameText,
        /// BCP-47 language tag, in canonical form.
        language: Language,
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
    /// carried in a [`crate::grammar::identity::DistinctPair`] so
    /// self-loops fail at construction; see the cluster docs.
    Relationship {
        /// Source and target entity, structurally distinct.
        pair: crate::grammar::identity::DistinctPair<EntId>,
        relation: EntityRelationType,
    },
}

impl<EntId: Ord> Fact<EntId> {
    /// Construct a `Relationship`, rejecting `from == to`.
    pub fn relationship(
        from: EntId,
        to: EntId,
        relation: EntityRelationType,
    ) -> Result<Self, crate::grammar::identity::SelfPairError<EntId>> {
        let pair = crate::grammar::identity::DistinctPair::new(from, to)?;
        Ok(Self::Relationship { pair, relation })
    }
}

impl<EntId: Ord> Fact<EntId> {
    /// The entity this fact is a claim about. A relationship's subject is the
    /// directed pair's source (`from`); the projected relation slot keys off the
    /// target, but the fact belongs to the source entity.
    pub fn subject(&self) -> &EntId {
        match self {
            Self::Name { entity, .. } | Self::ExternalReference { entity, .. } => entity,
            Self::Relationship { pair, .. } => pair.from(),
        }
    }

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
    /// [`crate::grammar::identity::SelfLoop`] variant (the
    /// [`FactualAssertion`](crate::grammar::assertions::FactualAssertion) dispatch
    /// wraps it as [`crate::grammar::identity::SelfLoop::Relationship`]).
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
