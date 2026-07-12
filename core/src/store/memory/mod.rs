//! In-memory fact-store backend.
//!
//! For tests and proptests. Aims to be simple and correct, holding the same
//! [`FactStore`] properties as the DB backends.
//!
//! Surface:
//!
//! - `submit_commit` is the provided [`FactStore`] method — the shared
//!   driver — running on the [`FactWrite`] primitives [`MemoryTx`]
//!   implements.
//! - `fact()` returns `Active` / `Retracted` / `Future` / `Unknown`. A
//!   resolved slot reads `Retracted` when an effective retractor exists at the
//!   view's snapshot, resolved by scanning the fact bag.
//! - `next_fact_id` / `no_later_than` / `now` clock surface.
//! - Per-subject view reads: `all_facts_about_*` page the backlink indexes;
//!   the entity / image `*_representative` / `*_class` reads union-find over
//!   the visible `SameEntity` / `SameArtifact` facts; `walk_entity_classes`
//!   over `ByName` / `ByExternalReference` / `All` and `walk_image_classes`
//!   over `BySourceUrl` / `All` scan the fact bag into `(representative,
//!   fact_id)` rows.
//!
//! ## Concrete id types
//!
//! `u64`-newtype ids — [`MemoryEntityId`], [`MemoryEventId`],
//! [`MemoryImageId`] — minted from per-store monotonic counters. The "known to
//! this store?" check is an integer comparison against the counter.
//!
//! ## Transaction model
//!
//! `with_tx` holds the `Inner` mutex guard for the whole closure — the
//! writer serialisation, mirroring a SQL write lock. [`MemoryTx`] sees the
//! committed state under that guard plus the [`Pending`] overlay of
//! everything the transaction staged: facts, minted counters, backlink /
//! reverse-retraction edges, and the metadata and cached results of commits
//! recorded so far. Reads through the handle union the two, so the match →
//! mint → validate → stage sequence sees one consistent snapshot that
//! includes its own earlier commits.
//!
//! The [`async_lock::Mutex`] guard is `Send`, so it spans the `.await`s on
//! the async matcher and validator (the pipeline takes `&mut V: FactView<S>`
//! and awaits, since a SQL backend awaits DB reads inside its own
//! transaction). Here those futures are always ready — the overlay is owned
//! state read on the same task — so the held lock never serialises real I/O.
//!
//! `Inner` is mutated only at [`apply_pending`], once per transaction, when
//! the `with_tx` closure returns `Ok`. A closure that returns `Err` or
//! panics drops the [`MemoryTx`] — overlay and all — leaving `Inner`
//! untouched, with no rollback path because nothing was mutated. The
//! transaction is the unit of atomicity: a multi-commit closure that fails
//! partway applies none of its commits. The overlay can't unwind one
//! submit's slice, so a failed [`FactWrite::with_submit_scope`] poisons the
//! transaction: the write primitives and the apply refuse, and a closure
//! that swallows the rejection can't commit its leftovers.

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::marker::PhantomData;
use std::ops::Bound;

use async_lock::{Mutex, MutexGuard};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::grammar::assertions::MetaAssertion;
use crate::grammar::ids::{CommitId, FactId, IdScheme};
use crate::store::retraction::RetractionEdges;
use crate::store::schema::{
    ClassPage, EntityStream, EquivClass, FactPage, ImageStream, PageItem, normalize_name,
};
use crate::store::{
    ClassWalkPage, DepictionWalkPage, EntityView, EventView, FactPlacement, FactStore, FactView,
    FactWrite, ImageView, StoredFactOf,
};
use crate::submit::{FactLookup, StoredCommit, StoredFact, SubmitResult};

mod equiv;
mod scan;

use self::scan::{
    entity_ids_of, entity_in_viewport, entity_named, entity_referenced, image_captured_in_viewport,
    image_ids_of, image_sourced_from, same_artifact_edge, same_entity_edge,
};

// ============================================================================
// Concrete id types
// ============================================================================

/// Define a `u64`-newtype backend id.
///
/// `Copy`/`Ord`/`Hash` for use as an index and map key, a `<prefix>-N` `Display`
/// for logs, and a wire form that renders the `u64` as a decimal *string* (via
/// [`Serializer::collect_str`](serde::Serializer::collect_str)). Serializing the
/// key as a string keeps the read wire's id a `string` for every backend, so no
/// consumer bakes in an integer-shaped id. `Deserialize` parses that string back
/// to the `u64`; the `JsonSchema` is a non-referenceable bare `string`, so the
/// backend's id type name never reaches the OpenAPI spec.
macro_rules! memory_id {
    ($name:ident, $prefix:literal, $doc:expr) => {
        #[doc = $doc]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub u64);

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, concat!($prefix, "-{}"), self.0)
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let s = String::deserialize(deserializer)?;
                s.parse::<u64>().map(Self).map_err(|e| {
                    serde::de::Error::custom(format!("invalid {} {s:?}: {e}", stringify!($name)))
                })
            }
        }

        impl JsonSchema for $name {
            fn schema_name() -> String {
                <String as JsonSchema>::schema_name()
            }

            fn json_schema(
                generator: &mut schemars::r#gen::SchemaGenerator,
            ) -> schemars::schema::Schema {
                <String as JsonSchema>::json_schema(generator)
            }

            fn is_referenceable() -> bool {
                false
            }
        }
    };
}

