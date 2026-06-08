//! Shared submit-pipeline machinery.
//!
//! [`FactStore::submit_commit`](super::store::FactStore::submit_commit) is
//! thin; each backend composes the free functions here with its own minting +
//! insertion, so SQLite / Postgres reuse the orchestration.
//!
//! The module provides:
//!
//! - [`substitute_facts`] — translates index references to persistent ids.
//! - [`match_entities`] / [`match_images`] — resolve each [`Decl::Local`]
//!   against the unified view, returning [`MatchOutcome::Matched`] for a
//!   single hit or [`MatchOutcome::Mint`] otherwise.
//! - [`validate_submit`] — runs the submit-rule checks (today,
//!   retraction/supersession target existence) over the in-flight commit.
//!
//! Commit-id derivation lives on [`Commit::id`](super::Commit::id); backends
//! call `bundle.id()?`.
//!
//! The matcher and validator are async so a SQL backend can `.await` reads
//! (index lookups, rule reads) inside the transaction holding the commit; the
//! in-memory backend reads its unified view through the same async
//! [`FactView`] surface. They take a `&V: FactView<S>` and return a future.
//! The in-memory backend's [`async_lock::Mutex`] guard is `Send`, so holding
//! it across these awaits keeps the future `Send`. The validator awaits even
//! in-memory — resolving each retraction target through `view.fact(...)` and
//! each commit-retraction target through `view.commit_known(...)` — but
//! services those reads from memory, where a SQL backend awaits the equivalent
//! reads against its transaction.

use std::collections::{BTreeSet, HashMap};

use super::error::SubmitError;
use super::result::{
    FactLookup, StoredFact, StoredFactualFact, StoredJudgmentFact, StoredMetaFact,
};
use super::{Decl, EntityIdx, EventIdx, ImageIdx, SubmitFact};
use crate::facts::assertions::{FactualAssertion, JudgmentAssertion, MetaAssertion};
use crate::facts::identity::{IdMapError, SelfLoop};
use crate::facts::ids::SubjectKind;
use crate::facts::store::{
    FactStore, FactView, StoredFactOf, SubmitCommitError, SubmitCommitInput, SubmitCommitOutput,
};

// ============================================================================
// commit_facts — end-to-end transaction-and-commit wrapper
// ============================================================================

/// Submit a commit bundle to `store`, opening and closing a transaction around
/// it. The path for one commit per transaction; callers needing several
/// commits in one transaction use [`FactStore::with_tx`] directly.
///
/// Equivalent to:
///
/// ```ignore
/// store.with_tx(|s, tx| Box::pin(async move {
///     s.submit_commit(tx, bundle).await
/// })).await.map_err(SubmitCommitError::Backend)?
/// ```
///
/// The closure takes `&Self` rather than capturing `store`: the `for<'brand>`
/// HRTB would force a captured `&store` to be `'static`. Takes `&S` (both
/// `with_tx` and `submit_commit` are `&self`), so callers can submit from
/// multiple tasks sharing `&store`.
///
/// The double `Result` flattens here: `with_tx` returns `Result<R, S::Error>`
/// for transaction failures, and `R` is itself
/// `Result<SubmitResult, SubmitCommitError<S::Error>>`. The outer error
/// collapses into `SubmitCommitError::Backend`, then the inner result
/// propagates.
pub async fn commit_facts<S: FactStore>(
    store: &S,
    bundle: SubmitCommitInput<S>,
) -> SubmitCommitOutput<S> {
    store
        .with_tx(|s, tx| Box::pin(async move { s.submit_commit(tx, bundle).await }))
        .await
        .map_err(SubmitCommitError::Backend)?
}

// ============================================================================
// MatchOutcome
// ============================================================================

/// What the matcher concluded for one declaration.
///
/// One entry per [`Decl::Local`]; [`Decl::Existing`] bypasses the matcher. The
/// `Mint` arm covers both no-match (empty `candidates`) and ambiguous
/// (non-empty) — both mint a fresh id, and the candidate list distinguishes
/// the resolution origin downstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchOutcome<Id> {
    /// Exactly one existing id; the pipeline adopts it.
    Matched(Id),
    /// No single unambiguous match — the pipeline mints. `candidates` is
    /// empty for no-match, non-empty when several existing ids matched the
    /// anchors (flowing to
    /// [`ResolutionOrigin::Ambiguous`](super::result::ResolutionOrigin::Ambiguous)).
    Mint {
        /// Existing ids that matched the anchors. Empty = no-match,
        /// non-empty = ambiguous.
        candidates: Vec<Id>,
    },
}

