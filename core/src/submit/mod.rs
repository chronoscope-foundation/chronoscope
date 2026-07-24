//! Submission shape — the producer-side input to `submit_commit`.
//!
//! Producers describe a commit as declarations (newly-mentioned entities,
//! events, images) plus facts referencing those declarations through
//! bundle-local indices. The store resolves each declaration to a persistent
//! id at submit time and rewrites the index references before storing.
//!
//! This layer and [`store`](crate::store) are co-recursive peers, not a clean
//! stack: `submit`'s data types ([`Commit`], [`StoredFact`], [`SubmitResult`],
//! [`FactLookup`], [`SubmitError`]) supply the store trait's associated types,
//! while `submit`'s own algorithms ([`pipeline`], [`matcher`]) consume that
//! trait — the write path and the store's shape are defined together.
//!
//! Key shapes:
//!
//! - [`Commit`] — the top-level bundle.
//! - [`Decl<Id>`] — generic over the id type. [`Decl::Local`] mints a fresh
//!   id (a matcher match is asserted as an identity judgment, not adopted);
//!   [`Decl::Existing`] adopts a known id.
//! - [`EntityIdx`] / [`EventIdx`] / [`ImageIdx`] — bundle-local index
//!   newtypes that prevent intermixing.
//! - [`SubmitFact`] — one variant per assertion category, pairing the
//!   assertion with its matching citation type so a category mismatch is
//!   unrepresentable.
//!
//! The submission grammar reuses every cluster `Fact` enum: the generic
//! parameters on `FactualAssertion` / `JudgmentAssertion` are instantiated
//! with the index newtypes instead of persistent ids.

pub(crate) mod driver;
pub mod error;
pub mod matcher;
pub mod pipeline;
pub mod result;

// Selective re-exports so common types reach via `crate::submit::X`.
pub use crate::grammar::ids::SubjectKind;
pub use error::{DateRole, SubmitError};
pub use pipeline::commit_facts;
pub use result::{
    CommitAuthor, FactLookup, LocatedSubject, Resolution, ResolutionOrigin, StoredCommit,
    StoredFact, SubmitResult,
};

use std::collections::BTreeSet;

use chrono::{DateTime, SubsecRound, Utc};
use chronoscope_macros::grammar_type;
use schemars::JsonSchema;
use serde::de::Error as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::grammar::assertions::{FactualAssertion, JudgmentAssertion, MetaAssertion};
use crate::grammar::citations::{FactualCitation, JudgmentSource, MetaSource};
use crate::grammar::ids::{CommitId, IdScheme};

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

/// The submission / index scheme: a fact's references are bundle-local indices
/// into a [`Commit`]'s declaration lists, resolved to persistent ids at submit
/// time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, JsonSchema)]
pub struct BundleLocal;

impl IdScheme for BundleLocal {
    type Entity = EntityIdx;
    type Event = EventIdx;
    type Image = ImageIdx;
}

// ============================================================================
// Decl
// ============================================================================

/// A declaration in a submission bundle.
///
/// Generic over the id type so one enum covers entity, event, and image
/// declarations. `Existing` adopts a previously-minted id — producer
/// knowledge, a hard identity claim. `Local` always mints fresh; when the
/// store's matcher recognises the subject, it asserts the identity as a
/// machine-authored judgment in a companion commit instead of adopting, so
/// one retraction undoes a bad match.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decl<Id> {
    /// Adopt a previously-minted id.
    Existing {
        /// The persistent id to adopt.
        id: Id,
    },
    /// Mint a fresh id; any matcher match is asserted as an identity
    /// judgment rather than adopted.
    Local,
}

// ============================================================================
// Submission-side assertion aliases
// ============================================================================

/// A submission-side factual assertion. The same
/// [`FactualAssertion`] shape
/// as storage, over the [`BundleLocal`] scheme (the bundle-local index
/// newtypes).
pub type SubmitFactualAssertion = FactualAssertion<BundleLocal>;

/// A submission-side judgment assertion.
pub type SubmitJudgmentAssertion = JudgmentAssertion<BundleLocal>;

// Commit is parameterised over a backend's id scheme so it pairs with any
// backend. The Decl side carries that scheme's persistent ids; the SubmitFact
// side carries bundle-local indices ([`BundleLocal`]).

// ============================================================================
// SubmitFact — sum-of-pairs
// ============================================================================

/// A submission-side fact: assertion paired with its matching citation.
///
/// The (assertion, citation) pair is a struct so the category tag binds both
/// halves — a `Factual` variant holds a [`FactualAssertion`] and a
/// [`FactualCitation`], and a `JudgmentSource` in the `Factual` arm is
/// unrepresentable. The submission-side analogue of
/// [`StoredFact`].
///
/// Through [`grammar_type`]: the `type` tag
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
        /// The judgment source backing the claim. Its observed image (for an
        /// `ImageObservation`) is a bundle-local [`ImageIdx`], rewritten to a
        /// persistent id at submission alongside the assertion.
        citation: JudgmentSource<ImageIdx>,
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

