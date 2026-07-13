//! Shared submit-pipeline machinery.
//!
//! [`FactStore::submit_commit`](crate::store::FactStore::submit_commit)
//! is provided: the shared driver composes the free functions here with the
//! [`FactWrite`](crate::store::FactWrite) primitives a backend's
//! transaction supplies, so every backend reuses the orchestration.
//!
//! The module provides:
//!
//! - [`substitute_facts_accumulating`] — translates index references to
//!   persistent ids, collecting every per-fact substitution failure.
//! - [`validate_submit`] — runs the submit-rule checks (the meta-fact target
//!   guards plus the cluster rules) over the in-flight commit, accumulating
//!   every violation into one batch.
//!
//! Submit-time matching lives in [`matcher`](super::matcher); commit-id
//! derivation lives on [`Commit::id`](super::Commit::id) — backends call
//! `bundle.id()?`.
//!
//! The validator is async so a SQL backend can `.await` reads (index lookups,
//! rule reads) inside the transaction holding the commit; the in-memory
//! backend reads its unified view through the same async [`FactView`]
//! surface. It takes a `&mut V` bounded by the view traits it reads and
//! returns a future.
//! The in-memory backend's [`async_lock::Mutex`] guard is `Send`, so holding
//! it across these awaits keeps the future `Send`. The validator awaits even
//! in-memory — resolving each retraction target through `view.fact(...)` and
//! each commit-retraction target through `view.commit_known(...)` — but
//! services those reads from memory, where a SQL backend awaits the equivalent
//! reads against its transaction.

use std::collections::{BTreeSet, HashMap};

use futures_util::TryStreamExt;

use super::error::{DateRole, LocationRole, SubmitError};
use super::result::{StoredFact, StoredFactualFact, StoredJudgmentFact, StoredMetaFact};
use super::{EntityIdx, EventIdx, ImageIdx, SubmitFact};
use crate::date::UncertainDate;
use crate::grammar::assertions::{FactualAssertion, JudgmentAssertion, MetaAssertion};
use crate::grammar::citations::{ExternalSource, FactualCitation, JudgmentSource, MetaSource};
use crate::grammar::identity::{self, IdMapError, SelfLoop};
use crate::grammar::ids::{FactId, IdScheme, SubjectKind};
use crate::grammar::lifecycle::LifetimeEventKind;
use crate::grammar::{attribute, bookend, composites, event, image};
use crate::location::{ConflictStatus, UnresolvedLocation};
use crate::store::pagination::{PAGE_SIZE, paginate};
use crate::store::{
    EntityIdOf, EventIdOf, EventView, FactPlacement, FactStore, FactView, ImageIdOf, ImageView,
    StoredFactOf, SubmitCommitError, SubmitCommitInput, SubmitCommitOutput,
};

/// Sanity bound on the number of circles a stored location may name. A place is
/// a handful of spots, not hundreds — like
/// [`MAX_UNCERTAINTY_RADIUS`](crate::location::MAX_UNCERTAINTY_RADIUS) on a
/// single circle's radius, this is an input-validation bound, here on the whole
/// location's circle count. It bounds the cubic candidate-point cost of
/// [`UnresolvedLocation::conflict_status`]'s emptiness check against adversarial
/// input.
pub const MAX_LOCATION_CIRCLES: usize = 100;

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
// validate_submit
// ============================================================================