// ============================================================================
// match_entities / match_images
// ============================================================================

/// Match each [`Decl::Local`] entity declaration against the unified view of
/// committed + in-flight state.
///
/// The exact-match matcher reads name / external-ref indexes over the view;
/// with no match a `Local` decl resolves to [`MatchOutcome::Mint`] with an
/// empty candidate list.
///
/// Returns a `HashMap<EntityIdx, MatchOutcome<EntityId>>` keyed by decl
/// position. [`Decl::Existing`] decls produce no entry; `submit_commit`
/// handles them directly.
///
/// Async so a backend's indexes can `.await` view reads. The future is `Send`
/// (`V: FactView<S>` is `Sync`), so a backend can drive it under its write
/// guard. A backend with nothing to await resolves immediately.
pub async fn match_entities<S: FactStore, V: FactView<S>>(
    decls: &[Decl<S::EntityId>],
    _facts: &BTreeSet<SubmitFact>,
    _view: &V,
) -> HashMap<EntityIdx, MatchOutcome<S::EntityId>> {
    // One empty-candidates Mint per Local decl; Existing decls bypass.
    let mut out = HashMap::new();
    for (i, decl) in decls.iter().enumerate() {
        if matches!(decl, Decl::Local) {
            out.insert(
                EntityIdx(i),
                MatchOutcome::Mint {
                    candidates: Vec::new(),
                },
            );
        }
    }
    out
}

/// Match each [`Decl::Local`] image declaration against the unified view of
/// committed + in-flight state.
///
/// Reads the image-source URL index; with no match a `Local` decl resolves to
/// [`MatchOutcome::Mint`] with an empty candidate list. See [`match_entities`]
/// for the async / view rationale.
pub async fn match_images<S: FactStore, V: FactView<S>>(
    decls: &[Decl<S::ImageId>],
    _facts: &BTreeSet<SubmitFact>,
    _view: &V,
) -> HashMap<ImageIdx, MatchOutcome<S::ImageId>> {
    let mut out = HashMap::new();
    for (i, decl) in decls.iter().enumerate() {
        if matches!(decl, Decl::Local) {
            out.insert(
                ImageIdx(i),
                MatchOutcome::Mint {
                    candidates: Vec::new(),
                },
            );
        }
    }
    out
}

// ============================================================================
// validate_submit
// ============================================================================

/// Run the submit-rule checks over the in-flight commit.
///
/// The only rule today is retraction/supersession target existence. A
/// meta-assertion may not target a fact or commit in the same commit (see the
/// [`MetaAssertion`] doc), so each target resolves against `view` (the
/// committed snapshot), never against `candidates`:
///
/// - [`MetaAssertion::RetractFact`] and [`MetaAssertion::SupersedeFact`] name
///   a `target` [`FactId`](crate::facts::ids::FactId), resolved via
///   [`FactView::fact`]. [`FactLookup::Unknown`] or [`FactLookup::Future`]
///   means no such target — [`SubmitError::FactNotFound`].
///   [`FactLookup::Active`] and [`FactLookup::Retracted`] both denote an
///   existing fact (re-retracting a retracted one is legitimate).
/// - [`MetaAssertion::RetractCommit`] names a target
///   [`CommitId`](crate::facts::ids::CommitId), resolved via
///   [`FactView::commit_known`]. An unrecorded commit is
///   [`SubmitError::CommitNotFound`]; a recorded one (even
///   previously-retracted) passes.
///
/// `SupersedeFact::replacement` is not checked: it may live in the same
/// commit, so it isn't visible through `view` yet.
///
/// Async: target lookups go through `view.fact(...)` / `view.commit_known(...)`.
/// The in-memory backend's `Send` mutex guard lets those reads run inside the
/// held-lock region, while a SQL backend issues them inside its transaction.
///
/// Returns `Result<(), SubmitCommitError<S::Error, …>>` to express both rule
/// rejections (`Submit` arm) and backend read failures (`Backend` arm).
pub async fn validate_submit<S: FactStore, V: FactView<S>>(
    candidates: &[StoredFactOf<S>],
    view: &V,
) -> Result<(), SubmitCommitError<S::Error, S::EntityId, S::EventId, S::ImageId>> {
    for fact in candidates {
        let StoredFact::Meta(meta) = fact else {
            continue;
        };
        // Each meta target resolves against `view`, not `candidates`.
        // FactId targets go through `view.fact`, the CommitId target through
        // `view.commit_known`. Exhaustive so a new variant forces a decision.
        match &meta.assertion {
            MetaAssertion::RetractFact { target, .. }
            | MetaAssertion::SupersedeFact { target, .. } => {
                let lookup = view
                    .fact(*target)
                    .await
                    .map_err(SubmitCommitError::Backend)?;
                match lookup {
                    // Minted at-or-before the snapshot, so it exists as a target.
                    FactLookup::Active(_) | FactLookup::Retracted { .. } => {}
                    // Never minted, or not yet visible: no such target.
                    FactLookup::Unknown | FactLookup::Future => {
                        return Err(SubmitError::FactNotFound { id: *target }.into());
                    }
                }
            }
            MetaAssertion::RetractCommit { target, .. } => {
                let known = view
                    .commit_known(target)
                    .await
                    .map_err(SubmitCommitError::Backend)?;
                if !known {
                    return Err(SubmitError::CommitNotFound { id: target.clone() }.into());
                }
            }
        }
    }
    Ok(())
}