memory_id!(
    MemoryEntityId,
    "entity",
    "In-memory entity id — a `u64` newtype minted from the store's entity counter."
);
memory_id!(
    MemoryEventId,
    "event",
    "In-memory lifetime-event id — a `u64` newtype minted from the store's event counter."
);
memory_id!(
    MemoryImageId,
    "image",
    "In-memory image id — a `u64` newtype minted from the store's image counter."
);

/// The in-memory backend's id scheme: the three `u64`-newtype id kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, JsonSchema)]
pub struct MemoryIds;

impl IdScheme for MemoryIds {
    type Entity = MemoryEntityId;
    type Event = MemoryEventId;
    type Image = MemoryImageId;
}

// ============================================================================
// Backend error
// ============================================================================

/// In-memory backend errors that aren't submit-pipeline domain errors.
///
/// Domain errors (index out-of-range, rule violations, `Existing` decl
/// id-not-found) flow through
/// [`SubmitError`](crate::submit::SubmitError) inside
/// [`SubmitCommitError::Submit`](crate::store::SubmitCommitError::Submit)
/// instead.
///
/// Holds the rendered message as a `String`; each construction site supplies
/// its context, and the type stays value-comparable for tests.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct MemoryError(String);

// Aliases to keep the spellings short.
type MemStoredFact = StoredFact<MemoryIds>;
type MemFactLookup = FactLookup<MemoryIds>;
type MemSubmitResult = SubmitResult<MemoryIds>;

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
    /// resumes with an exclusive `range` past its cursor instead of skipping the
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
/// to async readers. A writer holds the lock across the whole `with_tx`
/// closure, mirroring the SQL backends' single-transaction shape. The guard
/// is `Send`, so it spans the `.await`s on the async matcher / validator.
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
/// The single home for the lookup logic [`MemorySource`] and [`MemoryTx`]
/// share. A source lends a `ReadCore` for one lookup via
/// [`CoreSource::with_core`]; the slices are valid only inside that closure.
///
/// `pending` is empty for committed-only sources and holds the transaction's
/// staged facts for a [`MemoryTx`].
struct ReadCore<'a> {
    committed: &'a [MemStoredFact],
    pending: &'a [MemStoredFact],
    /// The pending facts covered by commits already recorded in the
    /// transaction — the committed/in-flight boundary [`Self::placement_at`]
    /// reports. `None` for a committed-only source.
    recorded_pending: Option<&'a BTreeSet<FactId>>,
    /// The committed commit ids; a `FactId` retraction target resolves against
    /// the same committed snapshot.
    committed_commits: &'a HashMap<CommitId, StoredCommit>,
    /// Commits recorded earlier in the transaction; `None` for a
    /// committed-only source. [`Self::commit_known_at`] unions these with
    /// [`Self::committed_commits`], so a commit landed earlier in the same
    /// transaction is a valid retraction target.
    pending_commits: Option<&'a HashMap<CommitId, StoredCommit>>,
    /// Reverse retraction index over committed facts (see
    /// [`Inner::retractors`]), borrowed for the lookup; resolution filters its
    /// entries by snapshot.
    retractors: &'a HashMap<FactId, Vec<FactId>>,
    /// In-flight reverse-retraction edges from the transaction's staged
    /// meta-facts (see [`Pending::retractors`]); `None` for a committed-only
    /// source. [`Self::retracted_by`] unions these with [`Self::retractors`] so
    /// a same-commit retraction of a pre-commit fact is visible to the reads
    /// the cluster rules drive.
    pending_retractors: Option<&'a HashMap<FactId, Vec<FactId>>>,
    /// Backlink indexes over committed facts (see [`Inner::entity_backlinks`]),
    /// borrowed for the paginated `all_facts_about_*` reads.
    entity_backlinks: &'a HashMap<MemoryEntityId, BTreeSet<FactId>>,
    event_backlinks: &'a HashMap<MemoryEventId, BTreeSet<FactId>>,
    image_backlinks: &'a HashMap<MemoryImageId, BTreeSet<FactId>>,
    /// In-flight backlink edges from the transaction's staged facts (see
    /// [`Pending`]'s `*_backlinks`); `None` for a committed-only source.
    /// [`Self::facts_about`] chains the pending range onto the committed
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
            Some(fact) => match self.retracted_by(fact_id) {
                Some(by) => FactLookup::Retracted { by },
                None => FactLookup::Active(Box::new(fact.clone())),
            },
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

    /// Every fact id retracting `id`, unioning the committed index with the
    /// in-flight pending overlay. A fact can be retracted by a pre-commit
    /// meta-fact (committed) and an in-commit one (pending) at once, so both
    /// indexes contribute.
    fn retractors_of(&self, id: FactId) -> impl Iterator<Item = FactId> + '_ {
        let committed = self.retractors.get(&id).into_iter().flatten();
        let pending = self
            .pending_retractors
            .and_then(|m| m.get(&id))
            .into_iter()
            .flatten();
        committed.chain(pending).copied()
    }

    /// The retractor edges bearing on `fact_id`: a frontier walk over the
    /// index maps materializing only the reachable subgraph — the same
    /// closure the SQLite backend's recursive CTE fetches.
    fn retraction_edges_for(&self, fact_id: FactId) -> RetractionEdges {
        let mut edges: Vec<(FactId, FactId)> = Vec::new();
        let mut visited: BTreeSet<FactId> = BTreeSet::new();
        let mut frontier = vec![fact_id];
        while let Some(id) = frontier.pop() {
            if !visited.insert(id) {
                continue;
            }
            for retractor in self.retractors_of(id) {
                edges.push((id, retractor));
                frontier.push(retractor);
            }
        }
        RetractionEdges::from_edges(edges)
    }

    /// The lowest [`FactId`] effectively retracting `fact_id` at this snapshot,
    /// or `None` — the shared fixpoint
    /// ([`retraction::effective_retractor`](crate::store::retraction::effective_retractor))
    /// over the subgraph from [`Self::retraction_edges_for`].
    fn retracted_by(&self, fact_id: FactId) -> Option<FactId> {
        // Most facts have no retractor at all; answer without materializing.
        self.retractors_of(fact_id).next()?;
        let edges = self.retraction_edges_for(fact_id);
        crate::store::retraction::effective_retractor(fact_id, self.snapshot, &edges)
    }

    /// Whether `id` names a recorded commit — committed, or recorded earlier
    /// in this transaction. A retracted commit is still present (retraction
    /// records a meta-fact, it doesn't erase the commit), so it reports as
    /// existing. The commit currently being validated is never in either map,
    /// so a commit can't retract itself.
    fn commit_known_at(&self, id: &CommitId) -> bool {
        self.committed_commits.contains_key(id)
            || self.pending_commits.is_some_and(|m| m.contains_key(id))
    }

    /// Where `id` sits relative to the snapshot: at-or-past the snapshot is
    /// [`FactPlacement::Absent`], an id staged by a commit still in flight is
    /// [`FactPlacement::InFlight`], a committed id is
    /// [`FactPlacement::Committed`], and an id past both halves is
    /// [`FactPlacement::Absent`]. Pending facts in [`Self::recorded_pending`]
    /// belong to commits recorded earlier in the transaction — committed for
    /// the meta-target rules, which only fence off ids whose commit hasn't
    /// recorded. A committed-only source (empty `pending`) reports only
    /// `Committed` / `Absent`.
    fn placement_at(&self, id: FactId) -> FactPlacement {
        if id.get() >= self.snapshot.get() {
            FactPlacement::Absent
        } else {
            match self.locate(id) {
                Partition::Committed(_) => FactPlacement::Committed,
                Partition::Pending(_) => {
                    if self.recorded_pending.is_some_and(|s| s.contains(&id)) {
                        FactPlacement::Committed
                    } else {
                        FactPlacement::InFlight
                    }
                }
                Partition::Beyond => FactPlacement::Absent,
            }
        }
    }

    /// A page of the facts mentioning `subject`, active and below the snapshot,
    /// resuming strictly past `after` (`None` opens the walk). The one paginator
    /// behind every `all_facts_about_*` read — pass the matching committed and
    /// pending `*_backlinks` maps and subject id.
    ///
    /// The subject is its own `representative` — backlinks are literal, not
    /// scoped to an equivalence class. Both backlink sets are
    /// sorted, and committed fids (`0..committed.len()`) sit wholly below pending
    /// provisional fids (`committed.len()..`), so chaining the committed range
    /// onto the pending range walks one ascending stream. The exclusive `range`
    /// on each seeks past already-paged facts and the snapshot bound breaks the
    /// scan as soon as it is crossed. A committed-only source passes `None` for
    /// the pending map, degrading to a committed-only walk.
    ///
    /// `next_cursor` is `Some(last-emitted id)` when the page fills before the
    /// range runs out, so the next walk resumes strictly past it; `None` once
    /// the backlink range is exhausted or the snapshot bound ends the scan. A
    /// retracted fact is skipped without consuming a slot, so a page can come
    /// back short or empty yet still carry a resume cursor.
    fn facts_about<S>(
        &self,
        backlinks: &HashMap<S, BTreeSet<FactId>>,
        pending_backlinks: Option<&HashMap<S, BTreeSet<FactId>>>,
        subject: &S,
        after: Option<FactId>,
        limit: std::num::NonZeroUsize,
    ) -> FactPage<MemStoredFact, S, FactId>
    where
        S: Copy + Ord + std::hash::Hash,
    {
        let lower = after.map_or(Bound::Unbounded, Bound::Excluded);
        let committed = backlinks
            .get(subject)
            .into_iter()
            .flat_map(|ids| ids.range((lower, Bound::Unbounded)));
        let pending = pending_backlinks
            .and_then(|m| m.get(subject))
            .into_iter()
            .flat_map(|ids| ids.range((lower, Bound::Unbounded)));
        let mut items = Vec::new();
        let mut last_emitted: Option<FactId> = None;
        let mut next_cursor = None;
        for &fid in committed.chain(pending) {
            if fid.get() >= self.snapshot.get() {
                break;
            }
            if items.len() == limit.get() {
                next_cursor = last_emitted;
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
            last_emitted = Some(fid);
        }
        FactPage { items, next_cursor }
    }

    /// Every fact below the snapshot with its id — the committed half then the
    /// pending half, ids ascending.
    ///
    /// `i as u64` is lossless: a `usize` index fits in `u64` under the crate's
    /// `size_of::<usize>() <= size_of::<u64>()` invariant.
    fn visible_facts(&self) -> impl Iterator<Item = (FactId, &MemStoredFact)> {
        self.committed
            .iter()
            .chain(self.pending.iter())
            .enumerate()
            .map(|(i, fact)| (FactId::new(i as u64), fact))
            .take_while(|(fid, _)| fid.get() < self.snapshot.get())
    }
}

