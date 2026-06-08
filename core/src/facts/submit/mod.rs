//! Submission shape — the producer-side input to `submit_commit`.
//!
//! Producers describe a commit as declarations (newly-mentioned entities,
//! events, images) plus facts referencing those declarations through
//! bundle-local indices. The store resolves each declaration to a persistent
//! id at submit time and rewrites the index references before storing.
//!
//! Key shapes:
//!
//! - [`Commit`] — the top-level bundle.
//! - [`Decl<Id>`] — generic over the id type. [`Decl::Local`] resolves via
//!   match-or-mint; [`Decl::Existing`] adopts a known id.
//! - [`EntityIdx`] / [`EventIdx`] / [`ImageIdx`] — bundle-local index
//!   newtypes that prevent intermixing.
//! - [`SubmitFact`] — one variant per assertion category, pairing the
//!   assertion with its matching citation type so a category mismatch is
//!   unrepresentable.
//!
//! The submission grammar reuses every cluster `Fact` enum: the generic
//! parameters on `FactualAssertion` / `JudgmentAssertion` are instantiated
//! with the index newtypes instead of persistent ids.

pub mod error;
pub mod pipeline;
pub mod result;

// Selective re-exports so common types reach via `crate::facts::submit::X`.
pub use crate::facts::ids::SubjectKind;
pub use error::SubmitError;
pub use pipeline::commit_facts;
pub use result::{
    CommitAuthor, FactLookup, Resolution, ResolutionOrigin, StoredCommit, StoredFact, SubmitResult,
};

use std::collections::BTreeSet;

use chrono::{DateTime, SubsecRound, Utc};
use chronoscope_macros::grammar_type;
use schemars::JsonSchema;
use serde::de::Error as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::facts::assertions::{FactualAssertion, JudgmentAssertion, MetaAssertion};
use crate::facts::citations::{FactualCitation, JudgmentSource, MetaSource};
use crate::facts::ids::CommitId;

// ============================================================================
// Bundle-local index newtypes
// ============================================================================

/// Bundle-local index into a [`Commit::entities`] declaration list.
///
/// A newtype so a cross-kind mixup (an `EventIdx` where an `EntityIdx` was
/// wanted) is a compile error.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct EntityIdx(pub usize);

/// Bundle-local index into a [`Commit::events`] declaration list.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct EventIdx(pub usize);

/// Bundle-local index into a [`Commit::images`] declaration list.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct ImageIdx(pub usize);

// ============================================================================
// Decl
// ============================================================================

/// A declaration in a submission bundle.
///
/// Generic over the id type so one enum covers entity, event, and image
/// declarations. `Existing` adopts a previously-minted id; `Local` asks the
/// store to match-or-mint.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq)]
#[serde(bound(
    serialize = "Id: ::serde::Serialize",
    deserialize = "Id: ::serde::de::DeserializeOwned"
))]
#[schemars(bound = "Id: ::schemars::JsonSchema")]
pub enum Decl<Id> {
    /// Adopt a previously-minted id.
    Existing {
        /// The persistent id to adopt.
        id: Id,
    },
    /// Resolve via match-or-mint: adopt a matched existing id, or mint a
    /// fresh one when there's no unambiguous match.
    Local,
}

// ============================================================================
// Submission-side assertion aliases
// ============================================================================

/// A submission-side factual assertion. The same
/// [`FactualAssertion`](crate::facts::assertions::FactualAssertion) shape
/// as storage, with the three id parameters bound to the bundle-local
/// index newtypes.
pub type SubmitFactualAssertion = FactualAssertion<EntityIdx, EventIdx, ImageIdx>;

/// A submission-side judgment assertion.
pub type SubmitJudgmentAssertion = JudgmentAssertion<EntityIdx, EventIdx, ImageIdx>;

// SubmitFact and Commit are parameterised over the three persistent id
// types so they pair with any backend's id types. The Decl side carries
// persistent ids; the SubmitFact side carries bundle-local indices.

// ============================================================================
// SubmitFact — sum-of-pairs
// ============================================================================