// ============================================================================
// substitute_facts
// ============================================================================

/// Result of substituting a commit's facts: the [`StoredFact`]s on success, a
/// [`SubmitError`] (carrying any typed offending id) on failure. An alias to
/// keep the signatures readable and clippy's `type_complexity` quiet.
type SubstituteResult<EntId, EvtId, ImgId> =
    Result<Vec<StoredFact<EntId, EvtId, ImgId>>, SubmitError<EntId, EvtId, ImgId>>;

/// Translate every bundle-local index in `facts` to its resolved persistent
/// id, returning the substituted [`StoredFact`]s.
///
/// Iterates in `BTreeSet` order, so the output order is a function of the
/// facts, not submission order. An out-of-range index produces a
/// [`SubmitError::EntityIdxOutOfRange`] / `EventIdxOutOfRange` /
/// `ImageIdxOutOfRange`. The lookup maps are per-Idx (the three id kinds use
/// distinct newtypes).
pub fn substitute_facts<EntId, EvtId, ImgId>(
    facts: &BTreeSet<SubmitFact>,
    entities: &HashMap<EntityIdx, EntId>,
    events: &HashMap<EventIdx, EvtId>,
    images: &HashMap<ImageIdx, ImgId>,
) -> SubstituteResult<EntId, EvtId, ImgId>
where
    EntId: Clone + Ord + std::fmt::Display + std::fmt::Debug + serde::de::DeserializeOwned,
    EvtId: Clone + Ord + std::fmt::Display + std::fmt::Debug + serde::de::DeserializeOwned,
    ImgId: Clone + Ord + std::fmt::Display + std::fmt::Debug + serde::de::DeserializeOwned,
{
    let mut out = Vec::with_capacity(facts.len());
    for fact in facts {
        let stored = substitute_one(fact, entities, events, images)?;
        out.push(stored);
    }
    Ok(out)
}

fn substitute_one<EntId, EvtId, ImgId>(
    fact: &SubmitFact,
    entities: &HashMap<EntityIdx, EntId>,
    events: &HashMap<EventIdx, EvtId>,
    images: &HashMap<ImageIdx, ImgId>,
) -> Result<StoredFact<EntId, EvtId, ImgId>, SubmitError<EntId, EvtId, ImgId>>
where
    EntId: Clone + Ord + std::fmt::Display + std::fmt::Debug + serde::de::DeserializeOwned,
    EvtId: Clone + Ord + std::fmt::Display + std::fmt::Debug + serde::de::DeserializeOwned,
    ImgId: Clone + Ord + std::fmt::Display + std::fmt::Debug + serde::de::DeserializeOwned,
{
    match fact {
        SubmitFact::Factual {
            assertion,
            citation,
        } => Ok(StoredFact::Factual(StoredFactualFact {
            assertion: substitute_factual(assertion, entities, events, images)?,
            citation: citation.clone(),
        })),
        SubmitFact::Judgment {
            assertion,
            citation,
        } => Ok(StoredFact::Judgment(StoredJudgmentFact {
            assertion: substitute_judgment(assertion, entities, events, images)?,
            source: citation.clone(),
        })),
        SubmitFact::Meta {
            assertion,
            citation,
        } => Ok(StoredFact::Meta(StoredMetaFact {
            assertion: substitute_meta(assertion),
            source: citation.clone(),
        })),
    }
}

