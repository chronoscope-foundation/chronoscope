//! In-memory fact-store backend.
//!
//! For tests and proptests. Not optimised — linear scans where the DB
//! backends use indexes. The same [`FactStore`] properties hold here.
//!
//! Surface:
//!
//! - `submit_commit` resolves [`Decl::Local`] via match-or-mint and passes
//!   [`Decl::Existing`] through.
//! - `fact()` returns `Active` / `Retracted` / `Future` / `Unknown`. A
//!   resolved slot reads `Retracted` when an effective retractor exists at the
//!   view's snapshot, resolved by scanning the fact bag.
//! - `next_fact_id` / `no_later_than` / `now` clock surface.
//! - The per-subject view methods (`walk_*`, `*_representative`, `*_class`,
//!   `*_subgraph`) are stubs: a subject is its own representative, its class a
//!   singleton, its subgraph a single node, the walks an empty page.
//!
//! ## Concrete id types
//!
//! `u64`-newtype ids — [`MemoryEntityId`], [`MemoryEventId`],
//! [`MemoryImageId`] — minted from per-store monotonic counters. The "known to
//! this store?" check is an integer comparison against the counter.
//!
//! ## Transaction model
//!
//! [`MemoryTx`] is a Send-able sentinel that stops a caller from interleaving
//! two `submit_commit` calls through one handle. The mutation runs under the
//! `Inner` mutex, acquired once at the start of `submit_commit` and held
//! across the whole match → mint → validate → insert sequence, so the reads
//! and the insert see one consistent snapshot. Re-acquiring mid-sequence would
//! let a writer land between the reads and the insert and violate a cross-fact
//! rule.
//!
//! The [`async_lock::Mutex`] guard is `Send`, so it spans the `.await`s on the
//! async matcher and validator (the pipeline takes `&V: FactView<S>` and
//! awaits, since a SQL backend awaits DB reads inside its own transaction).
//! Here those futures are always ready — the [`UnionSource`] accumulates mints
//! in owned state and the drain runs on the same task — so the held lock never
//! serialises real I/O.
//!
//! `Inner` is mutated only at [`apply_pending`], at the end of `submit_commit`,
//! under the held lock; everything before accumulates in the [`UnionSource`]'s
//! pending state. So dropping the [`MemoryTx`] without applying — a validator
//! rejection, an early return, a panic — leaves `Inner` untouched, with no
//! rollback path because nothing was mutated.

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::marker::PhantomData;

use async_lock::{Mutex, MutexGuard};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::facts::assertions::MetaAssertion;
use crate::facts::ids::{CommitId, FactId};
use crate::facts::schema::{
    EdgeSubgraph, EntityStream, EquivClass, EventStream, FactPage, ImageStream, PageItem,
};
use crate::facts::store::{
    EntityView, EventView, FactPlacement, FactStore, FactView, ImageView, StoredFactOf,
    SubmitCommitError,
};
use crate::facts::submit::pipeline::{
    self, MatchOutcome, substitute_facts_accumulating, validate_submit,
};
use crate::facts::submit::{
    Commit, Decl, EntityIdx, EventIdx, FactLookup, ImageIdx, Resolution, ResolutionOrigin,
    StoredCommit, StoredFact, SubjectKind, SubmitError, SubmitResult,
};
use crate::nonempty::NonEmptyVec;

// ============================================================================
// Concrete id types
// ============================================================================

/// In-memory entity id — a `u64` newtype minted from the store's entity
/// counter.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct MemoryEntityId(pub u64);

impl std::fmt::Display for MemoryEntityId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "entity-{}", self.0)
    }
}

/// In-memory lifetime-event id — a `u64` newtype minted from the store's
/// event counter.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct MemoryEventId(pub u64);

impl std::fmt::Display for MemoryEventId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "event-{}", self.0)
    }
}

/// In-memory image id — a `u64` newtype minted from the store's image
/// counter.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct MemoryImageId(pub u64);

impl std::fmt::Display for MemoryImageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "image-{}", self.0)
    }
}

// ============================================================================
// Backend error
// ============================================================================

/// In-memory backend errors that aren't submit-pipeline domain errors.
///
/// Domain errors (index out-of-range, rule violations, `Existing` decl
/// id-not-found) flow through
/// [`SubmitError`](crate::facts::submit::SubmitError) inside
/// [`SubmitCommitError::Submit`] instead.
///
/// Holds the message as a `String` because [`serde_json::Error`] isn't
/// `Eq`/`PartialEq` and this type is value-compared in tests; the `From` impl
/// preserves the rendering.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("commit hash encoding failed: {0}")]
pub struct MemoryError(String);

impl From<serde_json::Error> for MemoryError {
    fn from(e: serde_json::Error) -> Self {
        Self(e.to_string())
    }
}

// Aliases to keep the spellings short.
type MemStoredFact = StoredFact<MemoryEntityId, MemoryEventId, MemoryImageId>;
type MemFactLookup = FactLookup<MemoryEntityId, MemoryEventId, MemoryImageId>;
type MemCommit = Commit<MemoryEntityId, MemoryEventId, MemoryImageId>;
type MemSubmitResult = SubmitResult<MemoryEntityId, MemoryEventId, MemoryImageId>;
type MemSubmitCommitError =
    SubmitCommitError<MemoryError, MemoryEntityId, MemoryEventId, MemoryImageId>;

// ============================================================================
// Storage
// ============================================================================

/// Internal storage, guarded by the [`async_lock::Mutex`] on
/// [`MemoryFactStore`].
#[derive(Debug, Default)]
struct Inner {
    /// The fact bag. `facts[i]` is `FactId(i)`; the first fact is
    /// `FactId(0)`.
    facts: Vec<MemStoredFact>,
    /// Which commit each fact belongs to. Parallel to [`Self::facts`].
    fact_commits: Vec<CommitId>,
    commits: HashMap<CommitId, StoredCommit>,
    /// Reverse retraction index: each retracted fact's id → the ids of the
    /// meta-facts retracting it. Grown as facts land in [`apply_pending`], so a
    /// read consults it without rebuilding; resolution filters by snapshot.
    retractors: HashMap<FactId, Vec<FactId>>,
    /// Backlink indexes: each subject id → the ids of facts mentioning it.
    /// Grown alongside [`Self::retractors`] in [`apply_pending`]. The value is a
    /// [`BTreeSet`] so the ids stay sorted and deduped, and a paginated read
    /// seeks to its cursor with `range(cursor..)` instead of skipping the
    /// prefix. (The retractor index keeps a `Vec` — it is fully iterated with no
    /// cursor seek, so a sorted set buys it nothing.)
    entity_backlinks: HashMap<MemoryEntityId, BTreeSet<FactId>>,
    event_backlinks: HashMap<MemoryEventId, BTreeSet<FactId>>,
    image_backlinks: HashMap<MemoryImageId, BTreeSet<FactId>>,
    /// Content-addressed dedup cache: the `SubmitResult` from the first
    /// successful submit of a `CommitId`. A re-submit returns this with
    /// `previously_committed: true` and does no mints or inserts.
    submit_results: HashMap<CommitId, MemSubmitResult>,
    /// Next id to mint. The "known" predicate is `id.0 < next_*_id`.
    next_entity_id: u64,
    next_event_id: u64,
    next_image_id: u64,
}