/// "Lend me a [`ReadCore`]" — abstracts where the committed + pending slices
/// come from. [`MemorySource`] locks the store per call; [`MemoryTx`] hands
/// back its owned pending state alongside the already-held guard without
/// awaiting. The view-trait impls hang off this via blanket impls, so any
/// `CoreSource` is a read view.
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
            recorded_pending: None,
            committed_commits: &inner.commits,
            pending_commits: None,
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
// MemoryTx — the transaction: committed state + pending overlay
// ============================================================================

/// The delta a transaction accumulates: staged facts, the mint-advanced
/// next-id counters, the backlink / reverse-retraction edges those facts
/// contribute, and the metadata plus cached results of commits recorded so
/// far. The provisional fact ids the edges reference equal the durable ids
/// [`apply_pending`] will assign, so the same structure serves in-transaction
/// reads and the durable splice at apply.
///
/// Held on [`MemoryTx`] while the transaction runs, spliced into `Inner` via
/// [`apply_pending`] when the `with_tx` closure returns `Ok`. Carrying
/// everything the transaction changed means a failed closure drops the
/// [`Pending`] without touching `Inner` — rollback is implicit in not
/// applying it.
struct Pending {
    facts: Vec<MemStoredFact>,
    /// The staged facts covered by commits recorded in this transaction.
    /// Staged facts outside the set are the in-flight commit's —
    /// [`ReadCore::placement_at`] reports them [`FactPlacement::InFlight`];
    /// [`MemoryTx::record_commit`] moves exactly its commit's `fact_ids` in.
    recorded: BTreeSet<FactId>,
    /// Set by a failed [`FactWrite::with_submit_scope`]: this backend
    /// cannot unwind a rejected submit's staging and mints, so its
    /// conforming unwind poisons the transaction instead — the write
    /// primitives and [`apply_pending`] refuse, naming the failure that
    /// poisoned it, keeping facts no recorded commit stands behind out of
    /// durability.
    poisoned: Option<String>,
    /// Metadata of commits recorded in this transaction, unioned with the
    /// committed map for `commit_known` and `RetractCommit` expansion, so a
    /// later commit in the same transaction can retract an earlier one.
    commits: HashMap<CommitId, StoredCommit>,
    /// Results of commits recorded in this transaction, unioned with the
    /// committed cache for content-address dedup.
    submit_results: HashMap<CommitId, MemSubmitResult>,
    next_entity_id: u64,
    next_event_id: u64,
    next_image_id: u64,
    /// Backlink edges the staged facts contribute, keyed by subject id. Each
    /// fact's provisional fid lands at `committed.len() + pending.len()`, above
    /// every committed fid and below the snapshot, so the paginated scan and
    /// snapshot filter accept it during validation; [`apply_pending`] merges
    /// these into the durable indexes unchanged.
    entity_backlinks: HashMap<MemoryEntityId, BTreeSet<FactId>>,
    event_backlinks: HashMap<MemoryEventId, BTreeSet<FactId>>,
    image_backlinks: HashMap<MemoryImageId, BTreeSet<FactId>>,
    /// Reverse-retraction edges the staged meta-facts contribute, keyed by
    /// retracted fact id. A read unions this with the committed
    /// [`Inner::retractors`], so a commit that retracts a pre-commit fact and
    /// then adds facts depending on its absence validates against the
    /// post-retraction state. Provisional retractor ids climb above every
    /// pre-commit target and below the snapshot, so the high-to-low retraction
    /// walk and snapshot filter accept them.
    retractors: HashMap<FactId, Vec<FactId>>,
}