/// A submission-side fact: assertion paired with its matching citation.
///
/// The (assertion, citation) pair is a struct so the category tag binds both
/// halves — a `Factual` variant holds a [`FactualAssertion`] and a
/// [`FactualCitation`], and a `JudgmentSource` in the `Factual` arm is
/// unrepresentable. The submission-side analogue of
/// [`StoredFact`](result::StoredFact).
///
/// Through [`grammar_type`](chronoscope_macros::grammar_type): the `type` tag
/// discriminates and the inner shape is the payload; [`Commit::id`] feeds
/// these to `serde_jcs::to_string` directly.
///
/// `Eq`/`Ord`/`Hash` are derived so [`Commit::facts`] can be a
/// `BTreeSet<SubmitFact>` (indices and cluster facts are all `Ord`).
/// Duplicate facts in one bundle collapse to one set entry.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SubmitFact {
    /// A claim about the external world, backed by an external citation.
    Factual {
        /// The factual assertion (with bundle-local indices).
        assertion: SubmitFactualAssertion,
        /// The citation backing the claim.
        citation: FactualCitation,
    },
    /// An interpretive judgment, backed by a judgment source.
    Judgment {
        /// The judgment assertion (with bundle-local indices).
        assertion: SubmitJudgmentAssertion,
        /// The judgment source backing the claim.
        citation: JudgmentSource,
    },
    /// A fact about other facts (retraction, supersession), backed by a
    /// meta source.
    Meta {
        /// The meta-assertion.
        assertion: MetaAssertion,
        /// The meta source backing the claim.
        citation: MetaSource,
    },
}

// ============================================================================
// Commit
// ============================================================================

/// A producer-form commit: declarations plus facts referencing them via
/// bundle-local indices.
///
/// `facts` is a `BTreeSet<SubmitFact>` so identical facts dedup before the
/// commit is hashed, and its `SubmitFact: Ord` order makes the [`CommitId`]
/// independent of submission order.
///
/// Parameterised over the three persistent id types so any
/// [`FactStore`](crate::facts::store::FactStore) backend pairs it with its own
/// id types.
#[derive(Debug, Clone, PartialEq)]
pub struct Commit<EntId, EvtId, ImgId> {
    /// Who recorded the commit.
    pub author: CommitAuthor,
    /// When the commit was recorded.
    pub recorded_at: DateTime<Utc>,
    /// Entity declarations. [`EntityIdx(i)`] inside a fact indexes here.
    pub entities: Vec<Decl<EntId>>,
    /// Event declarations. [`EventIdx(i)`] inside a fact indexes here.
    pub events: Vec<Decl<EvtId>>,
    /// Image declarations. [`ImageIdx(i)`] inside a fact indexes here.
    pub images: Vec<Decl<ImgId>>,
    pub facts: BTreeSet<SubmitFact>,
}

/// Canonical hash-input projection of a [`Commit`].
///
/// The serialized fields feed the [`CommitId`]: the canonical author string,
/// the whole-second-quantized `recorded_at`, the sorted fact set, and the
/// three declaration lists in declared order.
///
/// The declaration lists are in the address because each index binds a
/// mint-vs-adopt-existing identity decision: whether an index mints
/// ([`Decl::Local`]) or adopts a named id ([`Decl::Existing`]) changes what
/// the commit asserts, so two commits with identical facts but different decls
/// must content-address apart. The lists serialize in declared order, not
/// sorted, because fact indices are positional into that order — reordering
/// would rebind every index pointing in.
///
/// Private so [`Commit::id`] controls the canonical form; callers can't rename
/// or reorder fields without recomputing every `CommitId`.
#[derive(Serialize)]
struct CommitHashView<'a, EntId, EvtId, ImgId> {
    author: String,
    entities: &'a [Decl<EntId>],
    events: &'a [Decl<EvtId>],
    facts: &'a BTreeSet<SubmitFact>,
    images: &'a [Decl<ImgId>],
    recorded_at: String,
}