impl Inner {
    /// The next [`FactId`] this store would mint — one past the highest stored
    /// fact, or `FactId::new(0)` on an empty store.
    ///
    /// `as u64` is lossless: a `usize` length fits in `u64` under the crate's
    /// `size_of::<usize>() <= size_of::<u64>()` invariant.
    fn next_fact_id(&self) -> FactId {
        FactId::new(self.facts.len() as u64)
    }
}

// ============================================================================
// MemoryFactStore
// ============================================================================

/// In-memory implementation of [`FactStore`].
///
/// State lives behind one [`async_lock::Mutex`] so the store presents `&self`
/// to async readers. Writers hold the lock across the whole `submit_commit`
/// mint → validate → insert sequence, mirroring the SQL backends'
/// single-transaction shape. The guard is `Send`, so it spans the `.await`s on
/// the async matcher / validator.
#[derive(Debug, Default)]
pub struct MemoryFactStore {
    inner: Mutex<Inner>,
}

impl MemoryFactStore {
    /// Construct an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Acquire the inner mutex. async-lock doesn't poison — a panic under the
    /// guard releases the lock. The state is append-only (counter advances +
    /// monotonic fact-vec growth), so whatever survives a panic is a valid
    /// committed prefix.
    async fn lock_inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().await
    }
}

// ============================================================================
// ReadCore — borrowed read surface (the only home for lookup logic)
// ============================================================================

/// Borrowed read surface over a committed fact slice plus an optional pending
/// slice, with a frozen exclusive-upper-bound snapshot.
///
/// The single home for the lookup logic [`MemorySource`] and [`UnionSource`]
/// share. A source lends a `ReadCore` for one lookup via
/// [`CoreSource::with_core`]; the slices are valid only inside that closure.
///
/// `pending` is empty for committed-only sources and non-empty while a
/// `submit_commit` accumulates in-flight facts.
struct ReadCore<'a> {
    committed: &'a [MemStoredFact],
    pending: &'a [MemStoredFact],
    /// The committed commit ids. Existence is checked against the committed set
    /// only — a retraction targets a commit that already landed, and in-flight
    /// commit metadata doesn't exist until apply. Mirrors how a `FactId`
    /// retraction target resolves against the committed snapshot.
    committed_commits: &'a HashMap<CommitId, StoredCommit>,
    /// Reverse retraction index over committed facts (see
    /// [`Inner::retractors`]), borrowed for the lookup; resolution filters its
    /// entries by snapshot.
    retractors: &'a HashMap<FactId, Vec<FactId>>,
    /// In-flight reverse-retraction edges from a commit still being validated
    /// (see [`UnionSource::pending_retractors`]); `None` for a committed-only
    /// source. [`Self::retracted_by`] unions these with [`Self::retractors`] so
    /// a same-commit retraction of a pre-commit fact is visible to the reads
    /// the cluster rules drive.
    pending_retractors: Option<&'a HashMap<FactId, Vec<FactId>>>,
    /// Backlink indexes over committed facts (see [`Inner::entity_backlinks`]),
    /// borrowed for the paginated `all_facts_about_*` reads.
    entity_backlinks: &'a HashMap<MemoryEntityId, BTreeSet<FactId>>,
    event_backlinks: &'a HashMap<MemoryEventId, BTreeSet<FactId>>,
    image_backlinks: &'a HashMap<MemoryImageId, BTreeSet<FactId>>,
    /// In-flight backlink edges from a commit still being validated (see
    /// [`UnionSource`]'s `pending_*_backlinks`); `None` for a committed-only
    /// source. [`Self::facts_about`] chains the pending range onto the committed
    /// one so `all_facts_about_*` returns committed ∪ pending.
    pending_entity_backlinks: Option<&'a HashMap<MemoryEntityId, BTreeSet<FactId>>>,
    pending_event_backlinks: Option<&'a HashMap<MemoryEventId, BTreeSet<FactId>>>,
    pending_image_backlinks: Option<&'a HashMap<MemoryImageId, BTreeSet<FactId>>>,
    /// Exclusive upper bound: ids `>= snapshot` read as [`FactLookup::Future`].
    snapshot: FactId,
}

/// Where a [`FactId`] lands in a [`ReadCore`]'s committed/pending partition,
/// before any snapshot filtering: an index into `committed`, an offset into
/// `pending`, or past both halves.
enum Partition {
    Committed(usize),
    Pending(usize),
    Beyond,
}

impl<'a> ReadCore<'a> {
    /// Look up a fact across the committed + pending halves.
    ///
    /// - `fact_id >= snapshot` → [`FactLookup::Future`].
    /// - `fact_id < committed.len()` → `committed`.
    /// - otherwise → `pending` at offset `fact_id - committed.len()`.
    ///
    /// A resolved slot reads `Retracted` when an effective retractor exists at
    /// this snapshot (see [`Self::retracted_by`]), else `Active`. A
    /// missing-but-expected slot is `Unknown` (a shape bug, not a domain
    /// outcome).
    fn fact_at(&self, fact_id: FactId) -> MemFactLookup {
        if fact_id.get() >= self.snapshot.get() {
            return FactLookup::Future;
        }
        match self.fact_slot(fact_id) {
            Some(fact) => self.resolve_active_or_retracted(fact_id, fact),
            None => FactLookup::Unknown,
        }
    }

    /// Which half of the committed/pending partition `id` lands in, ignoring the
    /// snapshot. The committed half is `0..committed.len()`; the pending half the
    /// `pending.len()` slots above it; anything higher — or an id too large for
    /// `usize` — is [`Partition::Beyond`]. [`Self::fact_slot`] and
    /// [`Self::placement_at`] share this one boundary computation.
    fn locate(&self, id: FactId) -> Partition {
        let committed_len = self.committed.len() as u64;
        if id.get() < committed_len {
            match usize::try_from(id.get()) {
                Ok(i) => Partition::Committed(i),
                Err(_) => Partition::Beyond,
            }
        } else {
            match usize::try_from(id.get() - committed_len) {
                Ok(off) if off < self.pending.len() => Partition::Pending(off),
                _ => Partition::Beyond,
            }
        }
    }

    /// The stored fact at `fact_id`, reading the committed half below
    /// `committed.len()` and the pending half above it; `None` for an id past
    /// both halves or one that doesn't fit a `usize`. The slot resolver behind
    /// [`Self::fact_at`] and [`Self::facts_about`], with no snapshot filtering —
    /// callers gate on the snapshot first.
    fn fact_slot(&self, fact_id: FactId) -> Option<&MemStoredFact> {
        match self.locate(fact_id) {
            Partition::Committed(i) => self.committed.get(i),
            Partition::Pending(off) => self.pending.get(off),
            Partition::Beyond => None,
        }
    }

    /// A resolved slot: `Retracted` if an effective retractor exists at this
    /// snapshot, else `Active`.
    fn resolve_active_or_retracted(&self, fact_id: FactId, fact: &MemStoredFact) -> MemFactLookup {
        match self.retracted_by(fact_id) {
            Some(by) => FactLookup::Retracted { by },
            None => FactLookup::Active(Box::new(fact.clone())),
        }
    }