impl Pending {
    /// A transaction's empty delta, counters seeded from the committed ones.
    fn seeded(inner: &Inner) -> Self {
        Self {
            facts: Vec::new(),
            recorded: BTreeSet::new(),
            poisoned: None,
            commits: HashMap::new(),
            submit_results: HashMap::new(),
            next_entity_id: inner.next_entity_id,
            next_event_id: inner.next_event_id,
            next_image_id: inner.next_image_id,
            entity_backlinks: HashMap::new(),
            event_backlinks: HashMap::new(),
            image_backlinks: HashMap::new(),
            retractors: HashMap::new(),
        }
    }

    /// The structured refusal a poisoned transaction answers writes and the
    /// apply step with; `Ok` while healthy.
    fn poison_check(&self) -> Result<(), MemoryError> {
        match &self.poisoned {
            Some(cause) => Err(MemoryError(format!("transaction poisoned: {cause}"))),
            None => Ok(()),
        }
    }
}

/// Branded transaction handle for [`MemoryFactStore`] — the [`FactWrite`]
/// surface over one open transaction.
///
/// `with_tx` holds the store's `Inner` guard for the whole closure (the
/// writer serialisation: readers block until the transaction ends, mirroring
/// a SQL write lock); the handle carries a shared borrow of that committed
/// state plus an exclusive borrow of the [`Pending`] overlay. Reads union
/// the two; mints, staging, and commit recording accumulate in the overlay,
/// leaving `Inner` untouched until `with_tx` applies on `Ok`.
///
/// The handle holds borrows rather than the guard itself so it has no drop
/// glue: the closure's `&'brand mut` borrow of the handle lives as long as
/// the handle does (the invariant brand in its type pins the region), so a
/// handle that owned the guard could never release it back to `with_tx` for
/// the apply step.
///
/// `PhantomData<fn(&'brand ()) -> &'brand ()>` is invariant in `'brand`,
/// which the brand pattern needs; a covariant or contravariant marker would
/// let two closures' brands unify.
///
/// Cross-instance misuse is a compile error — see
/// `CrossInstanceBrandIsCompileError` below for the `compile_fail` doctest,
/// and [`tests::two_commits_share_one_with_tx_brand`] for the positive
/// intra-store check.
pub struct MemoryTx<'brand> {
    committed: &'brand Inner,
    pending: &'brand mut Pending,
    _brand: PhantomData<fn(&'brand ()) -> &'brand ()>,
}