impl<EntId, EvtId, ImgId> Commit<EntId, EvtId, ImgId>
where
    EntId: Serialize,
    EvtId: Serialize,
    ImgId: Serialize,
{
    /// Derive the content-addressed [`CommitId`] for this commit.
    ///
    /// JCS-encodes the canonical projection (author canonical-string, the
    /// three declaration lists in declared order, the sorted
    /// `BTreeSet<SubmitFact>`, the whole-second `recorded_at`), SHA-256s the
    /// bytes, and routes the hex digest through [`CommitId::parse`].
    ///
    /// The declaration lists are hashed because they carry the
    /// mint-vs-adopt-existing identity decision — see [`CommitHashView`].
    /// `recorded_at` is quantized to whole seconds so sub-second jitter doesn't
    /// defeat content addressing. The author is the
    /// [`CommitAuthor::canonical_string`] form. The id-type `Serialize` bounds
    /// are there because a [`Decl::Existing`] id is part of the hashed decision.
    ///
    /// Returns a `serde_json::Error` if JCS encoding fails (only on non-finite
    /// `f64`, which the grammar boundary already rejects) or if the hex digest
    /// fails [`CommitId::parse`] (can't happen for a 64-char SHA-256 hex, but
    /// surfaced as a typed error rather than a panic).
    pub fn id(&self) -> Result<CommitId, serde_json::Error> {
        let view = CommitHashView {
            author: self.author.canonical_string(),
            entities: &self.entities,
            events: &self.events,
            facts: &self.facts,
            images: &self.images,
            recorded_at: self.recorded_at.trunc_subsecs(0).to_rfc3339(),
        };
        let canonical = serde_jcs::to_string(&view)?;

        let mut hasher = Sha256::new();
        hasher.update(canonical.as_bytes());
        CommitId::parse(hex::encode(hasher.finalize()))
            .map_err(|e| serde_json::Error::custom(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use oxilangtag::LanguageTag;
    use url::Url;

    use crate::facts::assertions::FactualAssertion;
    use crate::facts::attribute::{self, NameType};
    use crate::facts::citations::{Excerpt, ExternalSource, FactualCitation};
    use crate::facts::ids::{IngesterRunId, UserId};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn fixed_time() -> Result<DateTime<Utc>, &'static str> {
        Utc.with_ymd_and_hms(2024, 1, 1, 12, 0, 0)
            .single()
            .ok_or("fixed timestamp is unambiguous")
    }

    fn sample_citation() -> Result<FactualCitation, Box<dyn std::error::Error>> {
        let url = Url::parse("https://example.com/source")?;
        let source = ExternalSource::Url {
            url,
            published: None,
        };
        let excerpts = vec![Excerpt::new("source-text")?];
        Ok(FactualCitation::new(source, excerpts)?)
    }

    /// A `Name` fact on `EntityIdx(0)` — a stable `SubmitFact` for the hash
    /// determinism / order-insensitivity tests.
    fn named_fact(name: &str) -> Result<SubmitFact, Box<dyn std::error::Error>> {
        let language = LanguageTag::parse("en".to_owned())?;
        Ok(SubmitFact::Factual {
            assertion: FactualAssertion::Attribute {
                fact: attribute::Fact::Name {
                    entity: EntityIdx(0),
                    name: name.to_owned(),
                    language,
                    name_type: NameType::Common,
                    valid_from: None,
                    valid_to: None,
                },
            },
            citation: sample_citation()?,
        })
    }

    fn alice_author() -> CommitAuthor {
        CommitAuthor::User(UserId::new("alice"))
    }

    fn bob_author() -> CommitAuthor {
        CommitAuthor::User(UserId::new("bob"))
    }

    fn bundle_with_facts(
        author: CommitAuthor,
        recorded_at: DateTime<Utc>,
        facts: Vec<SubmitFact>,
    ) -> Commit<
        crate::facts::ids::EntityId,
        crate::facts::ids::LifetimeEventId,
        crate::facts::ids::ImageId,
    > {
        Commit {
            author,
            recorded_at,
            entities: vec![Decl::Local],
            events: Vec::new(),
            images: Vec::new(),
            facts: facts.into_iter().collect(),
        }
    }

    #[test]
    fn commit_id_is_deterministic_for_same_components() -> TestResult {
        let facts = vec![named_fact("a")?, named_fact("b")?];
        let lhs = bundle_with_facts(alice_author(), fixed_time()?, facts.clone()).id()?;
        let rhs = bundle_with_facts(alice_author(), fixed_time()?, facts).id()?;
        assert_eq!(lhs, rhs);
        Ok(())
    }

    #[test]
    fn commit_id_is_insensitive_to_fact_order() -> TestResult {
        let lhs = bundle_with_facts(
            alice_author(),
            fixed_time()?,
            vec![named_fact("a")?, named_fact("b")?],
        )
        .id()?;
        let rhs = bundle_with_facts(
            alice_author(),
            fixed_time()?,
            vec![named_fact("b")?, named_fact("a")?],
        )
        .id()?;
        assert_eq!(lhs, rhs);
        Ok(())
    }

    #[test]
    fn commit_id_quantizes_subseconds() -> TestResult {
        let base = fixed_time()?;
        let with_ns = base + chrono::Duration::nanoseconds(123_456_789);
        let lhs = bundle_with_facts(alice_author(), base, vec![named_fact("a")?]).id()?;
        let rhs = bundle_with_facts(alice_author(), with_ns, vec![named_fact("a")?]).id()?;
        assert_eq!(lhs, rhs);
        Ok(())
    }

    #[test]
    fn commit_id_differs_for_different_authors() -> TestResult {
        let lhs = bundle_with_facts(alice_author(), fixed_time()?, vec![named_fact("a")?]).id()?;
        let rhs = bundle_with_facts(bob_author(), fixed_time()?, vec![named_fact("a")?]).id()?;
        assert_ne!(lhs, rhs);
        Ok(())
    }

    /// `User` and `Ingester` of the same string must hash differently — the
    /// canonical form prefixes the kind (`user:` / `ingester:`).
    #[test]
    fn commit_id_differs_across_author_kinds_with_same_inner_string() -> TestResult {
        let user = CommitAuthor::User(UserId::new("shared"));
        let ingester = CommitAuthor::Ingester(IngesterRunId::new("shared"));
        let lhs = bundle_with_facts(user, fixed_time()?, vec![named_fact("a")?]).id()?;
        let rhs = bundle_with_facts(ingester, fixed_time()?, vec![named_fact("a")?]).id()?;
        assert_ne!(lhs, rhs);
        Ok(())
    }

    #[test]
    fn commit_id_differs_for_different_recorded_at_seconds() -> TestResult {
        let lhs = bundle_with_facts(alice_author(), fixed_time()?, vec![named_fact("a")?]).id()?;
        let rhs = bundle_with_facts(
            alice_author(),
            fixed_time()? + chrono::Duration::seconds(1),
            vec![named_fact("a")?],
        )
        .id()?;
        assert_ne!(lhs, rhs);
        Ok(())
    }

    /// Two commits identical but for their decls — `[Decl::Local]` (mint)
    /// versus `[Decl::Existing(id)]` (adopt) — must content-address apart. Pins
    /// that the decl lists feed the hash.
    #[test]
    fn commit_id_differs_for_local_vs_existing_decl() -> TestResult {
        let facts: BTreeSet<SubmitFact> = std::iter::once(named_fact("a")?).collect();
        let local: Commit<
            crate::facts::ids::EntityId,
            crate::facts::ids::LifetimeEventId,
            crate::facts::ids::ImageId,
        > = Commit {
            author: alice_author(),
            recorded_at: fixed_time()?,
            entities: vec![Decl::Local],
            events: Vec::new(),
            images: Vec::new(),
            facts: facts.clone(),
        };
        let existing: Commit<
            crate::facts::ids::EntityId,
            crate::facts::ids::LifetimeEventId,
            crate::facts::ids::ImageId,
        > = Commit {
            author: alice_author(),
            recorded_at: fixed_time()?,
            entities: vec![Decl::Existing {
                id: crate::facts::ids::EntityId::new("E5"),
            }],
            events: Vec::new(),
            images: Vec::new(),
            facts,
        };
        assert_ne!(local.id()?, existing.id()?);
        Ok(())
    }
}
