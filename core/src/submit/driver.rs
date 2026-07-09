//! Backend-generic submit orchestration.
//!
//! [`FactStore::submit_commit`]'s provided body lives here. The driver runs
//! the whole pipeline — content-address dedup, reference checks, matching,
//! declaration resolution, substitution, rule validation, staging, and the
//! machine-authored companion commit — against the [`FactWrite`] primitives a
//! backend's transaction supplies. A backend implements minting, staging, and
//! commit recording; the sequencing above them is shared.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::future::Future;

use chrono::{DateTime, Utc};

use crate::grammar::assertions::JudgmentAssertion;
use crate::grammar::citations::JudgmentSource;
use crate::grammar::identity;
use crate::grammar::ids::{FactId, IdScheme};
use crate::nonempty::NonEmptyVec;
use crate::store::{
    EntityIdOf, EventIdOf, FactStore, FactWrite, ImageIdOf, SubmitCommitError, SubmitCommitOutput,
};
use crate::submit::matcher::{self, MatchOutcome};
use crate::submit::pipeline::{substitute_facts_accumulating, validate_submit};
use crate::submit::{
    Commit, CommitAuthor, Decl, EntityIdx, EventIdx, ImageIdx, Resolution, ResolutionOrigin,
    StoredCommit, SubjectKind, SubmitError, SubmitFact, SubmitResult,
};

/// [`SubmitError`] over store `S`'s three id kinds.
type StoreSubmitError<S> = SubmitError<EntityIdOf<S>, EventIdOf<S>, ImageIdOf<S>>;

/// [`SubmitCommitError`] over store `S`'s error and id scheme.
type StoreCommitError<S> = SubmitCommitError<<S as FactStore>::Error, <S as FactStore>::Ids>;

/// One id kind's driver hooks — the decl-position newtype, the [`FactWrite`]
/// mint and known-check, and the kind's unknown-`Existing` rejection — so
/// [`resolve_decls`] and [`unknown_existing_decls`] are one function each
/// across the three kinds.
trait DeclKind<S: FactStore> {
    type Id: Clone + Send;
    type Idx: Copy + Eq + std::hash::Hash;
    fn idx(i: usize) -> Self::Idx;
    fn mint<W: FactWrite<S>>(tx: &mut W)
    -> impl Future<Output = Result<Self::Id, S::Error>> + Send;
    fn known<W: FactWrite<S>>(
        tx: &mut W,
        id: &Self::Id,
    ) -> impl Future<Output = Result<bool, S::Error>> + Send;
    fn unknown_existing(decl_position: Self::Idx) -> StoreSubmitError<S>;
}

struct EntityDecls;

impl<S: FactStore> DeclKind<S> for EntityDecls {
    type Id = EntityIdOf<S>;
    type Idx = EntityIdx;
    fn idx(i: usize) -> EntityIdx {
        EntityIdx(i)
    }
    fn mint<W: FactWrite<S>>(
        tx: &mut W,
    ) -> impl Future<Output = Result<Self::Id, S::Error>> + Send {
        tx.mint_entity()
    }
    fn known<W: FactWrite<S>>(
        tx: &mut W,
        id: &Self::Id,
    ) -> impl Future<Output = Result<bool, S::Error>> + Send {
        tx.entity_known(id)
    }
    fn unknown_existing(decl_position: EntityIdx) -> StoreSubmitError<S> {
        SubmitError::UnknownExistingEntity { decl_position }
    }
}

struct EventDecls;

impl<S: FactStore> DeclKind<S> for EventDecls {
    type Id = EventIdOf<S>;
    type Idx = EventIdx;
    fn idx(i: usize) -> EventIdx {
        EventIdx(i)
    }
    fn mint<W: FactWrite<S>>(
        tx: &mut W,
    ) -> impl Future<Output = Result<Self::Id, S::Error>> + Send {
        tx.mint_event()
    }
    fn known<W: FactWrite<S>>(
        tx: &mut W,
        id: &Self::Id,
    ) -> impl Future<Output = Result<bool, S::Error>> + Send {
        tx.event_known(id)
    }
    fn unknown_existing(decl_position: EventIdx) -> StoreSubmitError<S> {
        SubmitError::UnknownExistingEvent { decl_position }
    }
}