    /// The lowest [`FactId`] effectively retracting `fact_id` at this snapshot,
    /// or `None`.
    ///
    /// Walks only the subgraph bearing on `fact_id` — the meta-facts retracting
    /// it, the ones retracting those, and so on — not the whole index. Submit
    /// validation forces every retractor's id above its target's, so the walk
    /// climbs strictly (no cycle) and resolves high-to-low, each retractor's
    /// status known before the fact it retracts. A fact is retracted by the
    /// lowest of its retractors that is visible at this snapshot and not itself
    /// effectively retracted.
    fn retracted_by(&self, fact_id: FactId) -> Option<FactId> {
        // No retractor edge in either source → active. The `?` returns None now,
        // before the walk below allocates its frontier and subgraph.
        self.retractor_ids(fact_id).next()?;
        // Collect the facts reachable upward from `fact_id` through visible
        // retractor edges. Edges climb in id, so the frontier drains and a
        // shared retractor is collected once.
        let mut subgraph: BTreeSet<FactId> = BTreeSet::new();
        let mut frontier = vec![fact_id];
        while let Some(id) = frontier.pop() {
            if !subgraph.insert(id) {
                continue;
            }
            frontier.extend(
                self.retractor_ids(id)
                    .filter(|candidate| candidate.get() < self.snapshot.get()),
            );
        }
        // Resolve the subgraph high-to-low: each retractor resolves before the
        // fact it retracts.
        let mut retracted: HashMap<FactId, Option<FactId>> = HashMap::new();
        for &id in subgraph.iter().rev() {
            let by = self
                .retractor_ids(id)
                .filter(|candidate| candidate.get() < self.snapshot.get())
                .filter(|candidate| retracted.get(candidate).copied().flatten().is_none())
                .min();
            retracted.insert(id, by);
        }
        retracted.get(&fact_id).copied().flatten()
    }

    /// Every fact id retracting `id`, unioning the committed index with the
    /// in-flight pending overlay. A fact can be retracted by a pre-commit
    /// meta-fact (committed) and an in-commit one (pending) at once, so both
    /// sources are walked.
    fn retractor_ids(&self, id: FactId) -> impl Iterator<Item = FactId> + '_ {
        let committed = self.retractors.get(&id).into_iter().flatten();
        let pending = self
            .pending_retractors
            .and_then(|m| m.get(&id))
            .into_iter()
            .flatten();
        committed.chain(pending).copied()
    }

    /// Whether `id` names a committed commit; see [`Self::committed_commits`].
    /// A retracted commit is still present (retraction records a meta-fact, it
    /// doesn't erase the commit), so it reports as existing.
    fn commit_known_at(&self, id: &CommitId) -> bool {
        self.committed_commits.contains_key(id)
    }

    /// Where `id` sits relative to the snapshot: at-or-past the snapshot is
    /// [`FactPlacement::Absent`], a pending provisional id is
    /// [`FactPlacement::InFlight`], a committed id is
    /// [`FactPlacement::Committed`], and an id past both halves is
    /// [`FactPlacement::Absent`]. `InFlight` is bounded by the real
    /// `pending.len()`, so a committed-only source (empty `pending`) reports
    /// only `Committed` / `Absent`.
    fn placement_at(&self, id: FactId) -> FactPlacement {
        if id.get() >= self.snapshot.get() {
            FactPlacement::Absent
        } else {
            match self.locate(id) {
                Partition::Committed(_) => FactPlacement::Committed,
                Partition::Pending(_) => FactPlacement::InFlight,
                Partition::Beyond => FactPlacement::Absent,
            }
        }
    }

    /// A page of the facts mentioning `subject`, active and below the snapshot,
    /// starting at the pagination `cursor`. The one paginator behind every
    /// `all_facts_about_*` read — pass the matching committed and pending
    /// `*_backlinks` maps and subject id.
    ///
    /// The subject is its own `representative` — backlinks are literal, not
    /// scoped to an equivalence class. Both backlink sets are
    /// sorted, and committed fids (`0..committed.len()`) sit wholly below pending
    /// provisional fids (`committed.len()..`), so chaining the committed range
    /// onto the pending range walks one ascending stream. `range(cursor..)` on
    /// each seeks past already-paged facts and the snapshot bound breaks the scan
    /// as soon as it is crossed. A committed-only source passes `None` for the
    /// pending map, degrading to a committed-only walk.
    ///
    /// `next_cursor` is `Some(fid)` at the id where the scan stopped while more
    /// eligible ids remain — whether the page filled or the snapshot bound was
    /// hit mid-range — and `None` once the backlink range is exhausted. A
    /// retracted fact is skipped without consuming a slot, so a page can come
    /// back short or empty yet still carry a resume cursor.
    fn facts_about<S>(
        &self,
        backlinks: &HashMap<S, BTreeSet<FactId>>,
        pending_backlinks: Option<&HashMap<S, BTreeSet<FactId>>>,
        subject: &S,
        cursor: FactId,
        limit: std::num::NonZeroUsize,
    ) -> FactPage<MemStoredFact, S>
    where
        S: Copy + Ord + std::hash::Hash,
    {
        let committed = backlinks
            .get(subject)
            .into_iter()
            .flat_map(|ids| ids.range(cursor..));
        let pending = pending_backlinks
            .and_then(|m| m.get(subject))
            .into_iter()
            .flat_map(|ids| ids.range(cursor..));
        let mut items = Vec::new();
        let mut next_cursor = None;
        for &fid in committed.chain(pending) {
            if fid.get() >= self.snapshot.get() {
                break;
            }
            if items.len() == limit.get() {
                next_cursor = Some(fid);
                break;
            }
            if self.retracted_by(fid).is_some() {
                continue;
            }
            let Some(fact) = self.fact_slot(fid) else {
                continue;
            };
            items.push(PageItem {
                fact_id: fid,
                fact: fact.clone(),
                representative: *subject,
            });
        }
        FactPage { items, next_cursor }
    }
}

/// "Lend me a [`ReadCore`]" — abstracts where the committed + pending slices
/// come from. [`MemorySource`] locks the store per call; [`UnionSource`] hands
/// back its owned pending state alongside the already-held `Inner` borrow
/// without awaiting. The view-trait impls hang off this via blanket impls, so
/// any `CoreSource` is a read view.
///
/// `with_core` is async so a source that has to lock can. It spells the
/// store-trait `fn -> impl Future + Send` convention to keep `Send` explicit.
/// [`Self::snapshot`] is sync — neither source locks to answer it.
trait CoreSource {
    fn with_core<R: Send>(
        &self,
        f: impl FnOnce(&ReadCore<'_>) -> R + Send,
    ) -> impl Future<Output = R> + Send;
    fn snapshot(&self) -> FactId;
}

// ============================================================================
// MemorySource — committed-only source over a borrowed store
// ============================================================================

/// Snapshot-scoped source over a [`MemoryFactStore`].
///
/// Holds a store borrow plus a frozen exclusive-upper-bound snapshot; every
/// lookup is filtered to `id < snapshot()`. Awaits the inner mutex per
/// [`CoreSource::with_core`] call (committed state only — no pending). The
/// snapshot is stored, so [`CoreSource::snapshot`] reads it lock-free.
#[derive(Debug)]
pub struct MemorySource<'a> {
    store: &'a MemoryFactStore,
    snapshot: FactId,
}

