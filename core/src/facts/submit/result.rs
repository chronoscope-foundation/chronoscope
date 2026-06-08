//! Submit-time result types: resolution maps, stored-fact shapes, and the
//! lookup outcome.
//!
//! `Resolution<Id>` is parameterised by the id type; `ResolutionOrigin<Id>` is
//! parametric because its `Ambiguous` variant carries a candidate list. The
//! struct-shaped `Ambiguous` leaves room for per-candidate scoring without a
//! breaking enum change.
//!
//! `StoredFact` is the post-resolution form of the three assertion sums — the
//! submission grammar's category shape with every id resolved to a persistent
//! [`EntityId`] / [`LifetimeEventId`] / [`ImageId`].

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{EntityIdx, EventIdx, ImageIdx};
use crate::facts::assertions::{FactualAssertion, JudgmentAssertion, MetaAssertion};
use crate::facts::citations::{FactualCitation, JudgmentSource, MetaSource};
use crate::facts::ids::{CommitId, FactId, IngesterRunId, UserId};
use crate::nonempty::NonEmptyVec;

// ============================================================================
// Resolution and origin
// ============================================================================

/// A single declaration's resolution outcome — the resolved id and how it
/// was arrived at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution<Id> {
    /// The id the declaration resolved to (matched, ambiguous-fallback
    /// mint, or freshly minted).
    pub id: Id,
    /// How the id was arrived at.
    pub origin: ResolutionOrigin<Id>,
}

/// How a declaration resolved to its id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolutionOrigin<Id> {
    /// No matcher candidates — store minted a fresh id.
    NewlyMinted,
    /// The producer named the id directly (a `Decl::Existing(id)`); no
    /// matching happened. The id is in the surrounding [`Resolution`].
    DeclaredExisting,
    /// The store matched a `Decl::Local` to one existing subject from its
    /// anchors. Unlike [`Self::DeclaredExisting`], the store identified the
    /// subject. The id is in the surrounding [`Resolution`].
    MatchedExisting,
    /// More than one matcher candidate — the store minted a fresh id (see
    /// [`Resolution::id`]) and reports the existing candidates here.
    Ambiguous {
        /// The existing subjects that matched the anchors. Excludes the
        /// minted fallback id (which is in [`Resolution::id`]).
        candidates: NonEmptyVec<Id>,
    },
}

// ============================================================================
// SubmitResult
// ============================================================================

/// Result of a successful `submit_commit`.
///
/// `previously_committed`: the worker-side idempotency check reads this.
/// `true` means the bundle deduped to an existing commit; `false` means the
/// store saw it for the first time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitResult<EntId, EvtId, ImgId> {
    /// The content-addressed commit identifier.
    pub commit_id: CommitId,
    /// Whether this bundle deduped to an existing commit.
    pub previously_committed: bool,
    /// The fact ids minted (or returned, under dedup), in ascending order.
    /// Ids are assigned by walking the commit's `BTreeSet<SubmitFact>`, so the
    /// order reflects fact content, not the producer's listing. `fact_ids[i]`
    /// does not correspond to the i-th submitted fact — look a fact up by
    /// content.
    pub fact_ids: Vec<FactId>,
    /// Per-declaration entity resolution map.
    pub entities: HashMap<EntityIdx, Resolution<EntId>>,
    /// Per-declaration event resolution map.
    pub events: HashMap<EventIdx, Resolution<EvtId>>,
    /// Per-declaration image resolution map.
    pub images: HashMap<ImageIdx, Resolution<ImgId>>,
}

// ============================================================================
// CommitAuthor
// ============================================================================

/// Who recorded a commit.
///
/// Canonical hash form: `user:<UserId>` / `ingester:<IngesterRunId>`, via
/// `canonical_string`.
///
/// JSON shape is externally tagged: `{ "user": "<UserId>" }` /
/// `{ "ingester": "<IngesterRunId>" }`. (serde doesn't support
/// internally-tagged tuple variants, and `canonical_string` is the source
/// of truth for hashing regardless of the wire form.)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitAuthor {
    /// A human user.
    User(UserId),
    /// An automated ingester run.
    Ingester(IngesterRunId),
}