impl SubmitFact {
    /// Visit every bundle-local index this fact references, dispatching each to
    /// its kind's closure. The fact-level traversal: it folds the assertion's
    /// own indices together with a judgment's observed image, so callers
    /// resolving or counting references see one complete stream per fact.
    ///
    /// A `Judgment`'s `ImageObservation` citation contributes its observed
    /// image through `fi`. `Meta` references facts and commits by persistent
    /// id, not bundle-local indices, so it visits nothing.
    pub fn for_each_id(
        &self,
        fe: &mut impl FnMut(&EntityIdx),
        fv: &mut impl FnMut(&EventIdx),
        fi: &mut impl FnMut(&ImageIdx),
    ) {
        match self {
            Self::Factual { assertion, .. } => assertion.for_each_id(fe, fv, fi),
            Self::Judgment {
                assertion,
                citation,
            } => {
                assertion.for_each_id(fe, fv, fi);
                if let Some(idx) = citation.observed_image() {
                    fi(idx);
                }
            }
            Self::Meta { .. } => {}
        }
    }
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
/// Parameterised over the id scheme `R` so any
/// [`FactStore`](crate::store::FactStore) backend pairs it with its own
/// id types.
#[derive(Debug, Clone, PartialEq)]
pub struct Commit<R: IdScheme> {
    /// Who recorded the commit.
    pub author: CommitAuthor,
    /// When the commit was recorded.
    pub recorded_at: DateTime<Utc>,
    /// Entity declarations. [`EntityIdx`]`(i)` inside a fact indexes here.
    pub entities: Vec<Decl<R::Entity>>,
    /// Event declarations. [`EventIdx`]`(i)` inside a fact indexes here.
    pub events: Vec<Decl<R::Event>>,
    /// Image declarations. [`ImageIdx`]`(i)` inside a fact indexes here.
    pub images: Vec<Decl<R::Image>>,
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

impl<R: IdScheme> Commit<R> {
    /// Derive the content-addressed [`CommitId`] for this commit.
    ///
    /// JCS-encodes the canonical projection (author canonical-string, the
    /// three declaration lists in declared order, the sorted
    /// `BTreeSet<SubmitFact>`, the whole-second `recorded_at`), SHA-256s the
    /// bytes, and routes the hex digest through [`CommitId::parse`].
    ///
    /// The declaration lists are hashed because they carry the
    /// mint-vs-adopt-existing identity decision — see `CommitHashView`.
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
        let canonical = self.canonical_jcs()?;

        let mut hasher = Sha256::new();
        hasher.update(canonical.as_bytes());
        CommitId::parse(hex::encode(hasher.finalize()))
            .map_err(|e| serde_json::Error::custom(e.to_string()))
    }

    /// The JCS bytes the [`CommitId`] hashes — the canonical hash-input
    /// projection ([`CommitHashView`]).
    pub(crate) fn canonical_jcs(&self) -> Result<String, serde_json::Error> {
        let view = CommitHashView {
            author: self.author.canonical_string(),
            entities: &self.entities,
            events: &self.events,
            facts: &self.facts,
            images: &self.images,
            recorded_at: self.recorded_at.trunc_subsecs(0).to_rfc3339(),
        };
        serde_jcs::to_string(&view)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use url::Url;

    use crate::grammar::assertions::FactualAssertion;
    use crate::grammar::attribute::{self, NameText, NameType};
    use crate::grammar::citations::{Excerpt, ExternalSource, FactualCitation, Language};
    use crate::grammar::ids::{IngesterRunId, UserId};
    use crate::store::memory::{MemoryEntityId, MemoryIds};

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
        let language = Language::new("en")?;
        Ok(SubmitFact::Factual {
            assertion: FactualAssertion::Attribute {
                fact: attribute::Fact::Name {
                    entity: EntityIdx(0),
                    name: NameText::new(name)?,
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
    ) -> Commit<MemoryIds> {
        Commit {
            author,
            recorded_at,
            entities: vec![Decl::Local],
            events: Vec::new(),
            images: Vec::new(),
            facts: facts.into_iter().collect(),
        }
    }

    /// A judgment fact whose `ImageObservation` citation names an image the
    /// assertion never mentions has that observed image visited by the
    /// fact-level `for_each_id` — the citation id is folded into the traversal,
    /// not just the assertion's own ids.
    #[test]
    fn for_each_id_visits_observed_image_of_judgment_citation() -> TestResult {
        use crate::grammar::assertions::JudgmentAssertion;
        use crate::grammar::citations::{JudgmentSource, Observer};
        use crate::grammar::observation;

        let observed = ImageIdx(7);
        let fact = SubmitFact::Judgment {
            assertion: JudgmentAssertion::Observation {
                fact: observation::Fact::Feature {
                    entity: EntityIdx(0),
                    feature: crate::grammar::features::Feature::StoryCount { stories: 2 },
                },
            },
            citation: JudgmentSource::ImageObservation {
                image: observed,
                region: None,
                observer: Observer::User {
                    user: UserId::new("alice"),
                    justification: None,
                },
            },
        };

        let mut entities = Vec::new();
        let mut images = Vec::new();
        fact.for_each_id(
            &mut |e: &EntityIdx| entities.push(*e),
            &mut |_v: &EventIdx| {},
            &mut |i: &ImageIdx| images.push(*i),
        );
        assert_eq!(entities, vec![EntityIdx(0)]);
        assert_eq!(images, vec![observed]);
        Ok(())
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
        let local: Commit<MemoryIds> = Commit {
            author: alice_author(),
            recorded_at: fixed_time()?,
            entities: vec![Decl::Local],
            events: Vec::new(),
            images: Vec::new(),
            facts: facts.clone(),
        };
        let existing: Commit<MemoryIds> = Commit {
            author: alice_author(),
            recorded_at: fixed_time()?,
            entities: vec![Decl::Existing {
                id: MemoryEntityId(5),
            }],
            events: Vec::new(),
            images: Vec::new(),
            facts,
        };
        assert_ne!(local.id()?, existing.id()?);
        Ok(())
    }
}