impl CoreSource for MemorySource<'_> {
    async fn with_core<R: Send>(&self, f: impl FnOnce(&ReadCore<'_>) -> R + Send) -> R {
        let inner = self.store.lock_inner().await;
        f(&ReadCore {
            committed: &inner.facts,
            pending: &[],
            committed_commits: &inner.commits,
            retractors: &inner.retractors,
            pending_retractors: None,
            entity_backlinks: &inner.entity_backlinks,
            event_backlinks: &inner.event_backlinks,
            image_backlinks: &inner.image_backlinks,
            pending_entity_backlinks: None,
            pending_event_backlinks: None,
            pending_image_backlinks: None,
            snapshot: self.snapshot,
        })
    }
    fn snapshot(&self) -> FactId {
        self.snapshot
    }
}

// ============================================================================
// UnionSource — in-flight accumulator that unions committed + pending state
// ============================================================================

/// The complete delta a `submit_commit` call adds: the in-flight facts, the
/// mint-advanced next-id counters, and the backlink / reverse-retraction edges
/// those facts contribute. The provisional fact ids the edges reference equal
/// the durable ids [`apply_pending`] will assign, so the same structure serves
/// the in-flight reads during validation and the durable splice at apply.
///
/// Held in a [`UnionSource`] while the commit is prepared, yielded by
/// [`UnionSource::into_pending`], spliced into `Inner` via [`apply_pending`].
/// Carrying the counters and edges alongside the facts means a validator
/// failure drops the [`Pending`] without touching `Inner` — rollback is
/// implicit in not applying it.
struct Pending {
    facts: Vec<MemStoredFact>,
    next_entity_id: u64,
    next_event_id: u64,
    next_image_id: u64,
    /// Backlink edges this commit's facts contribute, keyed by subject id. Each
    /// fact's provisional fid lands at `committed.len() + pending.len()`, above
    /// every committed fid and below the snapshot, so the paginated scan and
    /// snapshot filter accept it during validation; [`apply_pending`] merges
    /// these into the durable indexes unchanged.
    entity_backlinks: HashMap<MemoryEntityId, BTreeSet<FactId>>,
    event_backlinks: HashMap<MemoryEventId, BTreeSet<FactId>>,
    image_backlinks: HashMap<MemoryImageId, BTreeSet<FactId>>,
    /// Reverse-retraction edges this commit's pending meta-facts contribute,
    /// keyed by retracted fact id. A read unions this with the committed
    /// [`Inner::retractors`], so a commit that retracts a pre-commit fact and
    /// then adds facts depending on its absence validates against the
    /// post-retraction state. Provisional retractor ids climb above every
    /// pre-commit target and below the snapshot, so the high-to-low retraction
    /// walk and snapshot filter accept them.
    retractors: HashMap<FactId, Vec<FactId>>,
}

/// Source that unions committed `Inner` storage with the in-flight
/// [`Pending`] delta of a `submit_commit` call. The single read surface
/// across the match → mint → validate → insert sequence.
///
/// Borrowed via [`UnionSource::from_inner`] while the inner mutex is held.
/// Reads see committed facts plus any pushed onto `pending.facts`, and the
/// committed indexes unioned with `pending`'s edge maps; mints and pushes
/// accumulate in the owned `pending`, leaving `Inner` untouched.
/// [`UnionSource::into_pending`] yields that delta at the end, and the caller
/// splices it into `Inner` via [`apply_pending`].
///
/// The in-flight facts, counters, and edges all live in `pending`, so there's
/// one owner of the mutation state. Rollback on validation failure is implicit:
/// drop the source without calling `apply_pending`.
///
/// `'g` is the `Inner` borrow — the held guard's deref lifetime.
struct UnionSource<'g> {
    committed: &'g Inner,
    pending: Pending,
}

impl<'g> UnionSource<'g> {
    /// Construct a [`UnionSource`] from the held `Inner` guard. Seeds the
    /// pending counters from the committed ones; starts with no pending facts
    /// or edges.
    fn from_inner(inner: &'g Inner) -> Self {
        Self {
            committed: inner,
            pending: Pending {
                facts: Vec::new(),
                next_entity_id: inner.next_entity_id,
                next_event_id: inner.next_event_id,
                next_image_id: inner.next_image_id,
                entity_backlinks: HashMap::new(),
                event_backlinks: HashMap::new(),
                image_backlinks: HashMap::new(),
                retractors: HashMap::new(),
            },
        }
    }

    /// True if `id` is below the current pending counter — committed or
    /// minted into this source's in-flight state.
    fn entity_known(&self, id: &MemoryEntityId) -> bool {
        id.0 < self.pending.next_entity_id
    }

    fn event_known(&self, id: &MemoryEventId) -> bool {
        id.0 < self.pending.next_event_id
    }

    fn image_known(&self, id: &MemoryImageId) -> bool {
        id.0 < self.pending.next_image_id
    }

    /// Mint a fresh entity id from the in-flight counter, bumping it. `Inner`
    /// is untouched — if `into_pending` is never called, the mint is dropped
    /// with the rest of the pending state.
    fn mint_entity(&mut self) -> MemoryEntityId {
        let id = MemoryEntityId(self.pending.next_entity_id);
        self.pending.next_entity_id = self.pending.next_entity_id.saturating_add(1);
        id
    }

    fn mint_event(&mut self) -> MemoryEventId {
        let id = MemoryEventId(self.pending.next_event_id);
        self.pending.next_event_id = self.pending.next_event_id.saturating_add(1);
        id
    }

    fn mint_image(&mut self) -> MemoryImageId {
        let id = MemoryImageId(self.pending.next_image_id);
        self.pending.next_image_id = self.pending.next_image_id.saturating_add(1);
        id
    }

    /// Stage a fact onto the pending list, fixing its position. The durable
    /// [`FactId`] is assigned later by [`apply_pending`], which appends pending
    /// facts in push order.
    ///
    /// Every staged fact records its provisional backlink edges into the
    /// `pending_*_backlinks` maps, and a staged meta-fact also records its
    /// reverse-retraction edges into [`Self::pending_retractors`], so an
    /// in-flight fact is visible to the cluster-rule reads that follow it in the
    /// same commit. The provisional id matches the slot `apply_pending` will
    /// assign — the pre-push total of committed plus pending facts. A
    /// `RetractCommit` expands over the committed commit's fact ids; an in-flight
    /// commit isn't recorded yet, so it contributes no retractor edges (and an
    /// in-commit fact can't be retracted in-commit — `MetaTargetInSameCommit`
    /// forbids it).
    fn push_fact(&mut self, fact: MemStoredFact) {
        // The staged fact takes the union snapshot — the next id to assign, which
        // `apply_pending` then gives it.
        let provisional_id = CoreSource::snapshot(self);
        record_backlink_edges(
            &fact,
            provisional_id,
            &mut self.pending.entity_backlinks,
            &mut self.pending.event_backlinks,
            &mut self.pending.image_backlinks,
        );
        if let StoredFact::Meta(meta) = &fact {
            record_retractor_edges(
                &self.committed.commits,
                &mut self.pending.retractors,
                &meta.assertion,
                provisional_id,
            );
        }
        self.pending.facts.push(fact);
    }