struct ImageDecls;

impl<S: FactStore> DeclKind<S> for ImageDecls {
    type Id = ImageIdOf<S>;
    type Idx = ImageIdx;
    fn idx(i: usize) -> ImageIdx {
        ImageIdx(i)
    }
    fn mint<W: FactWrite<S>>(
        tx: &mut W,
    ) -> impl Future<Output = Result<Self::Id, S::Error>> + Send {
        tx.mint_image()
    }
    fn known<W: FactWrite<S>>(
        tx: &mut W,
        id: &Self::Id,
    ) -> impl Future<Output = Result<bool, S::Error>> + Send {
        tx.image_known(id)
    }
    fn unknown_existing(decl_position: ImageIdx) -> StoreSubmitError<S> {
        SubmitError::UnknownExistingImage { decl_position }
    }
}

// ============================================================================
// drive_submit — producer commit plus matcher companion
// ============================================================================

/// Run one producer commit through the pipeline, then persist the matcher's
/// identity judgments as a machine-authored companion commit.
///
/// The companion runs the same pipeline once; its decls are all `Existing`,
/// so its own matched set is empty and the nesting stops there. A companion
/// rejection can only mean a store bug, so it bubbles as this submit's error.
/// The producer is recorded after the companion id is attached, so an
/// idempotent re-submission replays the same companion.
///
/// The whole run sits inside a submit scope
/// ([`FactWrite::with_submit_scope`]): the scope keeps the staging and
/// mints on `Ok` and unwinds them on `Err` — how much of the transaction
/// survives an unwind is the backend's scope behavior. The rejected
/// submit's own error flows out untouched. A dedup replay is a success.
pub(crate) async fn drive_submit<S, W>(tx: &mut W, commit: Commit<S::Ids>) -> SubmitCommitOutput<S>
where
    S: FactStore,
    W: FactWrite<S>,
{
    tx.with_submit_scope(move |scope| {
        Box::pin(async move { submit_with_companion(scope, commit).await })
    })
    .await
    .map_err(SubmitCommitError::Backend)?
}

/// The producer + companion sequence behind [`drive_submit`], run inside the
/// producer's submit scope; the companion gets its own nested scope here.
async fn submit_with_companion<S, W>(tx: &mut W, commit: Commit<S::Ids>) -> SubmitCommitOutput<S>
where
    S: FactStore,
    W: FactWrite<S>,
{
    let recorded_at = commit.recorded_at;
    let submitted = submit_one(tx, commit).await?;
    let mut result = submitted.result;
    // A dedup replay's cached result already carries its companion id.
    let Some((stored, matched)) = submitted.fresh else {
        return Ok(result);
    };

    if let Some(companion) = build_companion_commit::<S>(recorded_at, &matched)? {
        let companion_commit_id = tx
            .with_submit_scope(move |scope| {
                Box::pin(async move { submit_companion(scope, companion).await })
            })
            .await
            .map_err(SubmitCommitError::Backend)??;
        result.companion_commit_id = Some(companion_commit_id);
    }

    tx.record_commit(stored, &result)
        .await
        .map_err(SubmitCommitError::Backend)?;
    Ok(result)
}

/// Pipeline-and-record for the companion commit, run inside its own nested
/// submit scope; returns the companion's commit id for the producer's
/// result.
async fn submit_companion<S, W>(
    tx: &mut W,
    companion: Commit<S::Ids>,
) -> Result<crate::grammar::ids::CommitId, StoreCommitError<S>>
where
    S: FactStore,
    W: FactWrite<S>,
{
    // The companion's decls are all `Existing` over ids this submit just
    // resolved, so a rule rejection here is a store bug — reclassified
    // off the client-facing `Submit` channel, keeping its violations as
    // the diagnosis.
    let companion_submitted = match submit_one(tx, companion).await {
        Err(SubmitCommitError::Submit(batch)) => {
            return Err(SubmitCommitError::Internal {
                message: format!("companion commit rejected: {batch:?}"),
            });
        }
        other => other?,
    };
    if let Some((companion_stored, _)) = companion_submitted.fresh {
        tx.record_commit(companion_stored, &companion_submitted.result)
            .await
            .map_err(SubmitCommitError::Backend)?;
    }
    Ok(companion_submitted.result.commit_id)
}