impl MemoryTx<'_> {
    /// Stage a fact onto the pending list, returning the [`FactId`] it keeps
    /// if the transaction commits — the pre-push total of committed plus
    /// pending facts, the slot [`apply_pending`] appends it to.
    ///
    /// Every staged fact records its provisional backlink edges into the
    /// pending `*_backlinks` maps, and a staged meta-fact also records its
    /// reverse-retraction edges into [`Pending::retractors`], so a staged
    /// fact is visible to the cluster-rule reads that follow it. A
    /// `RetractCommit` expands over the target commit's fact ids — committed
    /// or recorded earlier in this transaction; the commit being validated
    /// isn't recorded yet, so it contributes no retractor edges (and an
    /// in-commit fact can't be retracted in-commit — `MetaTargetInSameCommit`
    /// forbids it).
    fn push_fact(&mut self, fact: MemStoredFact) -> FactId {
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
                &self.pending.commits,
                &mut self.pending.retractors,
                &meta.assertion,
                provisional_id,
            );
        }
        self.pending.facts.push(fact);
        provisional_id
    }
}

impl CoreSource for MemoryTx<'_> {
    // Borrows the committed state plus the pending overlay — nothing to
    // lock or await, but `async` to match the trait.
    async fn with_core<R: Send>(&self, f: impl FnOnce(&ReadCore<'_>) -> R + Send) -> R {
        // UFCS: `self` also has a blanket `FactView::snapshot`, so a bare
        // call would be ambiguous.
        f(&ReadCore {
            committed: &self.committed.facts,
            pending: &self.pending.facts,
            recorded_pending: Some(&self.pending.recorded),
            committed_commits: &self.committed.commits,
            pending_commits: Some(&self.pending.commits),
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

impl<'brand> FactWrite<MemoryFactStore> for MemoryTx<'brand> {
    /// Mint a fresh entity id from the pending counter, bumping it. `Inner`
    /// is untouched — a transaction that never applies drops the mint with
    /// the rest of the pending state.
    async fn mint_entity(&mut self) -> Result<MemoryEntityId, MemoryError> {
        self.pending.poison_check()?;
        let id = MemoryEntityId(self.pending.next_entity_id);
        self.pending.next_entity_id = self.pending.next_entity_id.saturating_add(1);
        Ok(id)
    }

    async fn mint_event(&mut self) -> Result<MemoryEventId, MemoryError> {
        self.pending.poison_check()?;
        let id = MemoryEventId(self.pending.next_event_id);
        self.pending.next_event_id = self.pending.next_event_id.saturating_add(1);
        Ok(id)
    }

    async fn mint_image(&mut self) -> Result<MemoryImageId, MemoryError> {
        self.pending.poison_check()?;
        let id = MemoryImageId(self.pending.next_image_id);
        self.pending.next_image_id = self.pending.next_image_id.saturating_add(1);
        Ok(id)
    }

    /// True if `id` is below the pending counter — committed or minted
    /// earlier in this transaction.
    async fn entity_known(&mut self, id: &MemoryEntityId) -> Result<bool, MemoryError> {
        self.pending.poison_check()?;
        Ok(id.0 < self.pending.next_entity_id)
    }

    async fn event_known(&mut self, id: &MemoryEventId) -> Result<bool, MemoryError> {
        self.pending.poison_check()?;
        Ok(id.0 < self.pending.next_event_id)
    }

    async fn image_known(&mut self, id: &MemoryImageId) -> Result<bool, MemoryError> {
        self.pending.poison_check()?;
        Ok(id.0 < self.pending.next_image_id)
    }

    async fn stage_fact(&mut self, fact: MemStoredFact) -> Result<FactId, MemoryError> {
        self.pending.poison_check()?;
        Ok(self.push_fact(fact))
    }

    /// The cached result under `id`, from the committed cache or a commit
    /// recorded earlier in this transaction.
    async fn cached_result(
        &mut self,
        id: &CommitId,
    ) -> Result<Option<MemSubmitResult>, MemoryError> {
        self.pending.poison_check()?;
        Ok(self
            .pending
            .submit_results
            .get(id)
            .or_else(|| self.committed.submit_results.get(id))
            .cloned())
    }

    /// Record the commit's metadata and result into the pending overlay and
    /// mark exactly its `fact_ids` recorded, so its facts — and only its —
    /// read as committed to later commits in this transaction.
    async fn record_commit(
        &mut self,
        commit: StoredCommit,
        result: &MemSubmitResult,
    ) -> Result<(), MemoryError> {
        self.pending.poison_check()?;
        self.pending
            .submit_results
            .insert(result.commit_id.clone(), result.clone());
        self.pending
            .recorded
            .extend(commit.fact_ids.iter().copied());
        self.pending
            .commits
            .insert(commit.commit_id.clone(), commit);
        Ok(())
    }

    type Nested<'n>
        = MemoryTx<'brand>
    where
        Self: 'n;

    /// The overlay can't unwind one submit's slice, so a failed scope
    /// poisons the transaction instead — writes and the apply step refuse
    /// from here on. The read surface stays answerable; the overlay it sees
    /// is void along with the transaction.
    async fn with_submit_scope<'s, R, E, F>(&'s mut self, f: F) -> Result<Result<R, E>, MemoryError>
    where
        Self: 's,
        R: Send,
        E: std::fmt::Debug + Send,
        F: for<'n> FnOnce(
                &'n mut Self::Nested<'s>,
            )
                -> std::pin::Pin<Box<dyn Future<Output = Result<R, E>> + Send + 'n>>
            + Send,
    {
        let result = f(self).await;
        if let Err(e) = &result {
            // One capped line of the inner error keeps later refusals
            // diagnostic without replaying the full rejection; the first
            // cause wins across nested scopes.
            let cause: String = format!("an earlier submit failed: {e:?}")
                .chars()
                .take(200)
                .collect();
            self.pending.poisoned.get_or_insert(cause);
        }
        Ok(result)
    }
}

// ============================================================================
// View-trait impls — carried directly on any CoreSource
// ============================================================================
//
// The view traits and the `CoreSource` sources are all local to this crate, so
// the impls hang off `Src: CoreSource` via blanket impls, parameterised by
// `MemoryFactStore`. Both `MemorySource` and `MemoryTx` gain the read
// surface with no wrapper type, and the lookup logic lives once in `ReadCore`.
// The submit driver reads through `&mut V: FactView<S>`, so only the blanket
// impl on `Src` is exercised. The id kinds and error type come from
// `MemoryFactStore`, so the bodies name `Memory*Id` / `MemoryError` directly.

impl<Src: CoreSource + Send + Sync> FactView<MemoryFactStore> for Src {
    async fn snapshot(&mut self) -> Result<FactId, MemoryError> {
        Ok(CoreSource::snapshot(self))
    }

    async fn fact(&mut self, fact_id: FactId) -> Result<MemFactLookup, MemoryError> {
        Ok(self.with_core(|core| core.fact_at(fact_id)).await)
    }

    async fn commit_known(&mut self, id: &CommitId) -> Result<bool, MemoryError> {
        Ok(self.with_core(|core| core.commit_known_at(id)).await)
    }

    async fn placement(&mut self, id: FactId) -> Result<FactPlacement, MemoryError> {
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
        &mut self,
        member: &MemoryEntityId,
    ) -> Result<MemoryEntityId, MemoryError> {
        let member = *member;
        Ok(self
            .with_core(move |core| core.equiv_class(member, same_entity_edge).representative)
            .await)
    }

    async fn entity_class(
        &mut self,
        member: &MemoryEntityId,
    ) -> Result<EquivClass<MemoryEntityId>, MemoryError> {
        let member = *member;
        Ok(self
            .with_core(move |core| core.equiv_class(member, same_entity_edge))
            .await)
    }

    async fn walk_entity_classes<'b>(
        &'b mut self,
        stream: &'b EntityStream<'b>,
        after: Option<(MemoryEntityId, FactId)>,
        limit: std::num::NonZeroUsize,
    ) -> Result<ClassWalkPage<MemoryFactStore, MemoryEntityId>, MemoryError> {
        match stream {
            EntityStream::ByName { name, language } => {
                let needle = normalize_name(name);
                Ok(self
                    .with_core(move |core| {
                        core.walk_classes(
                            after,
                            limit,
                            |fact| entity_named(fact, &needle, language),
                            same_entity_edge,
                        )
                    })
                    .await)
            }
            EntityStream::ByExternalReference { reference } => Ok(self
                .with_core(move |core| {
                    core.walk_classes(
                        after,
                        limit,
                        |fact| entity_referenced(fact, reference),
                        same_entity_edge,
                    )
                })
                .await),
            EntityStream::All => Ok(self
                .with_core(move |core| {
                    core.walk_classes(after, limit, entity_ids_of, same_entity_edge)
                })
                .await),
            EntityStream::InViewport(viewport) => Ok(self
                .with_core(move |core| {
                    // The event→entity owner map is built once for the walk; the
                    // predicate reads it to attribute a `MovedToLocation` to the
                    // entity its `HasEvent` owns.
                    let owners = core.event_entity_map();
                    core.walk_classes(
                        after,
                        limit,
                        |fact| entity_in_viewport(fact, viewport, &owners),
                        same_entity_edge,
                    )
                })
                .await),
            EntityStream::InTimeRange(_) | EntityStream::InViewportAndTimeRange { .. } => {
                Ok(ClassPage {
                    rows: Vec::new(),
                    next: None,
                    next_class: None,
                })
            }
        }
    }

    async fn all_facts_about_entity(
        &mut self,
        entity: &MemoryEntityId,
        after: Option<FactId>,
        limit: std::num::NonZeroUsize,
    ) -> Result<FactPage<StoredFactOf<MemoryFactStore>, MemoryEntityId, FactId>, MemoryError> {
        Ok(self
            .with_core(|c| {
                c.facts_about(
                    c.entity_backlinks,
                    c.pending_entity_backlinks,
                    entity,
                    after,
                    limit,
                )
            })
            .await)
    }

    async fn walk_entity_depictions<'b>(
        &'b mut self,
        entity: &'b MemoryEntityId,
        after: Option<(MemoryImageId, FactId)>,
        limit: std::num::NonZeroUsize,
    ) -> Result<DepictionWalkPage<MemoryFactStore>, MemoryError> {
        let entity = *entity;
        Ok(self
            .with_core(move |core| {
                // The entity's SameEntity class is resolved once; the walk keys
                // depictions on whether their entity falls in it.
                let members = core.equiv_class(entity, same_entity_edge).members;
                core.walk_depictions(&members, after, limit)
            })
            .await)
    }
}