    /// Consume the source and yield the [`Pending`] payload, releasing the
    /// `&'g Inner` borrow so the caller can take `&mut Inner` and drain it via
    /// [`apply_pending`].
    fn into_pending(self) -> Pending {
        self.pending
    }
}

impl CoreSource for UnionSource<'_> {
    // Borrows the held `&Inner` plus the owned pending vec — nothing to
    // lock or await, but `async` to match the trait.
    async fn with_core<R: Send>(&self, f: impl FnOnce(&ReadCore<'_>) -> R + Send) -> R {
        // UFCS: `self` also has a blanket `FactView::snapshot`, so a bare
        // call would be ambiguous.
        f(&ReadCore {
            committed: &self.committed.facts,
            pending: &self.pending.facts,
            committed_commits: &self.committed.commits,
            retractors: &self.committed.retractors,
            pending_retractors: Some(&self.pending.retractors),
            entity_backlinks: &self.committed.entity_backlinks,
            event_backlinks: &self.committed.event_backlinks,
            image_backlinks: &self.committed.image_backlinks,
            pending_entity_backlinks: Some(&self.pending.entity_backlinks),
            pending_event_backlinks: Some(&self.pending.event_backlinks),
            pending_image_backlinks: Some(&self.pending.image_backlinks),
            snapshot: CoreSource::snapshot(self),
        })
    }

    /// The next id to be assigned — `committed.len() + pending.len()` as a
    /// [`FactId`] — so `fact_id < snapshot()` admits every visible fact,
    /// committed and pending, not just committed.
    fn snapshot(&self) -> FactId {
        FactId::new(
            (self.committed.facts.len() as u64).saturating_add(self.pending.facts.len() as u64),
        )
    }
}

// ============================================================================
// View-trait impls — carried directly on any CoreSource
// ============================================================================
//
// The view traits and the `CoreSource` sources are all local to this crate, so
// the impls hang off `Src: CoreSource` via blanket impls, parameterised by
// `MemoryFactStore`. Both `MemorySource` and `UnionSource` gain the read
// surface with no wrapper type, and the lookup logic lives once in `ReadCore`.
// The submit pipeline reads through a `&V: FactView<S>` to an owned source, so
// only the blanket impl on `Src` is exercised. The id kinds and error type
// come from `MemoryFactStore`, so the bodies name `Memory*Id` / `MemoryError`
// directly.

impl<Src: CoreSource + Send + Sync> FactView<MemoryFactStore> for Src {
    fn snapshot(&self) -> FactId {
        CoreSource::snapshot(self)
    }

    async fn fact(&self, fact_id: FactId) -> Result<MemFactLookup, MemoryError> {
        Ok(self.with_core(|core| core.fact_at(fact_id)).await)
    }

    async fn commit_known(&self, id: &CommitId) -> Result<bool, MemoryError> {
        Ok(self.with_core(|core| core.commit_known_at(id)).await)
    }

    async fn placement(&self, id: FactId) -> Result<FactPlacement, MemoryError> {
        Ok(self.with_core(|core| core.placement_at(id)).await)
    }
}

// ============================================================================
// EntityView / EventView / ImageView impls
// ============================================================================
//
// The subject-parametric bodies stay inline in the trait impls rather than
// on `ReadCore`: an inherent method ignoring `self` trips
// `clippy::unused_self`, but a trait-impl method doesn't.

impl<Src: CoreSource + Send + Sync> EntityView<MemoryFactStore> for Src {
    async fn entity_representative(
        &self,
        member: &MemoryEntityId,
    ) -> Result<MemoryEntityId, MemoryError> {
        Ok(*member)
    }

    async fn entity_class(
        &self,
        member: &MemoryEntityId,
    ) -> Result<EquivClass<MemoryEntityId>, MemoryError> {
        Ok(EquivClass {
            representative: *member,
            members: std::iter::once(*member).collect(),
        })
    }

    async fn walk_entities<'b>(
        &'b self,
        _stream: &'b EntityStream<'b>,
        _cursor: FactId,
        _limit: std::num::NonZeroUsize,
    ) -> Result<FactPage<StoredFactOf<MemoryFactStore>, MemoryEntityId>, MemoryError> {
        Ok(FactPage {
            items: Vec::new(),
            next_cursor: None,
        })
    }

    async fn all_facts_about_entity(
        &self,
        entity: &MemoryEntityId,
        cursor: FactId,
        limit: std::num::NonZeroUsize,
    ) -> Result<FactPage<StoredFactOf<MemoryFactStore>, MemoryEntityId>, MemoryError> {
        Ok(self
            .with_core(|c| {
                c.facts_about(
                    c.entity_backlinks,
                    c.pending_entity_backlinks,
                    entity,
                    cursor,
                    limit,
                )
            })
            .await)
    }

    async fn entity_topological_subgraph(
        &self,
        seed: MemoryEntityId,
        _cursor: FactId,
        _limit: std::num::NonZeroUsize,
    ) -> Result<EdgeSubgraph<MemoryEntityId, StoredFactOf<MemoryFactStore>>, MemoryError> {
        Ok(EdgeSubgraph {
            subjects: vec![seed],
            edge_facts: Vec::new(),
            truncated: false,
        })
    }
}

impl<Src: CoreSource + Send + Sync> EventView<MemoryFactStore> for Src {
    async fn event_representative(
        &self,
        member: &MemoryEventId,
    ) -> Result<MemoryEventId, MemoryError> {
        Ok(*member)
    }

    async fn event_class(
        &self,
        member: &MemoryEventId,
    ) -> Result<EquivClass<MemoryEventId>, MemoryError> {
        Ok(EquivClass {
            representative: *member,
            members: std::iter::once(*member).collect(),
        })
    }

    async fn walk_events<'b>(
        &'b self,
        _stream: &'b EventStream<'b>,
        _cursor: FactId,
        _limit: std::num::NonZeroUsize,
    ) -> Result<FactPage<StoredFactOf<MemoryFactStore>, MemoryEventId>, MemoryError> {
        Ok(FactPage {
            items: Vec::new(),
            next_cursor: None,
        })
    }

    async fn all_facts_about_event(
        &self,
        event: &MemoryEventId,
        cursor: FactId,
        limit: std::num::NonZeroUsize,
    ) -> Result<FactPage<StoredFactOf<MemoryFactStore>, MemoryEventId>, MemoryError> {
        Ok(self
            .with_core(|c| {
                c.facts_about(
                    c.event_backlinks,
                    c.pending_event_backlinks,
                    event,
                    cursor,
                    limit,
                )
            })
            .await)
    }
}

impl<Src: CoreSource + Send + Sync> ImageView<MemoryFactStore> for Src {
    async fn image_representative(
        &self,
        member: &MemoryImageId,
    ) -> Result<MemoryImageId, MemoryError> {
        Ok(*member)
    }

    async fn image_class(
        &self,
        member: &MemoryImageId,
    ) -> Result<EquivClass<MemoryImageId>, MemoryError> {
        Ok(EquivClass {
            representative: *member,
            members: std::iter::once(*member).collect(),
        })
    }

