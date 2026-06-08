//! In-memory fact-store backend.
//!
//! For tests and proptests. Not optimised — linear scans where the DB
//! backends use indexes. The same [`FactStore`] properties hold here.
//!
//! Surface:
//!
//! - `submit_commit` resolves [`Decl::Local`] via match-or-mint and passes
//!   [`Decl::Existing`] through.
//! - `fact()` returns `Active` / `Future` / `Unknown`. It applies no
//!   retraction filtering, so it never yields [`FactLookup::Retracted`];
//!   retraction visibility layers on top of the raw slot lookup.
//! - `next_fact_id` / `no_later_than` / `now` clock surface.
//! - The per-subject view methods (`walk_*`, `*_representative`, `*_class`,
//!   `*_subgraph`) return valid stubs — an empty page, the subject as its own
//!   representative, a singleton class, a single-node subgraph — pending the
//!   index machinery.
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

use std::collections::HashMap;
use std::future::Future;
use std::marker::PhantomData;

use async_lock::{Mutex, MutexGuard};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::facts::ids::{CommitId, FactId};
use crate::facts::schema::{
    EdgeSubgraph, EntityStream, EquivClass, EventStream, FactPage, ImageStream,
};
use crate::facts::store::{
    EntityView, EventView, FactStore, FactView, ImageView, StoredFactOf, SubmitCommitError,
};
use crate::facts::submit::pipeline::{self, MatchOutcome, substitute_facts, validate_submit};
use crate::facts::submit::{
    Commit, Decl, EntityIdx, EventIdx, FactLookup, ImageIdx, Resolution, ResolutionOrigin,
    StoredCommit, StoredFact, SubjectKind, SubmitError, SubmitFact, SubmitResult,
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
    /// Exclusive upper bound: ids `>= snapshot` read as [`FactLookup::Future`].
    snapshot: FactId,
}

impl<'a> ReadCore<'a> {
    /// Look up a fact across the committed + pending halves.
    ///
    /// - `fact_id >= snapshot` → [`FactLookup::Future`].
    /// - `fact_id < committed.len()` → `committed`.
    /// - otherwise → `pending` at offset `fact_id - committed.len()`.
    ///
    /// No retraction filtering: a hit is `Active`, a missing-but-expected slot
    /// is `Unknown` (a shape bug, not a domain outcome). Retraction visibility
    /// layers on top.
    fn fact_at(&self, fact_id: FactId) -> MemFactLookup {
        if fact_id.get() >= self.snapshot.get() {
            return FactLookup::Future;
        }
        let committed_len = self.committed.len() as u64;
        if fact_id.get() < committed_len {
            let Ok(idx) = usize::try_from(fact_id.get()) else {
                return FactLookup::Unknown;
            };
            return match self.committed.get(idx) {
                Some(fact) => FactLookup::Active(Box::new(fact.clone())),
                None => FactLookup::Unknown,
            };
        }
        let Ok(offset) = usize::try_from(fact_id.get() - committed_len) else {
            return FactLookup::Unknown;
        };
        match self.pending.get(offset) {
            Some(fact) => FactLookup::Active(Box::new(fact.clone())),
            None => FactLookup::Unknown,
        }
    }