// ============================================================================
// submit_one — the pipeline for a single commit
// ============================================================================

/// One matcher-asserted identity: the fresh id a matched decl minted, the
/// existing subject it matched, and the anchor facts behind the hit.
struct MatchedPair<Id> {
    fresh: Id,
    matched: Id,
    basis: BTreeSet<FactId>,
}

/// Every identity the matcher asserted during one pipeline run, plus the
/// view snapshot it judged at. Feeds [`build_companion_commit`]; both lists
/// are empty when nothing matched.
struct AssertedIdentities<R: IdScheme> {
    /// Exclusive upper bound of the matcher's view — the watermark before
    /// this commit's facts staged. Every basis id is below it.
    snapshot: FactId,
    entities: Vec<MatchedPair<R::Entity>>,
    images: Vec<MatchedPair<R::Image>>,
}

/// What one pipeline run produced: the result, plus — for a commit the store
/// hadn't seen — the commit record left for the caller to persist and the
/// identities the matcher asserted. `None` when the commit deduped to an
/// already-recorded one.
struct SubmittedCommit<S: FactStore> {
    result: SubmitResult<S::Ids>,
    fresh: Option<(StoredCommit, AssertedIdentities<S::Ids>)>,
}

/// Run the full submit pipeline for one commit against the transaction:
/// content-address dedup, reference checks, matcher, decl resolution,
/// substitution, rule validation, and staging. Recording is left to the
/// caller, which attaches the companion id to the producer's result first so
/// the dedup path replays the full result.
async fn submit_one<S, W>(
    tx: &mut W,
    commit: Commit<S::Ids>,
) -> Result<SubmittedCommit<S>, StoreCommitError<S>>
where
    S: FactStore,
    W: FactWrite<S>,
{
    let commit_id = commit.id().map_err(|e| SubmitCommitError::Hashing {
        message: e.to_string(),
    })?;

    // A re-submit of a known `CommitId` returns the cached result with
    // `previously_committed: true`, doing no mints, inserts, or rule
    // re-evaluation. The cache spans committed state and commits recorded
    // earlier in this transaction.
    if let Some(cached) = tx
        .cached_result(&commit_id)
        .await
        .map_err(SubmitCommitError::Backend)?
    {
        return Ok(SubmittedCommit {
            result: SubmitResult {
                previously_committed: true,
                ..cached
            },
            fresh: None,
        });
    }

    // The matcher runs before any fact stages, so the snapshot captured here
    // is the view every matcher judgment cites.
    let matcher_snapshot = tx.snapshot().await.map_err(SubmitCommitError::Backend)?;

    // Reject unresolvable references before minting, so a malformed bundle
    // burns no ids. A fact pointing at a missing declaration can't resolve, so
    // collect every out-of-range index and every unknown `Decl::Existing` id
    // into one batch.
    let (entity_refs, event_refs, image_refs) = collect_idx_refs(&commit);
    let mut resolvability_errors: Vec<StoreSubmitError<S>> = Vec::new();
    resolvability_errors.extend(check_refs_in_range(
        &entity_refs,
        commit.entities.len(),
        |idx| idx.0,
        |idx, decl_count| SubmitError::EntityIdxOutOfRange { idx, decl_count },
    ));
    resolvability_errors.extend(check_refs_in_range(
        &event_refs,
        commit.events.len(),
        |idx| idx.0,
        |idx, decl_count| SubmitError::EventIdxOutOfRange { idx, decl_count },
    ));
    resolvability_errors.extend(check_refs_in_range(
        &image_refs,
        commit.images.len(),
        |idx| idx.0,
        |idx, decl_count| SubmitError::ImageIdxOutOfRange { idx, decl_count },
    ));
    resolvability_errors.extend(
        unknown_existing_decls::<S, W, EntityDecls>(tx, &commit.entities)
            .await
            .map_err(SubmitCommitError::Backend)?,
    );
    resolvability_errors.extend(
        unknown_existing_decls::<S, W, EventDecls>(tx, &commit.events)
            .await
            .map_err(SubmitCommitError::Backend)?,
    );
    resolvability_errors.extend(
        unknown_existing_decls::<S, W, ImageDecls>(tx, &commit.images)
            .await
            .map_err(SubmitCommitError::Backend)?,
    );
    if let Ok(batch) = NonEmptyVec::try_from_vec(resolvability_errors) {
        return Err(SubmitCommitError::Submit(batch));
    }

    // Judge each Local decl against the view; every Local mints below, and
    // a match becomes an identity judgment in the companion commit. Events
    // are never matched, so their empty outcome map mints every Local fresh.
    let entity_outcomes = matcher::match_entities::<S, W>(&commit.entities, &commit.facts, tx)
        .await
        .map_err(SubmitCommitError::Backend)?;
    let image_outcomes = matcher::match_images::<S, W>(&commit.images, &commit.facts, tx)
        .await
        .map_err(SubmitCommitError::Backend)?;
    let event_outcomes: HashMap<EventIdx, MatchOutcome<EventIdOf<S>>> = HashMap::new();

    // Mints land in the transaction; nothing is durable until it commits.
    let (entity_resolutions, matched_entities) =
        resolve_decls::<S, W, EntityDecls>(tx, &commit.entities, &entity_outcomes)
            .await
            .map_err(SubmitCommitError::Backend)?;
    let (event_resolutions, _) =
        resolve_decls::<S, W, EventDecls>(tx, &commit.events, &event_outcomes)
            .await
            .map_err(SubmitCommitError::Backend)?;
    let (image_resolutions, matched_images) =
        resolve_decls::<S, W, ImageDecls>(tx, &commit.images, &image_outcomes)
            .await
            .map_err(SubmitCommitError::Backend)?;

    // The matcher's verdicts, paired with the fresh ids they minted —
    // the payload of the companion commit.
    let matched = AssertedIdentities {
        snapshot: matcher_snapshot,
        entities: matched_entities,
        images: matched_images,
    };

    // Build substitution maps over the resolved ids.
    let entity_sub_map: HashMap<EntityIdx, EntityIdOf<S>> = entity_resolutions
        .iter()
        .map(|(idx, res)| (*idx, res.id.clone()))
        .collect();
    let event_sub_map: HashMap<EventIdx, EventIdOf<S>> = event_resolutions
        .iter()
        .map(|(idx, res)| (*idx, res.id.clone()))
        .collect();
    let image_sub_map: HashMap<ImageIdx, ImageIdOf<S>> = image_resolutions
        .iter()
        .map(|(idx, res)| (*idx, res.id.clone()))
        .collect();

    // Substitute, accumulating: the reject batch begins with any substitution
    // self-loops, and successfully substituted facts proceed to the rules.
    let (stored_facts, mut validation_errors) = substitute_facts_accumulating::<S::Ids>(
        &commit.facts,
        &entity_sub_map,
        &event_sub_map,
        &image_sub_map,
    );

    // Stage the substituted facts, fixing their order; each staged fact's id
    // is the one it keeps if the transaction commits.
    let mut fact_ids = Vec::with_capacity(stored_facts.len());
    for fact in &stored_facts {
        fact_ids.push(
            tx.stage_fact(fact.clone())
                .await
                .map_err(SubmitCommitError::Backend)?,
        );
    }

    // Unused declarations are checked post-mint; a non-empty reject batch
    // aborts the submit boundary, so neither the staging nor the burned
    // counters can reach durability.
    validation_errors.extend(check_all_decls_referenced(
        &entity_refs,
        commit.entities.len(),
        EntityIdx,
        |position| SubmitError::UnusedDeclaration {
            kind: SubjectKind::Entity,
            position,
        },
    ));
    validation_errors.extend(check_all_decls_referenced(
        &event_refs,
        commit.events.len(),
        EventIdx,
        |position| SubmitError::UnusedDeclaration {
            kind: SubjectKind::Event,
            position,
        },
    ));
    validation_errors.extend(check_all_decls_referenced(
        &image_refs,
        commit.images.len(),
        ImageIdx,
        |position| SubmitError::UnusedDeclaration {
            kind: SubjectKind::Image,
            position,
        },
    ));

    // Two `Existing` declarations of one kind naming the same persistent
    // id is non-canonical: it breaks content-address dedup, and a
    // self-pair would otherwise slip through. One error per duplicated id.
    validation_errors.extend(
        duplicate_existing_decl_ids(&entity_resolutions)
            .into_iter()
            .map(|id| SubmitError::DuplicateEntityDecl { id }),
    );
    validation_errors.extend(
        duplicate_existing_decl_ids(&event_resolutions)
            .into_iter()
            .map(|id| SubmitError::DuplicateEventDecl { id }),
    );
    validation_errors.extend(
        duplicate_existing_decl_ids(&image_resolutions)
            .into_iter()
            .map(|id| SubmitError::DuplicateImageDecl { id }),
    );

    // Run the rule validator (meta + cluster rules) over the transaction view,
    // folding its batch in. Only a backend read failure short-circuits; a rule
    // violation joins the batch.
    validation_errors.extend(
        validate_submit::<S, W>(&stored_facts, tx)
            .await
            .map_err(SubmitCommitError::Backend)?,
    );

    if let Ok(batch) = NonEmptyVec::try_from_vec(validation_errors) {
        return Err(SubmitCommitError::Submit(batch));
    }

    let stored = StoredCommit {
        commit_id: commit_id.clone(),
        author: commit.author,
        recorded_at: commit.recorded_at,
        fact_ids: fact_ids.clone(),
    };
    let result = SubmitResult {
        commit_id,
        previously_committed: false,
        fact_ids,
        entities: entity_resolutions,
        events: event_resolutions,
        images: image_resolutions,
        companion_commit_id: None,
    };
    Ok(SubmittedCommit {
        result,
        fresh: Some((stored, matched)),
    })
}