/// Run the submit-rule checks over the in-flight commit.
///
/// Each [`MetaAssertion`] target resolves against `view` (the committed
/// snapshot unioned with this commit's already-pushed facts), and
/// [`FactView::placement`] classifies a fact target as pre-existing,
/// same-commit, or absent:
///
/// - [`MetaAssertion::RetractFact`] / [`MetaAssertion::SupersedeFact`] name a
///   `target` [`FactId`]. An [`FactPlacement::Absent`] target doesn't exist
///   ([`SubmitError::FactNotFound`]); an [`FactPlacement::InFlight`] one is a
///   fact in this commit ([`SubmitError::MetaTargetInSameCommit`]); a
///   [`FactPlacement::Committed`] one passes. `SupersedeFact` also rejects
///   `target == replacement` ([`SubmitError::SupersedeReplacementEqualsTarget`])
///   and requires `replacement` to merely exist — prior or minted in this
///   commit — rejecting only [`FactPlacement::Absent`] with
///   [`SubmitError::FactNotFound`].
/// - [`MetaAssertion::RetractCommit`] names a `target`
///   [`CommitId`](crate::grammar::ids::CommitId), resolved via
///   [`FactView::commit_known`]. An unrecorded commit is
///   [`SubmitError::CommitNotFound`]; a recorded one passes.
///
/// Async: target lookups go through `view.placement(...)` /
/// `view.commit_known(...)`, served from memory here and from the open
/// transaction in a SQL backend.
///
/// Returns `Ok(batch)` — every rule violation found, in deterministic order, for
/// the caller to fold into its accumulating reject batch — or `Err(S::Error)`
/// when a backend read fails. An empty `Vec` means the commit passed every rule.
///
/// The bound widens to [`EventView`] + [`ImageView`] because the gather phase
/// drains `all_facts_about_event` / `all_facts_about_image` through `view` to
/// build the subject neighbourhood the cluster rules read.
pub async fn validate_submit<S: FactStore, V: FactView<S> + EventView<S> + ImageView<S>>(
    candidates: &[StoredFactOf<S>],
    view: &mut V,
) -> Result<Vec<SubmitError<EntityIdOf<S>, EventIdOf<S>, ImageIdOf<S>>>, S::Error> {
    let mut errors = Vec::new();
    for fact in candidates {
        let StoredFact::Meta(meta) = fact else {
            continue;
        };
        // Exhaustive so a new MetaAssertion variant forces a decision here.
        match &meta.assertion {
            MetaAssertion::RetractFact { target, .. } => {
                if let Some(e) = check_meta_fact_target::<S, V>(&mut *view, *target).await? {
                    errors.push(e);
                }
            }
            MetaAssertion::SupersedeFact {
                target,
                replacement,
                ..
            } => {
                if target == replacement {
                    // A self-superseding meta-fact is malformed on its own; the
                    // target/replacement existence checks would be redundant.
                    errors.push(SubmitError::SupersedeReplacementEqualsTarget { target: *target });
                } else {
                    if let Some(e) = check_meta_fact_target::<S, V>(&mut *view, *target).await? {
                        errors.push(e);
                    }
                    // The replacement is the corrected fact; require only that it
                    // names a real fact — a prior one, or one minted in this commit
                    // (already visible through `view`). Only an absent id is a miss.
                    if view.placement(*replacement).await? == FactPlacement::Absent {
                        errors.push(SubmitError::FactNotFound { id: *replacement });
                    }
                }
            }
            MetaAssertion::RetractCommit { target, .. } => {
                if !view.commit_known(target).await? {
                    errors.push(SubmitError::CommitNotFound { id: target.clone() });
                }
            }
        }
    }
    let (event_facts, image_facts) = gather_subject_neighborhood::<S, V>(candidates, view).await?;
    run_cluster_rules::<S>(candidates, &event_facts, &image_facts, &mut errors);
    Ok(errors)
}

/// Drain each event and image this commit's `candidates` touch exactly once,
/// producing the subject-keyed neighbourhood the cluster rules read.
///
/// `for_each_id` folds each candidate's mentioned subject ids (the entity
/// closure is a no-op — no cluster rule keys off the entity neighbourhood, and
/// the judgment citation's observed image already reaches `fi`). Every touched
/// event / image is drained once through the union view, so a subject several
/// candidates mention is read a single time. The maps are looked up by id,
/// never iterated for output, so the `HashMap` choice doesn't affect rule
/// determinism.
async fn gather_subject_neighborhood<S, V>(
    candidates: &[StoredFactOf<S>],
    view: &mut V,
) -> Result<
    (
        HashMap<EventIdOf<S>, Vec<StoredFactOf<S>>>,
        HashMap<ImageIdOf<S>, Vec<StoredFactOf<S>>>,
    ),
    S::Error,
>
where
    S: FactStore,
    V: EventView<S> + ImageView<S>,
{
    let mut events: BTreeSet<EventIdOf<S>> = BTreeSet::new();
    let mut images: BTreeSet<ImageIdOf<S>> = BTreeSet::new();
    for fact in candidates {
        fact.for_each_id(
            &mut |_entity| {},
            &mut |event: &EventIdOf<S>| {
                events.insert(event.clone());
            },
            &mut |image: &ImageIdOf<S>| {
                images.insert(image.clone());
            },
        );
    }
    let mut event_facts: HashMap<EventIdOf<S>, Vec<StoredFactOf<S>>> = HashMap::new();
    for e in events {
        let event = &e;
        let facts = paginate(&mut *view, |v, cursor| async move {
            let page = v.all_facts_about_event(event, cursor, PAGE_SIZE).await?;
            let (rows, next) = page.into_parts();
            Ok::<_, S::Error>((rows, next, v))
        })
        .map_ok(|item| item.fact)
        .try_collect()
        .await?;
        event_facts.insert(e, facts);
    }
    let mut image_facts: HashMap<ImageIdOf<S>, Vec<StoredFactOf<S>>> = HashMap::new();
    for i in images {
        let image = &i;
        let facts = paginate(&mut *view, |v, cursor| async move {
            let page = v.all_facts_about_image(image, cursor, PAGE_SIZE).await?;
            let (rows, next) = page.into_parts();
            Ok::<_, S::Error>((rows, next, v))
        })
        .map_ok(|item| item.fact)
        .try_collect()
        .await?;
        image_facts.insert(i, facts);
    }
    Ok((event_facts, image_facts))
}