    async fn walk_images<'b>(
        &'b self,
        _stream: &'b ImageStream<'b>,
        _cursor: FactId,
        _limit: std::num::NonZeroUsize,
    ) -> Result<FactPage<StoredFactOf<MemoryFactStore>, MemoryImageId>, MemoryError> {
        Ok(FactPage {
            items: Vec::new(),
            next_cursor: None,
        })
    }

    async fn all_facts_about_image(
        &self,
        image: &MemoryImageId,
        cursor: FactId,
        limit: std::num::NonZeroUsize,
    ) -> Result<FactPage<StoredFactOf<MemoryFactStore>, MemoryImageId>, MemoryError> {
        Ok(self
            .with_core(|c| {
                c.facts_about(
                    c.image_backlinks,
                    c.pending_image_backlinks,
                    image,
                    cursor,
                    limit,
                )
            })
            .await)
    }
}

/// Splice a [`Pending`] delta into `Inner`, assigning each fact its durable
/// [`FactId`] and merging the delta's precomputed edge maps into the durable
/// indexes.
///
/// Appends each fact in push order (the first at the current
/// `Inner::facts.len()`), records the parallel `fact_commits`, and collects the
/// assigned ids to return. The provisional ids the delta's edges reference equal
/// these durable ids, so the backlink and reverse-retraction maps merge in
/// wholesale — `push_fact` already built them at push, sparing a per-fact
/// rebuild here. The sole assigner of fact ids; the union source's staging only
/// fixed the order.
fn apply_pending(inner: &mut Inner, pending: Pending, commit_id: CommitId) -> Vec<FactId> {
    let mut assigned = Vec::with_capacity(pending.facts.len());
    for fact in pending.facts {
        let id = inner.next_fact_id();
        inner.facts.push(fact);
        inner.fact_commits.push(commit_id.clone());
        assigned.push(id);
    }
    merge_index(&mut inner.entity_backlinks, pending.entity_backlinks);
    merge_index(&mut inner.event_backlinks, pending.event_backlinks);
    merge_index(&mut inner.image_backlinks, pending.image_backlinks);
    merge_index(&mut inner.retractors, pending.retractors);
    inner.next_entity_id = pending.next_entity_id;
    inner.next_event_id = pending.next_event_id;
    inner.next_image_id = pending.next_image_id;
    assigned
}

/// Add a delta's per-subject id lists into a durable index, extending each
/// subject's existing entry with the incoming ids.
fn merge_index<K, C, V>(dst: &mut HashMap<K, C>, src: HashMap<K, C>)
where
    K: Eq + std::hash::Hash,
    C: Default + Extend<V> + IntoIterator<Item = V>,
{
    for (subject, ids) in src {
        dst.entry(subject).or_default().extend(ids);
    }
}

/// The edge-recording logic shared by the committed index and the in-flight
/// pending overlay — both reached through [`UnionSource::push_fact`]. A
/// fact-target assertion maps its target to `retractor_id`; a commit-target
/// expands over the named commit's fact ids read from `commits`. A
/// `RetractCommit` against a commit absent from `commits` records nothing —
/// in-flight commit metadata doesn't exist until apply, mirroring the
/// committed-only existence check.
fn record_retractor_edges(
    commits: &HashMap<CommitId, StoredCommit>,
    retractors: &mut HashMap<FactId, Vec<FactId>>,
    assertion: &MetaAssertion,
    retractor_id: FactId,
) {
    match assertion {
        MetaAssertion::RetractFact { target, .. } | MetaAssertion::SupersedeFact { target, .. } => {
            retractors.entry(*target).or_default().push(retractor_id);
        }
        MetaAssertion::RetractCommit { target, .. } => {
            if let Some(commit) = commits.get(target) {
                for retracted in &commit.fact_ids {
                    retractors.entry(*retracted).or_default().push(retractor_id);
                }
            }
        }
    }
}

/// Record the backlink edges a fact at `id` contributes: `id` is inserted into
/// the sorted set of every entity / event / image id the fact mentions. A fact
/// mentioning one id twice records it once — `id` is constant and the
/// destination is a set, so the repeated insert is idempotent. Meta facts
/// mention no subjects.
///
/// Called from [`UnionSource::push_fact`] at the provisional fid the push
/// assigns; [`apply_pending`] then merges the resulting maps into the durable
/// indexes, where the provisional fid equals the durable one.
fn record_backlink_edges(
    fact: &MemStoredFact,
    id: FactId,
    entity_backlinks: &mut HashMap<MemoryEntityId, BTreeSet<FactId>>,
    event_backlinks: &mut HashMap<MemoryEventId, BTreeSet<FactId>>,
    image_backlinks: &mut HashMap<MemoryImageId, BTreeSet<FactId>>,
) {
    fact.for_each_id(
        &mut |e| {
            entity_backlinks.entry(*e).or_default().insert(id);
        },
        &mut |v| {
            event_backlinks.entry(*v).or_default().insert(id);
        },
        &mut |i| {
            image_backlinks.entry(*i).or_default().insert(id);
        },
    );
}

// ============================================================================
// FactStore impl
// ============================================================================

/// Branded transaction handle for [`MemoryFactStore`].
///
/// A `Send`-able zero-sized sentinel with an invariant `'brand` marker, distinct
/// across [`MemoryFactStore::with_tx`] calls so a tx from one closure can't
/// reach another store's closure.
///
/// `PhantomData<fn(&'brand ()) -> &'brand ()>` is invariant in `'brand`, which
/// the brand pattern needs; a covariant or contravariant marker would let two
/// closures' brands unify.
///
/// The backend acquires its `Mutex` inside each
/// [`MemoryFactStore::submit_commit`] call and holds it across the whole
/// sequence; `MemoryTx` keeps the trait shape uniform and stops concurrent
/// `submit_commit` calls racing through one handle. A SQL backend puts a real
/// `sqlx::Transaction` here.
///
/// Cross-instance misuse is a compile error — see
/// `CrossInstanceBrandIsCompileError` below for the `compile_fail` doctest, and
/// [`tests::two_commits_share_one_with_tx_brand`] for the positive intra-store
/// check.
pub struct MemoryTx<'brand> {
    _brand: PhantomData<fn(&'brand ()) -> &'brand ()>,
}

// `PhantomData<fn(&'brand ()) -> &'brand ()>` is `Send + Sync` (fn pointers
// are) and carries no runtime data, so the handle is thread-safe.

impl FactStore for MemoryFactStore {
    type Error = MemoryError;
    type EntityId = MemoryEntityId;
    type EventId = MemoryEventId;
    type ImageId = MemoryImageId;
    type Tx<'brand> = MemoryTx<'brand>;
    type View<'a> = MemorySource<'a>;