// ============================================================================
// Companion commit
// ============================================================================

/// Build the matcher's companion commit: one machine-authored identity
/// judgment per matched decl, each citing the anchor facts behind the match
/// and the snapshot it was judged at. `recorded_at` borrows the producer
/// commit's timestamp so the companion's content address is a function of the
/// producer bundle and the matched state, not of wall-clock at persist time.
/// `None` when nothing matched.
fn build_companion_commit<S: FactStore>(
    recorded_at: DateTime<Utc>,
    matched: &AssertedIdentities<S::Ids>,
) -> Result<Option<Commit<S::Ids>>, StoreCommitError<S>> {
    if matched.entities.is_empty() && matched.images.is_empty() {
        return Ok(None);
    }
    let (process, version) = matcher::matcher_identity();
    let derivation = |basis: &BTreeSet<FactId>| JudgmentSource::Derivation {
        process: process.clone(),
        version: version.clone(),
        basis: basis.clone(),
        snapshot: matched.snapshot,
    };

    let mut entities: Vec<EntityIdOf<S>> = Vec::new();
    let mut images: Vec<ImageIdOf<S>> = Vec::new();
    let mut facts: BTreeSet<SubmitFact> = BTreeSet::new();
    for pair in &matched.entities {
        let fresh = EntityIdx(intern(&mut entities, &pair.fresh));
        let existing = EntityIdx(intern(&mut entities, &pair.matched));
        let fact = identity::Fact::same_entity(fresh, existing).map_err(companion_bug::<S, _>)?;
        facts.insert(SubmitFact::Judgment {
            assertion: JudgmentAssertion::Identity { fact },
            citation: derivation(&pair.basis),
        });
    }
    for pair in &matched.images {
        let fresh = ImageIdx(intern(&mut images, &pair.fresh));
        let existing = ImageIdx(intern(&mut images, &pair.matched));
        let fact = identity::Fact::same_artifact(fresh, existing).map_err(companion_bug::<S, _>)?;
        facts.insert(SubmitFact::Judgment {
            assertion: JudgmentAssertion::Identity { fact },
            citation: derivation(&pair.basis),
        });
    }

    Ok(Some(Commit {
        author: CommitAuthor::Analyzer { process, version },
        recorded_at,
        entities: entities
            .into_iter()
            .map(|id| Decl::Existing { id })
            .collect(),
        events: Vec::new(),
        images: images.into_iter().map(|id| Decl::Existing { id }).collect(),
        facts,
    }))
}