impl CommitAuthor {
    /// Canonical identity string used in the commit hash.
    pub fn canonical_string(&self) -> String {
        match self {
            Self::User(user) => format!("user:{user}"),
            Self::Ingester(run) => format!("ingester:{run}"),
        }
    }
}

// ============================================================================
// StoredFact — post-resolution
// ============================================================================

/// A stored factual fact: the post-resolution
/// [`FactualAssertion`](crate::facts::assertions::FactualAssertion) plus its
/// citation. Resolution substitutes every entity/event/image index with
/// the corresponding persistent id.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredFactualFact<EntId, EvtId, ImgId>
where
    EntId: Ord,
    EvtId: Ord,
    ImgId: Ord,
{
    /// The factual assertion.
    pub assertion: FactualAssertion<EntId, EvtId, ImgId>,
    /// The citation backing the claim.
    pub citation: FactualCitation,
}

/// A stored judgment fact: the post-resolution
/// [`JudgmentAssertion`](crate::facts::assertions::JudgmentAssertion) plus
/// its source.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredJudgmentFact<EntId, EvtId, ImgId>
where
    EntId: Ord,
    EvtId: Ord,
    ImgId: Ord,
{
    /// The judgment assertion.
    pub assertion: JudgmentAssertion<EntId, EvtId, ImgId>,
    /// The judgment source backing the claim.
    pub source: JudgmentSource,
}

/// A stored meta-fact: a [`MetaAssertion`] plus its source.
///
/// Not parameterised over entity/event/image id types: [`MetaAssertion`]
/// references other facts/commits by [`FactId`] / [`CommitId`], not by
/// entity/event/image ids.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredMetaFact {
    /// The meta-assertion.
    pub assertion: MetaAssertion,
    /// The meta source backing the claim.
    pub source: MetaSource,
}

/// A fact as stored after submission. Three category arms mirror the
/// three top-level assertion sums.
#[derive(Debug, Clone, PartialEq)]
pub enum StoredFact<EntId, EvtId, ImgId>
where
    EntId: Ord,
    EvtId: Ord,
    ImgId: Ord,
{
    /// A factual claim about the external world.
    Factual(StoredFactualFact<EntId, EvtId, ImgId>),
    /// An interpretive judgment.
    Judgment(StoredJudgmentFact<EntId, EvtId, ImgId>),
    /// A fact about other facts (retraction, supersession).
    Meta(StoredMetaFact),
}

// ============================================================================
// StoredCommit
// ============================================================================

/// A commit record, as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredCommit {
    /// The content-addressed commit id.
    pub commit_id: CommitId,
    /// Who recorded the commit.
    pub author: CommitAuthor,
    /// When the commit was recorded (quantised to whole seconds in the
    /// hash).
    pub recorded_at: DateTime<Utc>,
    /// The fact ids under this commit, in ascending order. Assigned by walking
    /// the commit's `BTreeSet<SubmitFact>`, so the order reflects fact content,
    /// not the producer's listing — no positional correspondence to the
    /// submitted facts.
    pub fact_ids: Vec<FactId>,
}

// ============================================================================
// FactLookup
// ============================================================================

/// Result of looking up a fact by id at a snapshot.
///
/// `Active` boxes its payload because [`StoredFact`]'s variants span a wide
/// size range (hundreds of bytes to kilobytes for a mask). Unboxed, every
/// `Future` / `Unknown` value would carry the worst-case size, and
/// `view.fact()` returns those for every not-yet-minted id.
#[derive(Debug, Clone, PartialEq)]
pub enum FactLookup<EntId, EvtId, ImgId>
where
    EntId: Ord,
    EvtId: Ord,
    ImgId: Ord,
{
    /// The fact exists and is active at this view's snapshot.
    Active(Box<StoredFact<EntId, EvtId, ImgId>>),
    /// The fact existed at-or-before snapshot but was retracted by
    /// another fact at-or-before snapshot. The retracting fact's id is
    /// reported.
    Retracted {
        /// The retracting fact's id.
        by: FactId,
    },
    /// The fact id is beyond this view's snapshot.
    Future,
    /// The fact id was never minted in this store.
    Unknown,
}