impl<Src: CoreSource + Send + Sync> EventView<MemoryFactStore> for Src {
    async fn event_representative(
        &mut self,
        member: &MemoryEventId,
    ) -> Result<MemoryEventId, MemoryError> {
        Ok(*member)
    }

    async fn event_class(
        &mut self,
        member: &MemoryEventId,
    ) -> Result<EquivClass<MemoryEventId>, MemoryError> {
        Ok(EquivClass {
            representative: *member,
            members: std::iter::once(*member).collect(),
        })
    }

    async fn all_facts_about_event(
        &mut self,
        event: &MemoryEventId,
        after: Option<FactId>,
        limit: std::num::NonZeroUsize,
    ) -> Result<FactPage<StoredFactOf<MemoryFactStore>, MemoryEventId, FactId>, MemoryError> {
        Ok(self
            .with_core(|c| {
                c.facts_about(
                    c.event_backlinks,
                    c.pending_event_backlinks,
                    event,
                    after,
                    limit,
                )
            })
            .await)
    }
}

impl<Src: CoreSource + Send + Sync> ImageView<MemoryFactStore> for Src {
    async fn image_representative(
        &mut self,
        member: &MemoryImageId,
    ) -> Result<MemoryImageId, MemoryError> {
        let member = *member;
        Ok(self
            .with_core(move |core| core.equiv_class(member, same_artifact_edge).representative)
            .await)
    }