/// The declaration slot of `id`, appending a new slot on first sight. The
/// matched pairs arrive in mint order, so slot assignment is deterministic; a
/// subject shared across pairs (two decls matching one entity) declares once.
fn intern<Id: Clone + PartialEq>(decls: &mut Vec<Id>, id: &Id) -> usize {
    match decls.iter().position(|d| d == id) {
        Some(i) => i,
        None => {
            decls.push(id.clone());
            decls.len() - 1
        }
    }
}

/// A companion-commit construction failure is a store bug — the inputs are
/// fresh mints paired with pre-existing subjects under a fixed analyzer
/// identity — so it surfaces loudly rather than persisting.
fn companion_bug<S: FactStore, E: std::fmt::Display>(e: E) -> StoreCommitError<S> {
    SubmitCommitError::Internal {
        message: format!("companion commit construction failed: {e}"),
    }
}

// ============================================================================
// Decl resolution (single pass per id kind)
// ============================================================================

/// Resolve every declaration of one kind to a persistent id, pairing each
/// matched decl with the identity the matcher asserted.
///
/// [`Decl::Existing`] passes through as `DeclaredExisting`. Every
/// [`Decl::Local`] mints fresh; the matcher outcome at the matching `Idx`
/// selects the origin — `MatchedExisting` carrying the matched subject (the
/// identity judgment lands in the companion commit), or `NewlyMinted` /
/// `Ambiguous` by the candidate count. A `Matched` outcome also yields a
/// [`MatchedPair`] here, at the moment the fresh id exists, so the pairs and
/// the `MatchedExisting` resolutions cannot drift apart. Pairs come back in
/// mint order — ascending fresh id — the deterministic order the companion's
/// decl list interns.
///
/// Matcher contract: one entry per `Decl::Local` keyed by `Idx`, none for
/// `Decl::Existing`. A missing `Local` entry is treated as a no-match rather
/// than failing the commit. Surplus entries can't arise — `match_entities` /
/// `match_images` only emit in-range positions.
async fn resolve_decls<S, W, K>(
    tx: &mut W,
    decls: &[Decl<K::Id>],
    outcomes: &HashMap<K::Idx, MatchOutcome<K::Id>>,
) -> Result<(HashMap<K::Idx, Resolution<K::Id>>, Vec<MatchedPair<K::Id>>), S::Error>
where
    S: FactStore,
    W: FactWrite<S>,
    K: DeclKind<S>,
{
    let mut out = HashMap::with_capacity(decls.len());
    let mut matched_pairs = Vec::new();
    for (i, decl) in decls.iter().enumerate() {
        let idx = K::idx(i);
        let resolution = match decl {
            Decl::Existing { id } => Resolution {
                id: id.clone(),
                origin: ResolutionOrigin::DeclaredExisting,
            },
            Decl::Local => match outcomes.get(&idx) {
                Some(MatchOutcome::Matched { id, basis }) => {
                    let fresh = K::mint(tx).await?;
                    matched_pairs.push(MatchedPair {
                        fresh: fresh.clone(),
                        matched: id.clone(),
                        basis: basis.clone(),
                    });
                    Resolution {
                        id: fresh,
                        origin: ResolutionOrigin::MatchedExisting {
                            matched: id.clone(),
                        },
                    }
                }
                // A missing Local outcome is treated as
                // `Unmatched { candidates: vec![] }`: mint fresh with no
                // candidates rather than drop the decl and corrupt the map.
                Some(MatchOutcome::Unmatched { candidates }) => {
                    let id = K::mint(tx).await?;
                    let origin = NonEmptyVec::try_from_vec(candidates.clone()).map_or(
                        ResolutionOrigin::NewlyMinted,
                        |nonempty| ResolutionOrigin::Ambiguous {
                            candidates: nonempty,
                        },
                    );
                    Resolution { id, origin }
                }
                None => Resolution {
                    id: K::mint(tx).await?,
                    origin: ResolutionOrigin::NewlyMinted,
                },
            },
        };
        out.insert(idx, resolution);
    }
    Ok((out, matched_pairs))
}