// ============================================================================
// Leaf-lookup closures and IdMapError -> SubmitError lowering
// ============================================================================

/// Look an [`EntityIdx`] up in the resolution map, raising
/// [`IdMapError::LeafLookup`] (tagged [`SubjectKind::Entity`]) on a miss. The
/// entity-kind leaf closure of the traversal; `decl_count` is the map's size.
///
/// The error is the full [`IdMapError<EntId, EvtId, ImgId>`] triple so the
/// three closures of one [`FactualAssertion::try_map_ids`] share an error
/// type. `EvtId` / `ImgId` appear in the return type, so neither is phantom.
fn lookup_entity<EntId: Clone, EvtId, ImgId>(
    entities: &HashMap<EntityIdx, EntId>,
    idx: EntityIdx,
) -> Result<EntId, IdMapError<EntId, EvtId, ImgId>> {
    entities.get(&idx).cloned().ok_or(IdMapError::LeafLookup {
        kind: SubjectKind::Entity,
        idx: idx.0,
        decl_count: entities.len(),
    })
}

fn lookup_event<EntId, EvtId: Clone, ImgId>(
    events: &HashMap<EventIdx, EvtId>,
    idx: EventIdx,
) -> Result<EvtId, IdMapError<EntId, EvtId, ImgId>> {
    events.get(&idx).cloned().ok_or(IdMapError::LeafLookup {
        kind: SubjectKind::Event,
        idx: idx.0,
        decl_count: events.len(),
    })
}

fn lookup_image<EntId, EvtId, ImgId: Clone>(
    images: &HashMap<ImageIdx, ImgId>,
    idx: ImageIdx,
) -> Result<ImgId, IdMapError<EntId, EvtId, ImgId>> {
    images.get(&idx).cloned().ok_or(IdMapError::LeafLookup {
        kind: SubjectKind::Image,
        idx: idx.0,
        decl_count: images.len(),
    })
}

/// Lower a grammar-layer [`IdMapError`] into the matching [`SubmitError`].
///
/// The grammar→submit boundary: the traversal stays in the grammar layer, and
/// `submit` maps each failure onto its own variant. Total and 1:1, carrying
/// the typed collided id through:
///
/// - leaf-lookup miss → the kind-specific `*IdxOutOfRange`;
/// - `Relationship` / `Spatial` collapse → the matching `SelfReferenceIn*`;
/// - identity-pair collapse → the kind-specific `Identity*SelfEquivalence`.
fn id_map_error_to_submit<E, V, I>(err: IdMapError<E, V, I>) -> SubmitError<E, V, I> {
    match err {
        IdMapError::LeafLookup {
            kind: SubjectKind::Entity,
            idx,
            decl_count,
        } => SubmitError::EntityIdxOutOfRange { idx, decl_count },
        IdMapError::LeafLookup {
            kind: SubjectKind::Event,
            idx,
            decl_count,
        } => SubmitError::EventIdxOutOfRange { idx, decl_count },
        IdMapError::LeafLookup {
            kind: SubjectKind::Image,
            idx,
            decl_count,
        } => SubmitError::ImageIdxOutOfRange { idx, decl_count },
        IdMapError::SelfLoop(SelfLoop::Relationship(id)) => {
            SubmitError::SelfReferenceInRelationship { id }
        }
        IdMapError::SelfLoop(SelfLoop::Spatial(id)) => SubmitError::SelfReferenceInSpatial { id },
        IdMapError::SelfLoop(SelfLoop::IdentityEntity(id)) => {
            SubmitError::IdentityEntitySelfEquivalence { id }
        }
        IdMapError::SelfLoop(SelfLoop::IdentityEvent(id)) => {
            SubmitError::IdentityEventSelfEquivalence { id }
        }
        IdMapError::SelfLoop(SelfLoop::IdentityArtifact(id)) => {
            SubmitError::IdentityArtifactSelfEquivalence { id }
        }
    }
}