    async fn with_tx<F, R>(&self, f: F) -> Result<R, Self::Error>
    where
        F: for<'brand> FnOnce(
                &'brand Self,
                &'brand mut Self::Tx<'brand>,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = R> + Send + 'brand>,
            > + Send,
        R: Send,
    {
        // Fresh-branded Tx; the `for<'brand>` bound lets the closure pick the
        // brand. No I/O — the transactional state is the `Mutex` acquired
        // inside `submit_commit`.
        let mut tx: MemoryTx<'_> = MemoryTx {
            _brand: PhantomData,
        };
        let result = f(self, &mut tx).await;
        // `submit_commit` mutates `Inner` only at `apply_pending`; anything it
        // abandons partway stays in the discarded `UnionSource`. `Inner`
        // reflects exactly the commits that completed, so dropping `tx` has
        // nothing to roll back.
        Ok(result)
    }

    async fn submit_commit<'brand, 'tx>(
        &'tx self,
        _tx: &'tx mut Self::Tx<'brand>,
        commit: MemCommit,
    ) -> Result<MemSubmitResult, MemSubmitCommitError>
    where
        Self: 'brand,
        'brand: 'tx,
    {
        let commit_id = commit
            .id()
            .map_err(|e| SubmitCommitError::Backend(e.into()))?;

        // Hold the lock across the whole sequence so the reads and the insert see
        // one snapshot. The `Send` guard spans the `.await`s below; the matcher
        // and validator resolve immediately, so the lock never serialises I/O.
        let mut inner_guard = self.lock_inner().await;

        // A re-submit of a known `CommitId` returns the cached result with
        // `previously_committed: true`, doing no mints, inserts, or rule
        // re-evaluation.
        if let Some(cached) = inner_guard.submit_results.get(&commit_id) {
            return Ok(SubmitResult {
                previously_committed: true,
                ..cached.clone()
            });
        }

        // All reads / mints / pushes go through the union source, so `Inner` is
        // touched only at apply.
        let mut source = UnionSource::from_inner(&inner_guard);

        // Reject unresolvable references before minting, so a malformed bundle
        // burns no ids. A fact pointing at a missing declaration can't resolve, so
        // collect every out-of-range index and every unknown `Decl::Existing` id
        // into one batch.
        let (entity_refs, event_refs, image_refs) = collect_idx_refs(&commit);
        let mut resolvability_errors: Vec<
            SubmitError<MemoryEntityId, MemoryEventId, MemoryImageId>,
        > = Vec::new();
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
        for (i, decl) in commit.entities.iter().enumerate() {
            if let Decl::Existing { id } = decl
                && !source.entity_known(id)
            {
                resolvability_errors.push(SubmitError::UnknownExistingEntity {
                    decl_position: EntityIdx(i),
                });
            }
        }
        for (i, decl) in commit.events.iter().enumerate() {
            if let Decl::Existing { id } = decl
                && !source.event_known(id)
            {
                resolvability_errors.push(SubmitError::UnknownExistingEvent {
                    decl_position: EventIdx(i),
                });
            }
        }
        for (i, decl) in commit.images.iter().enumerate() {
            if let Decl::Existing { id } = decl
                && !source.image_known(id)
            {
                resolvability_errors.push(SubmitError::UnknownExistingImage {
                    decl_position: ImageIdx(i),
                });
            }
        }
        if let Ok(batch) = NonEmptyVec::try_from_vec(resolvability_errors) {
            return Err(SubmitCommitError::Submit(batch));
        }

        // Resolve each Local decl to a match or a mint. Events are never matched,
        // so each Local event decl gets a synthesised mint outcome. The `&source`
        // borrows end before the `&mut source` mints below.
        let entity_outcomes =
            pipeline::match_entities(&commit.entities, &commit.facts, &source).await;
        let image_outcomes = pipeline::match_images(&commit.images, &commit.facts, &source).await;
        let event_outcomes: HashMap<EventIdx, MatchOutcome<MemoryEventId>> = commit
            .events
            .iter()
            .enumerate()
            .filter(|(_, d)| matches!(d, Decl::Local))
            .map(|(i, _)| {
                (
                    EventIdx(i),
                    MatchOutcome::Mint {
                        candidates: Vec::new(),
                    },
                )
            })
            .collect();

        // Mints land in the source's pending counters; `Inner` stays untouched.
        let entity_resolutions = resolve_decls(
            &commit.entities,
            &entity_outcomes,
            EntityIdx,
            |s| s.mint_entity(),
            &mut source,
        );
        let event_resolutions = resolve_decls(
            &commit.events,
            &event_outcomes,
            EventIdx,
            |s| s.mint_event(),
            &mut source,
        );
        let image_resolutions = resolve_decls(
            &commit.images,
            &image_outcomes,
            ImageIdx,
            |s| s.mint_image(),
            &mut source,
        );

        // Build substitution maps over the resolved ids.
        let entity_sub_map: HashMap<EntityIdx, MemoryEntityId> = entity_resolutions
            .iter()
            .map(|(idx, res)| (*idx, res.id))
            .collect();
        let event_sub_map: HashMap<EventIdx, MemoryEventId> = event_resolutions
            .iter()
            .map(|(idx, res)| (*idx, res.id))
            .collect();
        let image_sub_map: HashMap<ImageIdx, MemoryImageId> = image_resolutions
            .iter()
            .map(|(idx, res)| (*idx, res.id))
            .collect();

        // Substitute, accumulating: the reject batch begins with any substitution
        // self-loops, and successfully substituted facts proceed to the rules.
        let (stored_facts, mut validation_errors) = substitute_facts_accumulating(
            &commit.facts,
            &entity_sub_map,
            &event_sub_map,
            &image_sub_map,
        );

        // Stage the substituted facts onto the pending list, fixing their order;
        // `apply_pending` assigns their FactIds at drain.
        for fact in &stored_facts {
            source.push_fact(fact.clone());
        }

        // Unused declarations are checked post-mint, but the mint rollback is
        // implicit: a non-empty reject batch means apply never runs, so the
        // source's mints drop and no counter is burned.
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

        // Two declarations of one kind resolving to the same persistent id is
        // non-canonical: it breaks content-address dedup, and a self-pair would
        // otherwise slip through. One error per duplicated id.
        validation_errors.extend(
            duplicate_resolved_ids(&entity_resolutions)
                .into_iter()
                .map(|id| SubmitError::DuplicateEntityDecl { id }),
        );
        validation_errors.extend(
            duplicate_resolved_ids(&event_resolutions)
                .into_iter()
                .map(|id| SubmitError::DuplicateEventDecl { id }),
        );
        validation_errors.extend(
            duplicate_resolved_ids(&image_resolutions)
                .into_iter()
                .map(|id| SubmitError::DuplicateImageDecl { id }),
        );

        // Run the rule validator (meta + cluster rules) over the source, folding
        // its batch in. Only a backend read failure short-circuits; a rule
        // violation joins the batch.
        validation_errors.extend(
            validate_submit(&stored_facts, &source)
                .await
                .map_err(SubmitCommitError::Backend)?,
        );

        // Reject the whole batch if any rule fired. Drop the source without
        // applying — the mints roll back implicitly.
        if let Ok(batch) = NonEmptyVec::try_from_vec(validation_errors) {
            return Err(SubmitCommitError::Submit(batch));
        }

        // `apply_pending` is the sole assigner of the FactIds, returning them in
        // push order.
        let pending = source.into_pending();
        let assigned_fact_ids = apply_pending(&mut inner_guard, pending, commit_id.clone());

        inner_guard.commits.insert(
            commit_id.clone(),
            StoredCommit {
                commit_id: commit_id.clone(),
                author: commit.author.clone(),
                recorded_at: commit.recorded_at,
                fact_ids: assigned_fact_ids.clone(),
            },
        );

        let result = SubmitResult {
            commit_id,
            previously_committed: false,
            fact_ids: assigned_fact_ids,
            entities: entity_resolutions,
            events: event_resolutions,
            images: image_resolutions,
        };

        // Cache the result for idempotent re-submission.
        inner_guard
            .submit_results
            .insert(result.commit_id.clone(), result.clone());

        Ok(result)
    }

    async fn next_fact_id(&self) -> Result<FactId, Self::Error> {
        Ok(self.lock_inner().await.next_fact_id())
    }

    fn no_later_than(&self, snapshot: FactId) -> Self::View<'_> {
        MemorySource {
            store: self,
            snapshot,
        }
    }

    async fn now(&self) -> Result<Self::View<'_>, Self::Error> {
        let snap = self.lock_inner().await.next_fact_id();
        Ok(MemorySource {
            store: self,
            snapshot: snap,
        })
    }
}