// ============================================================================
// Reference checks
// ============================================================================

/// Collect the bundle-local indices every fact references, bucketed by kind.
/// `Meta` targets are persistent `FactId` / `CommitId`, not indices, so they
/// contribute none. The collector half of the id-traversal; the resulting sets
/// feed both the out-of-range check and the unused-declaration check.
fn collect_idx_refs<R: IdScheme>(
    commit: &Commit<R>,
) -> (HashSet<EntityIdx>, HashSet<EventIdx>, HashSet<ImageIdx>) {
    let mut entity_refs: HashSet<EntityIdx> = HashSet::new();
    let mut event_refs: HashSet<EventIdx> = HashSet::new();
    let mut image_refs: HashSet<ImageIdx> = HashSet::new();

    for fact in &commit.facts {
        fact.for_each_id(
            &mut |idx: &EntityIdx| {
                entity_refs.insert(*idx);
            },
            &mut |idx: &EventIdx| {
                event_refs.insert(*idx);
            },
            &mut |idx: &ImageIdx| {
                image_refs.insert(*idx);
            },
        );
    }
    (entity_refs, event_refs, image_refs)
}

/// The unknown-`Existing` rejections for one kind's declaration list: every
/// `Decl::Existing` whose id the store never minted, in declaration order.
async fn unknown_existing_decls<S, W, K>(
    tx: &mut W,
    decls: &[Decl<K::Id>],
) -> Result<Vec<StoreSubmitError<S>>, S::Error>
where
    S: FactStore,
    W: FactWrite<S>,
    K: DeclKind<S>,
{
    let mut errors = Vec::new();
    for (i, decl) in decls.iter().enumerate() {
        if let Decl::Existing { id } = decl
            && !K::known(tx, id).await?
        {
            errors.push(K::unknown_existing(K::idx(i)));
        }
    }
    Ok(errors)
}