    async fn image_class(
        &mut self,
        member: &MemoryImageId,
    ) -> Result<EquivClass<MemoryImageId>, MemoryError> {
        let member = *member;
        Ok(self
            .with_core(move |core| core.equiv_class(member, same_artifact_edge))
            .await)
    }

    async fn walk_image_classes<'b>(
        &'b mut self,
        stream: &'b ImageStream<'b>,
        after: Option<(MemoryImageId, FactId)>,
        limit: std::num::NonZeroUsize,
    ) -> Result<ClassWalkPage<MemoryFactStore, MemoryImageId>, MemoryError> {
        match stream {
            ImageStream::BySourceUrl { url } => Ok(self
                .with_core(move |core| {
                    core.walk_classes(
                        after,
                        limit,
                        |fact| image_sourced_from(fact, url),
                        same_artifact_edge,
                    )
                })
                .await),
            ImageStream::All => Ok(self
                .with_core(move |core| {
                    core.walk_classes(after, limit, image_ids_of, same_artifact_edge)
                })
                .await),
            ImageStream::InViewport(viewport) => Ok(self
                .with_core(move |core| {
                    core.walk_classes(
                        after,
                        limit,
                        |fact| image_captured_in_viewport(fact, viewport),
                        same_artifact_edge,
                    )
                })
                .await),
            ImageStream::InTimeRange(_) | ImageStream::InViewportAndTimeRange { .. } => {
                Ok(ClassPage {
                    rows: Vec::new(),
                    next: None,
                    next_class: None,
                })
            }
        }
    }

    async fn all_facts_about_image(
        &mut self,
        image: &MemoryImageId,
        after: Option<FactId>,
        limit: std::num::NonZeroUsize,
    ) -> Result<FactPage<StoredFactOf<MemoryFactStore>, MemoryImageId, FactId>, MemoryError> {
        Ok(self
            .with_core(|c| {
                c.facts_about(
                    c.image_backlinks,
                    c.pending_image_backlinks,
                    image,
                    after,
                    limit,
                )
            })
            .await)
    }
}