// ============================================================================
// Decl resolution (match-or-mint, single pass per id kind)
// ============================================================================

/// Resolve every declaration of one kind to a persistent id.
///
/// [`Decl::Existing(id)`] passes through as `DeclaredExisting`. For
/// [`Decl::Local`] the matcher outcome at the matching `Idx` selects an adopted
/// match (`MatchedExisting`, no mint) or a fresh mint (empty vs non-empty
/// candidates distinguishing `NewlyMinted` from `Ambiguous`).
///
/// Matcher contract: one entry per `Decl::Local` keyed by `Idx`, none for
/// `Decl::Existing`. A missing `Local` entry is treated as a no-match rather
/// than failing the commit. Surplus entries can't arise — `match_entities` /
/// `match_images` only emit in-range positions.
fn resolve_decls<Id, Idx, M>(
    decls: &[Decl<Id>],
    outcomes: &HashMap<Idx, MatchOutcome<Id>>,
    idx_ctor: fn(usize) -> Idx,
    mut mint: M,
    source: &mut UnionSource<'_>,
) -> HashMap<Idx, Resolution<Id>>
where
    Id: Clone,
    Idx: Copy + Eq + std::hash::Hash,
    M: FnMut(&mut UnionSource<'_>) -> Id,
{
    let mut out = HashMap::with_capacity(decls.len());
    for (i, decl) in decls.iter().enumerate() {
        let idx = idx_ctor(i);
        let resolution = match decl {
            Decl::Existing { id } => Resolution {
                id: id.clone(),
                origin: ResolutionOrigin::DeclaredExisting,
            },
            Decl::Local => match outcomes.get(&idx) {
                Some(MatchOutcome::Matched(id)) => Resolution {
                    id: id.clone(),
                    origin: ResolutionOrigin::MatchedExisting,
                },
                // A missing Local outcome is treated as
                // `Mint { candidates: vec![] }`: mint fresh with no
                // candidates rather than drop the decl and corrupt the map.
                Some(MatchOutcome::Mint { candidates }) => {
                    let id = mint(source);
                    let origin = NonEmptyVec::try_from_vec(candidates.clone()).map_or(
                        ResolutionOrigin::NewlyMinted,
                        |nonempty| ResolutionOrigin::Ambiguous {
                            candidates: nonempty,
                        },
                    );
                    Resolution { id, origin }
                }
                None => {
                    let id = mint(source);
                    Resolution {
                        id,
                        origin: ResolutionOrigin::NewlyMinted,
                    }
                }
            },
        };
        out.insert(idx, resolution);
    }
    out
}

/// Collect the bundle-local indices every fact references, bucketed by kind.
/// `Meta` targets are persistent `FactId` / `CommitId`, not indices, so they
/// contribute none. The collector half of the id-traversal; the resulting sets
/// feed both the out-of-range check and the unused-declaration check.
fn collect_idx_refs<EntId, EvtId, ImgId>(
    commit: &Commit<EntId, EvtId, ImgId>,
) -> (
    std::collections::HashSet<EntityIdx>,
    std::collections::HashSet<EventIdx>,
    std::collections::HashSet<ImageIdx>,
) {
    use std::collections::HashSet;

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

/// Every reference past the end of its kind's declaration list, one
/// [`SubmitError`] per offending position in ascending order. Parameterised over
/// the reference set, declaration count, the `position` extractor, and the
/// out-of-range error constructor.
fn check_refs_in_range<Idx, EntId, EvtId, ImgId>(
    refs: &std::collections::HashSet<Idx>,
    decl_count: usize,
    position: impl Fn(&Idx) -> usize,
    out_of_range: impl Fn(usize, usize) -> SubmitError<EntId, EvtId, ImgId>,
) -> Vec<SubmitError<EntId, EvtId, ImgId>> {
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

/// Every declaration position with no incoming reference, one [`SubmitError`]
/// per position in ascending order. A decl with no fact under it is almost
/// certainly a bug, better surfaced than silently minted. Parameterised over the
/// reference set, declaration count, the `idx_ctor` for testing set membership,
/// and the unreferenced-position error constructor.
fn check_all_decls_referenced<Idx, EntId, EvtId, ImgId>(
    refs: &std::collections::HashSet<Idx>,
    decl_count: usize,
    idx_ctor: fn(usize) -> Idx,
    unused: impl Fn(usize) -> SubmitError<EntId, EvtId, ImgId>,
) -> Vec<SubmitError<EntId, EvtId, ImgId>>
where
    Idx: Eq + std::hash::Hash,
{
    (0..decl_count)
        .filter(|i| !refs.contains(&idx_ctor(*i)))
        .map(unused)
        .collect()
}

/// The persistent ids that more than one declaration resolved to, sorted and
/// deduped. Empty when every declaration of the kind resolved to a distinct id.
/// The order is deterministic so a rejected commit's batch is reproducible.
fn duplicate_resolved_ids<Idx, Id>(resolutions: &HashMap<Idx, Resolution<Id>>) -> Vec<Id>
where
    Id: Clone + Ord,
{
    let mut seen: BTreeSet<Id> = BTreeSet::new();
    let mut duplicated: BTreeSet<Id> = BTreeSet::new();
    for res in resolutions.values() {
        if !seen.insert(res.id.clone()) {
            duplicated.insert(res.id.clone());
        }
    }
    duplicated.into_iter().collect()
}

#[cfg(test)]
mod tests;

/// Negative compile-time check for cross-instance brand misuse.
///
/// A `Tx<'brand>` from one `with_tx` call can't reach another store's `with_tx`
/// closure: each body introduces a fresh `'brand` existential, and
/// `MemoryTx<'brand>`'s invariant `PhantomData` keeps the two from unifying
/// even for the same backend type.
///
/// ```compile_fail
/// use chronoscope_core::facts::memory::MemoryFactStore;
/// use chronoscope_core::facts::store::FactStore;
///
/// async fn smuggle_tx_across_stores() {
///     let store_a = MemoryFactStore::new();
///     let store_b = MemoryFactStore::new();
///     let _ = store_a
///         .with_tx(|_s_a, tx_a| {
///             Box::pin(async move {
///                 // Leaking tx_a into store_b's closure: rejected, the
///                 // two brands are distinct existentials.
///                 let _ = store_b
///                     .with_tx(|_s_b, _tx_b| {
///                         Box::pin(async move {
///                             let _smuggled = tx_a;
///                         })
///                     })
///                     .await;
///             })
///         })
///         .await;
/// }
/// ```
#[cfg(doctest)]
struct CrossInstanceBrandIsCompileError;