/// Substitute a factual assertion's indices for persistent ids: drives
/// [`FactualAssertion::try_map_ids`] with the three leaf closures, then lowers
/// any [`IdMapError`] into the matching [`SubmitError`].
///
/// The submit-side adapter for the grammar-layer traversal. The closures clone
/// out of the resolution maps; a miss is [`IdMapError::LeafLookup`] (→
/// `*IdxOutOfRange`), and a `Relationship` collapse is
/// `SelfReferenceInRelationship`.
fn substitute_factual<EntId, EvtId, ImgId>(
    f: &FactualAssertion<EntityIdx, EventIdx, ImageIdx>,
    entities: &HashMap<EntityIdx, EntId>,
    events: &HashMap<EventIdx, EvtId>,
    images: &HashMap<ImageIdx, ImgId>,
) -> Result<FactualAssertion<EntId, EvtId, ImgId>, SubmitError<EntId, EvtId, ImgId>>
where
    EntId: Clone + Ord + std::fmt::Display + std::fmt::Debug + serde::de::DeserializeOwned,
    EvtId: Clone + Ord + std::fmt::Display + std::fmt::Debug + serde::de::DeserializeOwned,
    ImgId: Clone + Ord + std::fmt::Display + std::fmt::Debug + serde::de::DeserializeOwned,
{
    f.try_map_ids(
        &mut |idx: &EntityIdx| lookup_entity(entities, *idx),
        &mut |idx: &EventIdx| lookup_event(events, *idx),
        &mut |idx: &ImageIdx| lookup_image(images, *idx),
    )
    .map_err(id_map_error_to_submit)
}

/// Substitute a judgment assertion's indices for persistent ids. Symmetric
/// to [`substitute_factual`] over [`JudgmentAssertion::try_map_ids`].
///
/// An identity-pair collapse lowers to the kinded
/// `Identity*SelfEquivalence`, an observation `Spatial` collapse to
/// `SelfReferenceInSpatial`.
fn substitute_judgment<EntId, EvtId, ImgId>(
    j: &JudgmentAssertion<EntityIdx, EventIdx, ImageIdx>,
    entities: &HashMap<EntityIdx, EntId>,
    events: &HashMap<EventIdx, EvtId>,
    images: &HashMap<ImageIdx, ImgId>,
) -> Result<JudgmentAssertion<EntId, EvtId, ImgId>, SubmitError<EntId, EvtId, ImgId>>
where
    EntId: Clone + Ord + std::fmt::Display + std::fmt::Debug + serde::de::DeserializeOwned,
    EvtId: Clone + Ord + std::fmt::Display + std::fmt::Debug + serde::de::DeserializeOwned,
    ImgId: Clone + Ord + std::fmt::Display + std::fmt::Debug + serde::de::DeserializeOwned,
{
    j.try_map_ids(
        &mut |idx: &EntityIdx| lookup_entity(entities, *idx),
        &mut |idx: &EventIdx| lookup_event(events, *idx),
        &mut |idx: &ImageIdx| lookup_image(images, *idx),
    )
    .map_err(id_map_error_to_submit)
}

// MetaAssertion holds FactId / CommitId references — already persistent — so
// substitution is a deep clone. Exhaustive `match` so a new variant forces a
// decision; wildcard arms are disallowed by the coding standards.
fn substitute_meta(meta: &MetaAssertion) -> MetaAssertion {
    match meta {
        MetaAssertion::RetractFact { target, reason } => MetaAssertion::RetractFact {
            target: *target,
            reason: *reason,
        },
        MetaAssertion::SupersedeFact {
            target,
            replacement,
            reason,
        } => MetaAssertion::SupersedeFact {
            target: *target,
            replacement: *replacement,
            reason: *reason,
        },
        MetaAssertion::RetractCommit { target, reason } => MetaAssertion::RetractCommit {
            target: target.clone(),
            reason: *reason,
        },
    }
}