/// Every reference past the end of its kind's declaration list, one error per
/// offending position in ascending order. Parameterised over the reference
/// set, declaration count, the `position` extractor, and the out-of-range
/// error constructor.
fn check_refs_in_range<Idx, E>(
    refs: &HashSet<Idx>,
    decl_count: usize,
    position: impl Fn(&Idx) -> usize,
    out_of_range: impl Fn(usize, usize) -> E,
) -> Vec<E> {
    let mut offending: Vec<usize> = refs
        .iter()
        .map(&position)
        .filter(|&idx| idx >= decl_count)
        .collect();
    offending.sort_unstable();
    offending
        .into_iter()
        .map(|idx| out_of_range(idx, decl_count))
        .collect()
}

/// Every declaration position with no incoming reference, one error per
/// position in ascending order. A decl with no fact under it is almost
/// certainly a bug, better surfaced than silently minted. Parameterised over
/// the reference set, declaration count, the `idx_ctor` for testing set
/// membership, and the unreferenced-position error constructor.
fn check_all_decls_referenced<Idx, E>(
    refs: &HashSet<Idx>,
    decl_count: usize,
    idx_ctor: fn(usize) -> Idx,
    unused: impl Fn(usize) -> E,
) -> Vec<E>
where
    Idx: Eq + std::hash::Hash,
{
    (0..decl_count)
        .filter(|i| !refs.contains(&idx_ctor(*i)))
        .map(unused)
        .collect()
}

/// The persistent ids that more than one `Decl::Existing` declaration named,
/// sorted and deduped. Local decls always mint distinct fresh ids, so a
/// producer naming one id in two decl slots is the only way declarations of
/// one kind can collide. The order is deterministic so a rejected commit's
/// batch is reproducible.
fn duplicate_existing_decl_ids<Idx, Id>(resolutions: &HashMap<Idx, Resolution<Id>>) -> Vec<Id>
where
    Id: Clone + Ord,
{
    let mut seen: BTreeSet<Id> = BTreeSet::new();
    let mut duplicated: BTreeSet<Id> = BTreeSet::new();
    for res in resolutions.values() {
        if !matches!(res.origin, ResolutionOrigin::DeclaredExisting) {
            continue;
        }
        if !seen.insert(res.id.clone()) {
            duplicated.insert(res.id.clone());
        }
    }
    duplicated.into_iter().collect()
}