/// Resolve a retract/supersede fact-target by its [`FactPlacement`].
///
/// A target must predate the in-flight commit ([`FactPlacement::Committed`]).
/// An [`FactPlacement::InFlight`] target is minted in this commit
/// ([`SubmitError::MetaTargetInSameCommit`]); an [`FactPlacement::Absent`] one
/// was never minted ([`SubmitError::FactNotFound`]). `Ok(None)` means the
/// target passes; only a backend read failure short-circuits as `Err`.
async fn check_meta_fact_target<S: FactStore, V: FactView<S>>(
    view: &mut V,
    target: FactId,
) -> Result<Option<SubmitError<EntityIdOf<S>, EventIdOf<S>, ImageIdOf<S>>>, S::Error> {
    Ok(match view.placement(target).await? {
        FactPlacement::Absent => Some(SubmitError::FactNotFound { id: target }),
        FactPlacement::InFlight => Some(SubmitError::MetaTargetInSameCommit { target }),
        FactPlacement::Committed => None,
    })
}

// ============================================================================
// substitute_facts
// ============================================================================

/// [`SubmitError`] over an id scheme `R`'s three id kinds. Names the
/// leaf-chained triple once so the substitute signatures stay readable and
/// under clippy's `type_complexity` bar.
type SchemeSubmitError<R> =
    SubmitError<<R as IdScheme>::Entity, <R as IdScheme>::Event, <R as IdScheme>::Image>;

/// The substituted [`StoredFact`]s plus the per-fact [`SubmitError`]s
/// substitution rejected. An alias to keep the signature readable and clippy's
/// `type_complexity` quiet.
type SubstituteResult<R> = (Vec<StoredFact<R>>, Vec<SchemeSubmitError<R>>);

/// Translate every bundle-local index in `facts` to its resolved persistent
/// id, accumulating rather than stopping at the first failure.
///
/// Iterates in `BTreeSet` order, so both the output facts and the errors are a
/// function of the facts, not submission order. Each successfully substituted
/// fact lands in the facts vec and proceeds to the rules; a fact whose
/// substitution fails — an out-of-range index
/// ([`SubmitError::EntityIdxOutOfRange`] / `EventIdxOutOfRange` /
/// `ImageIdxOutOfRange`) or a resolved self-loop — lands in the error vec and is
/// omitted from the rule set. The lookup maps are per-Idx (the
/// three id kinds use distinct newtypes).
pub fn substitute_facts_accumulating<R: IdScheme>(
    facts: &BTreeSet<SubmitFact>,
    entities: &HashMap<EntityIdx, R::Entity>,
    events: &HashMap<EventIdx, R::Event>,
    images: &HashMap<ImageIdx, R::Image>,
) -> SubstituteResult<R> {
    let mut out = Vec::with_capacity(facts.len());
    let mut errors = Vec::new();
    for fact in facts {
        match substitute_one(fact, entities, events, images) {
            Ok(stored) => out.push(stored),
            Err(e) => errors.push(e),
        }
    }
    (out, errors)
}

