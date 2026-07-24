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
use crate::grammar::ids::{IdScheme, reject_nul};

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
    /// Wrap name text, rejecting a NUL then normalizing to NFC. Fallible only on
    /// a NUL (`U+0000`); every NUL-free string has an NFC form. The
    /// NFC-normalize keeps `new` lenient about casing/composition — only
    /// [`NameText::deserialize`] is strict about non-NFC input.
    pub fn new(text: impl AsRef<str>) -> Result<Self, NameTextError> {
        let raw = text.as_ref();
        if reject_nul(raw).is_err() {
            return Err(NameTextError::ContainsNul);
        }
        Ok(Self {
            inner: raw.nfc().collect(),
        })
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
        if reject_nul(&s).is_err() {
            return Err(serde::de::Error::custom(NameTextError::ContainsNul));
        }
        if !unicode_normalization::is_nfc(&s) {
            return Err(serde::de::Error::custom(NameTextError::NotNfc {
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

/// Errors from [`NameText`] construction and deserialization.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NameTextError {
    /// Wire input parsed but was not NFC. The wire form feeds the commit hash,
    /// so the boundary rejects it. Only [`NameText::deserialize`] raises this;
    /// [`NameText::new`] normalizes non-NFC input instead.
    #[error("name text {input:?} is not NFC (canonical form is {canonical:?})")]
    NotNfc {
        /// The non-canonical input as received.
        input: String,
        /// Its NFC form.
        canonical: String,
    },
    /// The text held a NUL (`U+0000`), which Postgres jsonb cannot store.
    #[error("name text contains a NUL character (U+0000)")]
    ContainsNul,
}

/// Attribute-cluster fact.
///
/// The `Relationship` variant carries a
/// [`crate::grammar::identity::DistinctPair<R::Entity>`] so the directional
/// `from != to` invariant is structurally enforced; see the cluster docs for
/// the rule that surfaces this rejection in [`crate::submit::SubmitError`].
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Fact<R: IdScheme> {
    /// A name applied to the entity, with temporal validity bounds.
    Name {
        entity: R::Entity,
        /// The name as the source used it, NFC-normalized.
        name: NameText,
        /// BCP-47 language tag, in canonical form.
        language: Language,
        name_type: NameType,
        /// When this name first applied, when known. The single bound
        /// expresses imprecision.
        #[date_role = "NameValidFrom"]
        valid_from: Option<UncertainDate>,
        /// When this name stopped applying, when known.
        #[date_role = "NameValidTo"]
        valid_to: Option<UncertainDate>,
    },
    /// The entity has an identifier in an external reference system
    /// (Wikidata QID, OSM element id, Wikipedia title). The identifier
    /// itself lives inline in the [`ExternalReference`] variant.
    ExternalReference {
        entity: R::Entity,
        /// The typed external reference (system + identifier).
        reference: ExternalReference,
    },
    /// A directed relationship between two entities. The from/to pair is
    /// carried in a [`crate::grammar::identity::DistinctPair`] so
    /// self-loops fail at construction; see the cluster docs.
    Relationship {
        /// Source and target entity, structurally distinct.
        #[self_loop = "Relationship"]
        pair: crate::grammar::identity::DistinctPair<R::Entity>,
        relation: EntityRelationType,
    },
}

impl<R: IdScheme> Fact<R> {
    /// Construct a `Relationship`, rejecting `from == to`.
    pub fn relationship(
        from: R::Entity,
        to: R::Entity,
        relation: EntityRelationType,
    ) -> Result<Self, crate::grammar::identity::SelfPairError<R::Entity>> {
        let pair = crate::grammar::identity::DistinctPair::new(from, to)?;
        Ok(Self::Relationship { pair, relation })
    }
}

impl<R: IdScheme> Fact<R> {
    /// The entity this fact is a claim about. A relationship's subject is the
    /// directed pair's source (`from`); the projected relation slot keys off the
    /// target, but the fact belongs to the source entity.
    pub fn subject(&self) -> &R::Entity {
        match self {
            Self::Name { entity, .. } | Self::ExternalReference { entity, .. } => entity,
            Self::Relationship { pair, .. } => pair.from(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_text_new_rejects_nul() {
        assert_eq!(NameText::new("a\u{0}b"), Err(NameTextError::ContainsNul));
    }

    #[test]
    fn name_text_new_normalizes_nul_free_input_to_nfc() -> Result<(), NameTextError> {
        // Decomposed "e" + combining acute normalizes to precomposed "é".
        let name = NameText::new("e\u{0301}")?;
        assert_eq!(name.as_str(), "\u{e9}");
        Ok(())
    }

    #[test]
    fn name_text_deserialize_rejects_nul() {
        // A NUL escaped in the wire string must be refused, even though it is NFC.
        let result: Result<NameText, _> = serde_json::from_str("\"a\\u0000b\"");
        assert!(result.is_err(), "a NUL in wire input must be rejected");
    }
}