/// Splice a transaction's [`Pending`] delta into `Inner`: append the staged
/// facts at their provisional ids, merge the precomputed edge maps into the
/// durable indexes, and adopt the recorded commit metadata, result cache, and
/// counters.
///
/// Facts append in push order (the first at the current
/// `Inner::facts.len()`), so each fact lands at the provisional id
/// [`MemoryTx::push_fact`] handed out — the edges and recorded `fact_ids`
/// merge in unchanged. The parallel `fact_commits` entries come from the
/// recorded [`StoredCommit`]s, whose `fact_ids` map staged offsets to their
/// commit.
///
/// A poisoned transaction refuses to apply — an aborted submit's leftovers
/// would otherwise commit. The coverage check below backs that up: every
/// staged fact must be covered by exactly one recorded commit (the recorded
/// `fact_ids` map staged offsets to commit ids), so commit-less or
/// doubly-claimed staging fails loudly even if it arrives unpoisoned.
fn apply_pending(inner: &mut Inner, pending: Pending) -> Result<(), MemoryError> {
    pending.poison_check()?;
    let committed_len = inner.facts.len() as u64;
    let mut owners: Vec<Option<CommitId>> = vec![None; pending.facts.len()];
    for commit in pending.commits.values() {
        for fid in &commit.fact_ids {
            let slot = fid
                .get()
                .checked_sub(committed_len)
                .and_then(|off| usize::try_from(off).ok())
                .and_then(|off| owners.get_mut(off));
            let Some(slot) = slot else {
                return Err(MemoryError(format!(
                    "recorded commit {:?} names fact id {} outside this transaction's staged range",
                    commit.commit_id,
                    fid.get(),
                )));
            };
            if let Some(prior) = slot {
                return Err(MemoryError(format!(
                    "recorded commits {:?} and {:?} both claim fact id {}",
                    prior,
                    commit.commit_id,
                    fid.get(),
                )));
            }
            *slot = Some(commit.commit_id.clone());
        }
    }
    let fact_commits: Vec<CommitId> = owners
        .into_iter()
        .enumerate()
        .map(|(off, owner)| {
            owner.ok_or_else(|| {
                MemoryError(format!(
                    "staged fact at offset {off} has no recorded commit; \
                     a rejected submit's staging cannot be committed"
                ))
            })
        })
        .collect::<Result<_, _>>()?;
    for (fact, commit_id) in pending.facts.into_iter().zip(fact_commits) {
        inner.facts.push(fact);
        inner.fact_commits.push(commit_id);
    }
    merge_index(&mut inner.entity_backlinks, pending.entity_backlinks);
    merge_index(&mut inner.event_backlinks, pending.event_backlinks);
    merge_index(&mut inner.image_backlinks, pending.image_backlinks);
    merge_index(&mut inner.retractors, pending.retractors);
    inner.commits.extend(pending.commits);
    inner.submit_results.extend(pending.submit_results);
    inner.next_entity_id = pending.next_entity_id;
    inner.next_event_id = pending.next_event_id;
    inner.next_image_id = pending.next_image_id;
    Ok(())
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

/// The edge-recording behind [`MemoryTx::push_fact`]. A fact-target assertion
/// maps its target to `retractor_id`; a commit-target expands over the named
/// commit's fact ids — read from the committed map or from the commits
/// recorded earlier in the transaction, the same union `commit_known`
/// resolves against. A `RetractCommit` against a commit in neither map
/// records nothing.
fn record_retractor_edges(
    committed_commits: &HashMap<CommitId, StoredCommit>,
    pending_commits: &HashMap<CommitId, StoredCommit>,
    retractors: &mut HashMap<FactId, Vec<FactId>>,
    assertion: &MetaAssertion,
    retractor_id: FactId,
) {
    match assertion {
        MetaAssertion::RetractFact { target, .. } | MetaAssertion::SupersedeFact { target, .. } => {
            retractors.entry(*target).or_default().push(retractor_id);
        }
        MetaAssertion::RetractCommit { target, .. } => {
            let commit = committed_commits
                .get(target)
                .or_else(|| pending_commits.get(target));
            if let Some(commit) = commit {
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
/// Called from [`MemoryTx::push_fact`] at the provisional fid the push
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

// `PhantomData<fn(&'brand ()) -> &'brand ()>` is `Send + Sync` (fn pointers
// are), and the handle's two borrows are of `Send + Sync` state, so the
// handle is thread-safe.

impl FactStore for MemoryFactStore {
    type Error = MemoryError;
    type Ids = MemoryIds;
    type Cursor = FactId;
    type ClassCursor<Rep>
        = (Rep, FactId)
    where
        Rep: Send;
    type Tx<'brand> = MemoryTx<'brand>;
    type View<'a> = MemorySource<'a>;

    async fn with_tx<F, T, E>(&self, f: F) -> Result<Result<T, E>, Self::Error>
    where
        F: for<'brand> FnOnce(
                &'brand Self,
                &'brand mut Self::Tx<'brand>,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<T, E>> + Send + 'brand>,
            > + Send,
        T: Send,
        E: Send,
    {
        // The guard is held across the whole closure — the writer
        // serialisation. The `Send` guard spans the closure's `.await`s; the
        // matcher and validator resolve immediately, so the held lock never
        // serialises real I/O. The `for<'brand>` bound lets the closure pick
        // the fresh brand.
        let mut guard = self.inner.lock().await;
        let mut pending = Pending::seeded(&guard);
        let mut tx = MemoryTx {
            committed: &guard,
            pending: &mut pending,
            _brand: PhantomData,
        };
        let result = f(self, &mut tx).await;
        if result.is_ok() {
            apply_pending(&mut guard, pending)?;
        }
        // On `Err` the overlay drops here and `Inner` was never touched, so
        // there is nothing to roll back.
        Ok(result)
    }

    async fn next_fact_id(&self) -> Result<FactId, Self::Error> {
        Ok(self.lock_inner().await.next_fact_id())
    }

    async fn no_later_than(&self, snapshot: FactId) -> Result<Self::View<'_>, Self::Error> {
        Ok(MemorySource {
            store: self,
            snapshot,
        })
    }

    async fn now(&self) -> Result<Self::View<'_>, Self::Error> {
        let snap = self.lock_inner().await.next_fact_id();
        Ok(MemorySource {
            store: self,
            snapshot: snap,
        })
    }
}

#[cfg(test)]
mod matcher_tests;
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
/// use chronoscope_core::store::memory::MemoryFactStore;
/// use chronoscope_core::store::FactStore;
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
///                             Ok::<(), String>(())
///                         })
///                     })
///                     .await;
///                 Ok::<(), String>(())
///             })
///         })
///         .await;
/// }
/// ```
#[cfg(doctest)]
struct CrossInstanceBrandIsCompileError;