    /// Whether `id` names a committed commit; see [`Self::committed_commits`].
    /// A retracted commit is still present (retraction records a meta-fact, it
    /// doesn't erase the commit), so it reports as existing.
    fn commit_known_at(&self, id: &CommitId) -> bool {
        self.committed_commits.contains_key(id)
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

/// Owned mutation payload accumulated during a `submit_commit` call: the
/// in-flight facts plus the mint-advanced next-id counters.
///
/// Held in a [`UnionSource`] while the commit is prepared, yielded by
/// [`UnionSource::into_pending`], drained into `Inner` via [`apply_pending`].
/// Carrying the counters alongside the facts means a validator failure drops
/// the [`Pending`] without touching `Inner` — counter rollback is implicit in
/// not applying it.
struct Pending {
    facts: Vec<MemStoredFact>,
    next_entity_id: u64,
    next_event_id: u64,
    next_image_id: u64,
}

/// Source that unions committed `Inner` storage with the in-flight
/// [`Pending`] payload of a `submit_commit` call. The single read surface
/// across the match → mint → validate → insert sequence.
///
/// Borrowed via [`UnionSource::from_inner`] while the inner mutex is held.
/// Reads see committed facts plus any pushed onto `pending.facts`; mints and
/// pushes accumulate in the owned `pending`, leaving `Inner` untouched.
/// [`UnionSource::into_pending`] yields that payload at the end, and the caller
/// drains it into `Inner` via [`apply_pending`].
///
/// The in-flight facts and the three counters live only in `pending`, so
/// there's one owner of the mutation state. Counter rollback on validation
/// failure is implicit: drop the source without calling `apply_pending`.
///
/// `'g` is the `Inner` borrow — the held guard's deref lifetime.
struct UnionSource<'g> {
    committed: &'g Inner,
    pending: Pending,
}

impl<'g> UnionSource<'g> {
    /// Construct a [`UnionSource`] from the held `Inner` guard. Seeds the
    /// pending counters from the committed ones; starts with no pending facts.
    fn from_inner(inner: &'g Inner) -> Self {
        Self {
            committed: inner,
            pending: Pending {
                facts: Vec::new(),
                next_entity_id: inner.next_entity_id,
                next_event_id: inner.next_event_id,
                next_image_id: inner.next_image_id,
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
    fn push_fact(&mut self, fact: MemStoredFact) {
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
        _limit: usize,
    ) -> Result<FactPage<StoredFactOf<MemoryFactStore>, MemoryEntityId>, MemoryError> {
        Ok(FactPage {
            items: Vec::new(),
            truncated: false,
        })
    }

    async fn all_facts_about_entity(
        &self,
        _entity: &MemoryEntityId,
        _cursor: FactId,
        _limit: usize,
    ) -> Result<FactPage<StoredFactOf<MemoryFactStore>, MemoryEntityId>, MemoryError> {
        Ok(FactPage {
            items: Vec::new(),
            truncated: false,
        })
    }

    async fn entity_topological_subgraph(
        &self,
        seed: MemoryEntityId,
        _cursor: FactId,
        _limit: usize,
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
        _limit: usize,
    ) -> Result<FactPage<StoredFactOf<MemoryFactStore>, MemoryEventId>, MemoryError> {
        Ok(FactPage {
            items: Vec::new(),
            truncated: false,
        })
    }

    async fn all_facts_about_event(
        &self,
        _event: &MemoryEventId,
        _cursor: FactId,
        _limit: usize,
    ) -> Result<FactPage<StoredFactOf<MemoryFactStore>, MemoryEventId>, MemoryError> {
        Ok(FactPage {
            items: Vec::new(),
            truncated: false,
        })
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
        _limit: usize,
    ) -> Result<FactPage<StoredFactOf<MemoryFactStore>, MemoryImageId>, MemoryError> {
        Ok(FactPage {
            items: Vec::new(),
            truncated: false,
        })
    }

    async fn all_facts_about_image(
        &self,
        _image: &MemoryImageId,
        _cursor: FactId,
        _limit: usize,
    ) -> Result<FactPage<StoredFactOf<MemoryFactStore>, MemoryImageId>, MemoryError> {
        Ok(FactPage {
            items: Vec::new(),
            truncated: false,
        })
    }
}

/// Drain a [`Pending`] payload into `Inner`, assigning each fact its durable
/// [`FactId`]. Appends in push order (the first at the current
/// `Inner::facts.len()`), records the parallel `fact_commits`, advances the
/// counters, and returns the assigned ids in order. The sole assigner of fact
/// ids; the union source's staging only fixed the order.
fn apply_pending(inner: &mut Inner, pending: Pending, commit_id: CommitId) -> Vec<FactId> {
    let mut assigned = Vec::with_capacity(pending.facts.len());
    for fact in pending.facts {
        let id = FactId::new(inner.facts.len() as u64);
        inner.facts.push(fact);
        inner.fact_commits.push(commit_id.clone());
        assigned.push(id);
    }
    inner.next_entity_id = pending.next_entity_id;
    inner.next_event_id = pending.next_event_id;
    inner.next_image_id = pending.next_image_id;
    assigned
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
        // 1. Hash the producer-form commit.
        let commit_id = commit
            .id()
            .map_err(|e| SubmitCommitError::Backend(e.into()))?;

        // 2. Structural check before any I/O: out-of-range indices and unused
        //    declarations are producer-side bundle bugs, so reject them before
        //    taking the lock and making a contended writer wait.
        check_idx_refs(&commit)?;

        // 3. Acquire the inner lock once and hold it across the sequence. The
        //    `Send` guard spans the `.await`s below: the `UnionSource`
        //    accumulates owned pending state, the matcher and validator resolve
        //    immediately, and the drain runs on the same task, so the lock
        //    never serialises real I/O.
        let mut inner_guard = self.lock_inner().await;

        // 4. Idempotent dedup: a re-submit returns the cached result with
        //    `previously_committed: true`, doing no mints, inserts, or rule
        //    re-evaluation.
        if let Some(cached) = inner_guard.submit_results.get(&commit_id) {
            return Ok(SubmitResult {
                previously_committed: true,
                ..cached.clone()
            });
        }

        // 5. Open the union source over the committed `Inner`. All reads /
        //    mints / pushes go through it, so `Inner` is touched only at apply.
        //    Trait reads borrow `&source`, ending before the `&mut source` mint
        //    phase.
        let mut source = UnionSource::from_inner(&inner_guard);

        // 6. Reject any Decl::Existing id unknown to the source, before any
        //    mint, so counter advances stay tied to successful commits.
        for (i, decl) in commit.entities.iter().enumerate() {
            if let Decl::Existing { id } = decl
                && !source.entity_known(id)
            {
                return Err(SubmitCommitError::Submit(
                    SubmitError::UnknownExistingEntity {
                        decl_position: EntityIdx(i),
                    },
                ));
            }
        }
        for (i, decl) in commit.events.iter().enumerate() {
            if let Decl::Existing { id } = decl
                && !source.event_known(id)
            {
                return Err(SubmitCommitError::Submit(
                    SubmitError::UnknownExistingEvent {
                        decl_position: EventIdx(i),
                    },
                ));
            }
        }
        for (i, decl) in commit.images.iter().enumerate() {
            if let Decl::Existing { id } = decl
                && !source.image_known(id)
            {
                return Err(SubmitCommitError::Submit(
                    SubmitError::UnknownExistingImage {
                        decl_position: ImageIdx(i),
                    },
                ));
            }
        }

        // 7. Run the matchers against the source to resolve each Local decl to
        //    a match or a mint. Events are never matched, so each Local event
        //    decl gets a synthesised mint outcome. The `&source` borrows end
        //    before the `&mut source` mint phase.
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

        // 8. Resolve each declaration to a persistent id. Mints land in the
        //    source's pending counters; `Inner` stays untouched.
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

        let stored_facts = substitute_facts(
            &commit.facts,
            &entity_sub_map,
            &event_sub_map,
            &image_sub_map,
        )
        .map_err(SubmitCommitError::Submit)?;

        // 9. Stage the substituted facts onto the pending list, fixing their
        //    order. `apply_pending` assigns their FactIds at drain.
        for fact in &stored_facts {
            source.push_fact(fact.clone());
        }

        // 10. Run the rule validator over the source. On rejection, drop the
        //     source without applying — counter rollback is implicit.
        validate_submit(&stored_facts, &source).await?;

        // 11. Drain into `Inner`. `apply_pending` is the sole assigner of the
        //     FactIds, returning them in push order.
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

/// Collect the bundle-local indices every fact references and reject two
/// producer bugs in priority order:
///
/// 1. Out-of-range — any idx past its decl-list length, checked first.
/// 2. Unused declaration — once references are in-range, any decl position with
///    no incoming reference. A decl with no fact under it is almost certainly a
///    bug, better surfaced than silently minted.
///
/// Runs before mint allocation, so neither rejection burns a counter. O(N+F)
/// over decls and facts.
fn check_idx_refs<EntId, EvtId, ImgId>(
    commit: &Commit<EntId, EvtId, ImgId>,
) -> Result<(), SubmitCommitError<MemoryError, EntId, EvtId, ImgId>> {
    use std::collections::HashSet;

    let mut entity_refs: HashSet<EntityIdx> = HashSet::new();
    let mut event_refs: HashSet<EventIdx> = HashSet::new();
    let mut image_refs: HashSet<ImageIdx> = HashSet::new();

    for fact in &commit.facts {
        // The collector half of the id-traversal records each referenced
        // index into the per-kind sets. `Meta` targets are persistent
        // `FactId` / `CommitId`, not indices, so they contribute none.
        let mut on_entity = |idx: &EntityIdx| {
            entity_refs.insert(*idx);
        };
        let mut on_event = |idx: &EventIdx| {
            event_refs.insert(*idx);
        };
        let mut on_image = |idx: &ImageIdx| {
            image_refs.insert(*idx);
        };
        match fact {
            SubmitFact::Factual { assertion, .. } => {
                assertion.for_each_id(&mut on_entity, &mut on_event, &mut on_image);
            }
            SubmitFact::Judgment { assertion, .. } => {
                assertion.for_each_id(&mut on_entity, &mut on_event, &mut on_image);
            }
            SubmitFact::Meta { .. } => {}
        }
    }

    // Out-of-range across all three kinds first, then unused-decl across
    // all three. Each check is one helper parameterised over the kind's
    // reference set, declaration count, and idx newtype.
    check_refs_in_range(
        &entity_refs,
        commit.entities.len(),
        |idx| idx.0,
        |idx, decl_count| SubmitError::EntityIdxOutOfRange { idx, decl_count },
    )?;
    check_refs_in_range(
        &event_refs,
        commit.events.len(),
        |idx| idx.0,
        |idx, decl_count| SubmitError::EventIdxOutOfRange { idx, decl_count },
    )?;
    check_refs_in_range(
        &image_refs,
        commit.images.len(),
        |idx| idx.0,
        |idx, decl_count| SubmitError::ImageIdxOutOfRange { idx, decl_count },
    )?;

    check_all_decls_referenced(&entity_refs, commit.entities.len(), EntityIdx, |position| {
        SubmitError::UnusedDeclaration {
            kind: SubjectKind::Entity,
            position,
        }
    })?;
    check_all_decls_referenced(&event_refs, commit.events.len(), EventIdx, |position| {
        SubmitError::UnusedDeclaration {
            kind: SubjectKind::Event,
            position,
        }
    })?;
    check_all_decls_referenced(&image_refs, commit.images.len(), ImageIdx, |position| {
        SubmitError::UnusedDeclaration {
            kind: SubjectKind::Image,
            position,
        }
    })?;
    Ok(())
}

/// Reject any reference past the end of its kind's declaration list.
/// Parameterised over the reference set, declaration count, the `position`
/// extractor, and the out-of-range error constructor. Called once per kind by
/// [`check_idx_refs`].
fn check_refs_in_range<Idx, EntId, EvtId, ImgId>(
    refs: &std::collections::HashSet<Idx>,
    decl_count: usize,
    position: impl Fn(&Idx) -> usize,
    out_of_range: impl Fn(usize, usize) -> SubmitError<EntId, EvtId, ImgId>,
) -> Result<(), SubmitCommitError<MemoryError, EntId, EvtId, ImgId>> {
    for r in refs {
        let idx = position(r);
        if idx >= decl_count {
            return Err(SubmitCommitError::Submit(out_of_range(idx, decl_count)));
        }
    }
    Ok(())
}

/// Reject the first declaration position with no incoming reference.
/// Parameterised over the reference set, declaration count, the `idx_ctor` for
/// testing set membership, and the unreferenced-position error constructor.
/// Called once per kind by [`check_idx_refs`], after the in-range checks pass.
fn check_all_decls_referenced<Idx, EntId, EvtId, ImgId>(
    refs: &std::collections::HashSet<Idx>,
    decl_count: usize,
    idx_ctor: fn(usize) -> Idx,
    unused: impl Fn(usize) -> SubmitError<EntId, EvtId, ImgId>,
) -> Result<(), SubmitCommitError<MemoryError, EntId, EvtId, ImgId>>
where
    Idx: Eq + std::hash::Hash,
{
    for i in 0..decl_count {
        if !refs.contains(&idx_ctor(i)) {
            return Err(SubmitCommitError::Submit(unused(i)));
        }
    }
    Ok(())
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