fn substitute_one<R: IdScheme>(
    fact: &SubmitFact,
    entities: &HashMap<EntityIdx, R::Entity>,
    events: &HashMap<EventIdx, R::Event>,
    images: &HashMap<ImageIdx, R::Image>,
) -> Result<StoredFact<R>, SchemeSubmitError<R>> {
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
            source: substitute_judgment_source::<R>(citation, images)?,
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
fn id_map_error_to_submit<EntId, EvtId, ImgId>(
    err: IdMapError<EntId, EvtId, ImgId>,
) -> SubmitError<EntId, EvtId, ImgId> {
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
fn substitute_factual<R: IdScheme>(
    f: &FactualAssertion<crate::submit::BundleLocal>,
    entities: &HashMap<EntityIdx, R::Entity>,
    events: &HashMap<EventIdx, R::Event>,
    images: &HashMap<ImageIdx, R::Image>,
) -> Result<FactualAssertion<R>, SchemeSubmitError<R>> {
    f.try_map_ids::<R>(
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
fn substitute_judgment<R: IdScheme>(
    j: &JudgmentAssertion<crate::submit::BundleLocal>,
    entities: &HashMap<EntityIdx, R::Entity>,
    events: &HashMap<EventIdx, R::Event>,
    images: &HashMap<ImageIdx, R::Image>,
) -> Result<JudgmentAssertion<R>, SchemeSubmitError<R>> {
    j.try_map_ids::<R>(
        &mut |idx: &EntityIdx| lookup_entity(entities, *idx),
        &mut |idx: &EventIdx| lookup_event(events, *idx),
        &mut |idx: &ImageIdx| lookup_image(images, *idx),
    )
    .map_err(id_map_error_to_submit)
}

/// Substitute a judgment citation's observed-image index for its persistent id.
/// Only `ImageObservation` carries an image; an out-of-range index lowers to
/// [`SubmitError::ImageIdxOutOfRange`], the same as the assertion path.
fn substitute_judgment_source<R: IdScheme>(
    source: &JudgmentSource<ImageIdx>,
    images: &HashMap<ImageIdx, R::Image>,
) -> Result<JudgmentSource<R::Image>, SchemeSubmitError<R>> {
    source
        .try_map_image(&mut |idx: &ImageIdx| {
            lookup_image::<R::Entity, R::Event, R::Image>(images, *idx)
        })
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

// ============================================================================
// Cluster rules
// ============================================================================

/// Run the cluster rules, appending every violation to `errors` in a fixed
/// order. Each rule keys off the subjects this commit's `candidates` touch and
/// reads the committed ∪ pending fact set from the precomputed neighbourhood
/// (`event_facts` / `image_facts`), so the rules are pure lookups — the gather
/// phase already did every drain.
fn run_cluster_rules<S>(
    candidates: &[StoredFactOf<S>],
    event_facts: &HashMap<EventIdOf<S>, Vec<StoredFactOf<S>>>,
    image_facts: &HashMap<ImageIdOf<S>, Vec<StoredFactOf<S>>>,
    errors: &mut Vec<SubmitError<EntityIdOf<S>, EventIdOf<S>, ImageIdOf<S>>>,
) where
    S: FactStore,
{
    rule_event_has_one_kind::<S>(candidates, event_facts, errors);
    rule_event_fact_kind_consistency::<S>(candidates, event_facts, errors);
    rule_same_event_unresolvable::<S>(candidates, errors);
    rule_name_window::<S>(candidates, errors);
    rule_single_interval_date::<S>(candidates, errors);
    rule_location_validity::<S>(candidates, errors);
    rule_observation_depiction::<S>(candidates, image_facts, errors);
    rule_composite_self_parent::<S>(candidates, errors);
    rule_composite_multiple_parents::<S>(candidates, image_facts, errors);
    rule_composite_chain::<S>(candidates, image_facts, errors);
}

/// The distinct `{entity, kind}` `HasEvent` claims an event carries in the
/// cumulative neighbourhood (committed ∪ this commit, active-only). The set
/// keys on `(entity, kind)`, so an identical claim re-asserted in another
/// commit collapses to one entry; counting distinct entries signals genuine
/// disagreement: one is well-typed, ≥2 is a subject/kind self-contradiction.
fn has_event_claims<S>(
    event: &EventIdOf<S>,
    gathered: &[StoredFactOf<S>],
) -> BTreeSet<(EntityIdOf<S>, LifetimeEventKind)>
where
    S: FactStore,
{
    let mut claims = BTreeSet::new();
    for fact in gathered {
        if let Some(event::Fact::HasEvent {
            entity,
            event: e,
            kind,
        }) = fact.event_fact()
            && e == event
        {
            claims.insert((entity.clone(), *kind));
        }
    }
    claims
}

/// Exactly one `HasEvent` per event id. An event has one subject entity and one
/// declared kind, so each id the commit *references* — as a typing or payload
/// subject, or as a `Gap` endpoint — must carry exactly one `HasEvent` across
/// the cumulative neighbourhood (committed ∪ this commit): no `HasEvent` is
/// [`SubmitError::EventMissingHasEvent`]; two or more *distinct* `HasEvent`
/// facts is [`SubmitError::EventMultipleHasEvent`]. Re-asserting an identical
/// `HasEvent` collapses into the `{entity, kind}` set, so it never counts twice.
/// A `Gap` endpoint pointing at an untyped event id trips the missing case,
/// which is why the set spans every referenced id, not just typing/payload
/// subjects.
/// Cross-source disagreement on the subject or kind belongs on separate
/// event ids, not on one id.
fn rule_event_has_one_kind<S>(
    candidates: &[StoredFactOf<S>],
    event_facts: &HashMap<EventIdOf<S>, Vec<StoredFactOf<S>>>,
    errors: &mut Vec<SubmitError<EntityIdOf<S>, EventIdOf<S>, ImageIdOf<S>>>,
) where
    S: FactStore,
{
    for e in referenced_events::<S>(candidates) {
        let gathered = event_facts.get(&e).map(Vec::as_slice).unwrap_or(&[]);
        match has_event_claims::<S>(&e, gathered).len() {
            0 => errors.push(SubmitError::EventMissingHasEvent { event: e }),
            1 => {}
            _ => errors.push(SubmitError::EventMultipleHasEvent { event: e }),
        }
    }
}

/// Cumulative payload/date↔kind consistency. Every payload or date fact active
/// on a touched event must suit the kind the event declares via `HasEvent` — a
/// `DamageCause` only on a `Damaged` event, a `DurationalDate` only on a
/// durational kind, and so on (the [`crate::grammar::event`] availability matrix).
/// Both the declared kind and the payloads read from the cumulative
/// neighbourhood (committed ∪ this commit, active-only), so the check covers a
/// payload added against a prior typing *and* a prior payload left active when
/// this commit re-types the event. Re-typing therefore requires retracting the
/// payloads the new kind doesn't admit in the same commit, or the commit is
/// rejected. An event already failing [`rule_event_has_one_kind`] with ≥2
/// distinct `HasEvent` is skipped — its kind is genuinely disputed, an
/// order-dependent mismatch here would just double-report. Stored cross-source
/// kind disagreement lives on separate event ids, each internally consistent.
fn rule_event_fact_kind_consistency<S>(
    candidates: &[StoredFactOf<S>],
    event_facts: &HashMap<EventIdOf<S>, Vec<StoredFactOf<S>>>,
    errors: &mut Vec<SubmitError<EntityIdOf<S>, EventIdOf<S>, ImageIdOf<S>>>,
) where
    S: FactStore,
{
    // The single declared kind per touched event, from the cumulative
    // neighbourhood. Skip an event with ≥2 distinct claims (disputed kind).
    let mut declared: HashMap<EventIdOf<S>, LifetimeEventKind> = HashMap::new();
    for e in touched_events::<S>(candidates) {
        let gathered = event_facts.get(&e).map(Vec::as_slice).unwrap_or(&[]);
        // 0 or ≥2 distinct claims: rule_event_has_one_kind already reports it,
        // and there's no single declared kind to typecheck against, so skip.
        let claims = has_event_claims::<S>(&e, gathered);
        if claims.len() == 1
            && let Some((_, kind)) = claims.into_iter().next()
        {
            declared.insert(e, kind);
        }
    }
    let mut offending: BTreeSet<(EventIdOf<S>, &'static str, LifetimeEventKind)> = BTreeSet::new();
    for (event, &kind) in &declared {
        let gathered = event_facts.get(event).map(Vec::as_slice).unwrap_or(&[]);
        for fact in gathered {
            if let Some(ef) = fact.event_fact()
                && ef.subject() == event
                && !ef.kind_constraints().contains(&kind)
            {
                offending.insert((event.clone(), ef.into(), kind));
            }
        }
    }
    for (event, fact, declared) in offending {
        errors.push(SubmitError::EventFactKindMismatch {
            event,
            fact,
            declared,
        });
    }
}

/// The distinct event ids this commit's candidates mention as a subject — the
/// typing or payload subject of an event fact.
fn touched_events<S>(candidates: &[StoredFactOf<S>]) -> BTreeSet<EventIdOf<S>>
where
    S: FactStore,
{
    let mut events = BTreeSet::new();
    for fact in candidates {
        if let Some(ef) = fact.event_fact() {
            events.insert(ef.subject().clone());
        }
    }
    events
}

/// Every distinct event id any candidate references — typing/payload subjects
/// plus the event endpoints of a `Gap` (and any `SameEvent` member). The full
/// id closure, so the exactly-one-`HasEvent` rule reaches an event named only
/// as a gap endpoint.
fn referenced_events<S>(candidates: &[StoredFactOf<S>]) -> BTreeSet<EventIdOf<S>>
where
    S: FactStore,
{
    let mut events = BTreeSet::new();
    for fact in candidates {
        fact.for_each_id(
            &mut |_entity| {},
            &mut |event: &EventIdOf<S>| {
                events.insert(event.clone());
            },
            &mut |_image| {},
        );
    }
    events
}

/// `SameEvent` identity facts are rejected: reads answer singleton event
/// classes — event equivalence is not resolved — so a stored edge would be
/// silently ignored. Pushes dedup by the event pair, so differently-cited
/// facts over one pair yield one error.
fn rule_same_event_unresolvable<S: FactStore>(
    candidates: &[StoredFactOf<S>],
    errors: &mut Vec<SubmitError<EntityIdOf<S>, EventIdOf<S>, ImageIdOf<S>>>,
) {
    let mut offending: BTreeSet<(EventIdOf<S>, EventIdOf<S>)> = BTreeSet::new();
    for fact in candidates {
        if let StoredFact::Judgment(StoredJudgmentFact {
            assertion:
                JudgmentAssertion::Identity {
                    fact: identity::Fact::SameEvent { pair },
                },
            ..
        }) = fact
        {
            offending.insert((pair.a().clone(), pair.b().clone()));
        }
    }
    for (a, b) in offending {
        errors.push(SubmitError::SameEventUnresolvable { a, b });
    }
}

/// A name's validity window may not close before it opens. With both bounds
/// present, the earliest possible `valid_from` must not exceed the latest
/// possible `valid_to`. Equal is accepted; an open bound can't prove inversion.
fn rule_name_window<S: FactStore>(
    candidates: &[StoredFactOf<S>],
    errors: &mut Vec<SubmitError<EntityIdOf<S>, EventIdOf<S>, ImageIdOf<S>>>,
) {
    let mut offending: BTreeSet<EntityIdOf<S>> = BTreeSet::new();
    for fact in candidates {
        if let StoredFact::Factual(StoredFactualFact {
            assertion:
                FactualAssertion::Attribute {
                    fact:
                        attribute::Fact::Name {
                            entity,
                            valid_from: Some(vf),
                            valid_to: Some(vt),
                            ..
                        },
                },
            ..
        }) = fact
            && let (Some(from_start), Some(to_end)) = (vf.earliest(), vt.latest())
            && from_start > to_end
        {
            offending.insert(entity.clone());
        }
    }
    for entity in offending {
        errors.push(SubmitError::NameWindowInverted { entity });
    }
}

/// Every stored `UncertainDate` must be a single non-empty interval. We model
/// no use for a disjunction or ⊥ in a submission today, so we reject them to
/// keep the model simple and junk out of the store. This is a deliberately
/// strict guard: a real use case can relax it later with nothing breaking. The
/// traversal in [`for_each_stored_date`] currently covers every date position;
/// until it is macro-derived, a new date-bearing grammar variant must be added
/// there by hand.
fn rule_single_interval_date<S: FactStore>(
    candidates: &[StoredFactOf<S>],
    errors: &mut Vec<SubmitError<EntityIdOf<S>, EventIdOf<S>, ImageIdOf<S>>>,
) {
    for fact in candidates {
        for_each_stored_date(fact, &mut |role, date| {
            if date.as_single_interval().is_none() {
                errors.push(SubmitError::NonSingleIntervalDate { role });
            }
        });
    }
}

/// Visit every [`UncertainDate`] a stored fact reaches — fact-payload date
/// fields and every `ExternalSource` date across the three citation hosts —
/// tagging each with its [`DateRole`]. The one closed traversal the
/// single-interval rule walks; routing all `ExternalSource` hosts through
/// [`for_each_external_source_date`] keeps a future fourth host from
/// reintroducing the citation-date hole.
fn for_each_stored_date<R: IdScheme>(
    fact: &StoredFact<R>,
    visit: &mut impl FnMut(DateRole, &UncertainDate),
) {
    match fact {
        StoredFact::Factual(StoredFactualFact {
            assertion,
            citation,
        }) => {
            for_each_assertion_date(assertion, visit);
            let FactualCitation { source, .. } = citation;
            for_each_external_source_date(source, visit);
        }
        StoredFact::Judgment(StoredJudgmentFact { source, .. }) => {
            if let JudgmentSource::External { source } = source {
                for_each_external_source_date(source, visit);
            }
        }
        StoredFact::Meta(StoredMetaFact { source, .. }) => {
            if let MetaSource::External { source } = source {
                for_each_external_source_date(source, visit);
            }
        }
    }
}

/// Visit the date fields a factual assertion's payload carries. The event and
/// gap clusters carry no calendar dates except the dated event facts, so the
/// event arm reaches only those.
fn for_each_assertion_date<R: IdScheme>(
    assertion: &FactualAssertion<R>,
    visit: &mut impl FnMut(DateRole, &UncertainDate),
) {
    match assertion {
        FactualAssertion::Attribute {
            fact:
                attribute::Fact::Name {
                    valid_from,
                    valid_to,
                    ..
                },
        } => {
            if let Some(date) = valid_from {
                visit(DateRole::NameValidFrom, date);
            }
            if let Some(date) = valid_to {
                visit(DateRole::NameValidTo, date);
            }
        }
        FactualAssertion::Construction { fact } => match fact {
            bookend::ConstructionFact::Started { bound, .. }
            | bookend::ConstructionFact::Completed { bound, .. } => {
                visit(DateRole::BookendBound, bound);
            }
            bookend::ConstructionFact::Location { .. } => {}
        },
        FactualAssertion::Demolition { fact } => {
            let (bookend::DemolitionFact::Started { bound, .. }
            | bookend::DemolitionFact::Completed { bound, .. }) = fact;
            visit(DateRole::BookendBound, bound);
        }
        FactualAssertion::Existence { fact } => {
            visit(DateRole::ExistenceWitness, &fact.at);
        }
        FactualAssertion::Event { fact } => {
            if let event::Fact::PointDate { bound, .. }
            | event::Fact::DurationalDate { bound, .. } = fact
            {
                visit(DateRole::EventDate, bound);
            }
        }
        FactualAssertion::Image { fact } => match fact {
            image::Fact::CreatedDate { bound, .. } => visit(DateRole::ImageCreated, bound),
            image::Fact::CapturedDate { bound, .. } => visit(DateRole::ImageCaptured, bound),
            image::Fact::Source { .. }
            | image::Fact::Author { .. }
            | image::Fact::CapturedLocation { .. }
            | image::Fact::Medium { .. } => {}
        },
        FactualAssertion::Attribute { .. } | FactualAssertion::Gap { .. } => {}
    }
}

/// Visit the publication/creation date an `ExternalSource` carries, when one is
/// present. The structured variants pin their version instead of a date.
fn for_each_external_source_date(
    source: &ExternalSource,
    visit: &mut impl FnMut(DateRole, &UncertainDate),
) {
    let date = match source {
        ExternalSource::Url { published, .. } | ExternalSource::Book { published, .. } => {
            published.as_ref()
        }
        ExternalSource::Archive { created, .. } => created.as_ref(),
        ExternalSource::Wikidata { .. } | ExternalSource::Dbpedia { .. } => None,
    };
    if let Some(date) = date {
        visit(DateRole::CitationDate, date);
    }
}

/// Every stored location must name a place that could exist, named by a bounded
/// number of circles. Per location: count its circles first; if the count is
/// over [`MAX_LOCATION_CIRCLES`] reject it as [`LocationTooComplex`] and stop —
/// the emptiness check never runs on an over-cap location, so the cubic
/// candidate-point cost stays bounded by the cap that guards it. Otherwise its
/// [`conflict_status`](UnresolvedLocation::conflict_status) decides: a
/// [`Conflict`](ConflictStatus::Conflict) location is rejected as
/// [`EmptyLocation`] (the spatial parallel of [`rule_single_interval_date`]'s
/// empty-interval guard — no source asserts a place that is nowhere, whether a
/// syntactic ⊥ or a geometrically-disjoint conjunction), while a
/// [`Pending`](ConflictStatus::Pending) or
/// [`Consistent`](ConflictStatus::Consistent) one passes.
///
/// The cap counts the same circle leaves the emptiness check draws its
/// candidate points from ([`UnresolvedLocation::conflict_status`] gathers caps
/// over the permissive skeleton, whose circles are exactly this location's), so
/// the bound measures the cost it guards.
///
/// [`LocationTooComplex`]: SubmitError::LocationTooComplex
/// [`EmptyLocation`]: SubmitError::EmptyLocation
fn rule_location_validity<S: FactStore>(
    candidates: &[StoredFactOf<S>],
    errors: &mut Vec<SubmitError<EntityIdOf<S>, EventIdOf<S>, ImageIdOf<S>>>,
) {
    for fact in candidates {
        for_each_stored_location(fact, &mut |role, location| {
            let circles = location.circle_count();
            if circles > MAX_LOCATION_CIRCLES {
                errors.push(SubmitError::LocationTooComplex {
                    role,
                    circles,
                    limit: MAX_LOCATION_CIRCLES,
                });
            } else if location.conflict_status() == ConflictStatus::Conflict {
                errors.push(SubmitError::EmptyLocation { role });
            }
        });
    }
}

/// Visit every [`UnresolvedLocation`] a stored fact carries, tagging each with
/// its [`LocationRole`]. Only factual facts carry a location: a construction
/// bookend, a `Moved` event's destination, an image's capture place. Until this
/// is macro-derived, a new location-bearing grammar variant must be added here by
/// hand.
fn for_each_stored_location<R: IdScheme>(
    fact: &StoredFact<R>,
    visit: &mut impl FnMut(LocationRole, &UnresolvedLocation),
) {
    let StoredFact::Factual(StoredFactualFact { assertion, .. }) = fact else {
        return;
    };
    match assertion {
        FactualAssertion::Construction {
            fact: bookend::ConstructionFact::Location { location, .. },
        } => visit(LocationRole::BookendLocation, location),
        FactualAssertion::Event {
            fact: event::Fact::MovedToLocation { location, .. },
        } => visit(LocationRole::MovedToLocation, location),
        FactualAssertion::Image { fact } => match fact {
            image::Fact::CapturedLocation { location, .. } => {
                visit(LocationRole::ImageCaptured, location)
            }
            image::Fact::Source { .. }
            | image::Fact::Author { .. }
            | image::Fact::CreatedDate { .. }
            | image::Fact::CapturedDate { .. }
            | image::Fact::Medium { .. } => {}
        },
        FactualAssertion::Attribute { .. }
        | FactualAssertion::Construction { .. }
        | FactualAssertion::Demolition { .. }
        | FactualAssertion::Existence { .. }
        | FactualAssertion::Event { .. }
        | FactualAssertion::Gap { .. } => {}
    }
}

/// Every entity an image-observation names must have a depiction tying it to the
/// observed image, in-commit ∪ pre-commit. Only `ImageObservation`-cited
/// observations gate; the other warrant flavors carry no observed image. Pushes
/// dedup by `(entity, image)`.
fn rule_observation_depiction<S>(
    candidates: &[StoredFactOf<S>],
    image_facts: &HashMap<ImageIdOf<S>, Vec<StoredFactOf<S>>>,
    errors: &mut Vec<SubmitError<EntityIdOf<S>, EventIdOf<S>, ImageIdOf<S>>>,
) where
    S: FactStore,
{
    let mut offending: BTreeSet<(EntityIdOf<S>, ImageIdOf<S>)> = BTreeSet::new();
    for fact in candidates {
        let StoredFact::Judgment(StoredJudgmentFact {
            assertion: JudgmentAssertion::Observation { fact: obs },
            source: JudgmentSource::ImageObservation { image, .. },
        }) = fact
        else {
            continue;
        };
        let mut entities: BTreeSet<EntityIdOf<S>> = BTreeSet::new();
        obs.for_each_id(&mut |e: &EntityIdOf<S>| {
            entities.insert(e.clone());
        });
        let gathered = image_facts.get(image).map(Vec::as_slice).unwrap_or(&[]);
        for entity in entities {
            if !gathered
                .iter()
                .any(|g| depicts_entity_on_image(g, &entity, image))
            {
                offending.insert((entity, image.clone()));
            }
        }
    }
    for (entity, image) in offending {
        errors.push(SubmitError::ObservationWithoutDepiction { entity, image });
    }
}

/// Whether a stored fact is a depiction placing `entity` on `image`.
fn depicts_entity_on_image<R: IdScheme>(
    fact: &StoredFact<R>,
    entity: &R::Entity,
    image: &R::Image,
) -> bool {
    if let StoredFact::Judgment(StoredJudgmentFact {
        assertion: JudgmentAssertion::Depiction { fact: df },
        ..
    }) = fact
    {
        &df.entity == entity && &df.image == image
    } else {
        false
    }
}

/// A subimage may not be its own parent.
fn rule_composite_self_parent<S: FactStore>(
    candidates: &[StoredFactOf<S>],
    errors: &mut Vec<SubmitError<EntityIdOf<S>, EventIdOf<S>, ImageIdOf<S>>>,
) {
    let mut offending: BTreeSet<ImageIdOf<S>> = BTreeSet::new();
    for fact in candidates {
        if let StoredFact::Judgment(StoredJudgmentFact {
            assertion:
                JudgmentAssertion::Composite {
                    fact:
                        composites::Fact::IsSubimageOf {
                            subimage, parent, ..
                        },
                },
            ..
        }) = fact
            && subimage == parent
        {
            offending.insert(subimage.clone());
        }
    }
    for image in offending {
        errors.push(SubmitError::CompositeSelfParent { image });
    }
}

/// A subimage has at most one parent. Two `IsSubimageOf` facts naming the same
/// subimage under different parents (in-commit ∪ pre-commit) conflict.
fn rule_composite_multiple_parents<S>(
    candidates: &[StoredFactOf<S>],
    image_facts: &HashMap<ImageIdOf<S>, Vec<StoredFactOf<S>>>,
    errors: &mut Vec<SubmitError<EntityIdOf<S>, EventIdOf<S>, ImageIdOf<S>>>,
) where
    S: FactStore,
{
    let mut subimages: BTreeSet<ImageIdOf<S>> = BTreeSet::new();
    for fact in candidates {
        if let StoredFact::Judgment(StoredJudgmentFact {
            assertion:
                JudgmentAssertion::Composite {
                    fact: composites::Fact::IsSubimageOf { subimage, .. },
                },
            ..
        }) = fact
        {
            subimages.insert(subimage.clone());
        }
    }
    for s in subimages {
        let gathered = image_facts.get(&s).map(Vec::as_slice).unwrap_or(&[]);
        // A self-loop edge `{s, s}` isn't a genuine parent — the self-parent
        // check owns it — so `subimage_edge` filters it out before the count.
        let parents: BTreeSet<&ImageIdOf<S>> = gathered
            .iter()
            .filter_map(subimage_edge)
            .filter(|(subimage, _)| *subimage == &s)
            .map(|(_, parent)| parent)
            .collect();
        if parents.len() > 1 {
            errors.push(SubmitError::CompositeMultipleParents { subimage: s });
        }
    }
}

/// Composites are one layer deep. An in-commit `IsSubimageOf{s, p}` is a chain if
/// `s` is itself some image's parent, or `p` is itself some image's subimage
/// (in-commit ∪ pre-commit). Self-loop facts (`subimage == parent`) aren't chain
/// evidence — the self-parent check owns them — so they are skipped as triggers
/// and filtered from the gathered edges. Pushes dedup by image id.
fn rule_composite_chain<S>(
    candidates: &[StoredFactOf<S>],
    image_facts: &HashMap<ImageIdOf<S>, Vec<StoredFactOf<S>>>,
    errors: &mut Vec<SubmitError<EntityIdOf<S>, EventIdOf<S>, ImageIdOf<S>>>,
) where
    S: FactStore,
{
    let mut offending: BTreeSet<ImageIdOf<S>> = BTreeSet::new();
    for fact in candidates {
        let StoredFact::Judgment(StoredJudgmentFact {
            assertion:
                JudgmentAssertion::Composite {
                    fact:
                        composites::Fact::IsSubimageOf {
                            subimage: s,
                            parent: p,
                            ..
                        },
                },
            ..
        }) = fact
        else {
            continue;
        };
        if s == p {
            continue;
        }
        let about_s = image_facts.get(s).map(Vec::as_slice).unwrap_or(&[]);
        if about_s
            .iter()
            .filter_map(subimage_edge)
            .any(|(_, parent)| parent == s)
        {
            offending.insert(s.clone());
        }
        let about_p = image_facts.get(p).map(Vec::as_slice).unwrap_or(&[]);
        if about_p
            .iter()
            .filter_map(subimage_edge)
            .any(|(subimage, _)| subimage == p)
        {
            offending.insert(p.clone());
        }
    }
    for image in offending {
        errors.push(SubmitError::CompositeChain { image });
    }
}

/// The `(subimage, parent)` edge of an `IsSubimageOf` fact, or `None` when the
/// fact isn't one or is a self-loop. Self-loops carry no genuine parent/child
/// layering, so they don't count as chain evidence.
fn subimage_edge<R: IdScheme>(fact: &StoredFact<R>) -> Option<(&R::Image, &R::Image)> {
    if let StoredFact::Judgment(StoredJudgmentFact {
        assertion:
            JudgmentAssertion::Composite {
                fact:
                    composites::Fact::IsSubimageOf {
                        subimage, parent, ..
                    },
            },
        ..
    }) = fact
        && subimage != parent
    {
        Some((subimage, parent))
    } else {
        None
    }
}
