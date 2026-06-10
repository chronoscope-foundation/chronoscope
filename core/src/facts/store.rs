//! Fact-store traits.
//!
//! - [`FactStore`] is the writeable handle: it owns the clock
//!   ([`Self::next_fact_id`], [`Self::now`]), accepts commits
//!   ([`Self::submit_commit`]), and builds snapshot-scoped read views
//!   ([`Self::no_later_than`]).
//! - [`FactView`] is a snapshot-scoped read handle with subject-agnostic
//!   methods (fact lookup, snapshot inquiry).
//!
//! Subject-parametric reads live on one companion trait per subject kind:
//! [`EntityView`], [`EventView`], [`ImageView`]. Each does equivalence
//! resolution, indexed walks, and backlink walks; [`EntityView`] adds
//! closed-subgraph walks over the canonical entity edge relation.
//!
//! The view traits are generic over the [`FactStore`] `S` and carry no
//! associated types — they read the id kinds and error type off `S`, so a
//! store and its views share one id vocabulary and one error type.
//!
//! Trait methods spell `async fn` as `fn ... -> impl Future + Send` to make
//! the `Send` bound explicit; clippy's `async_fn_in_trait` rejects the
//! implicit form under `-D warnings`.
//!
//! ## Snapshot semantics
//!
//! [`FactId`] is a `u64` newtype; every value including zero is valid.
//! Snapshots and pagination cursors are plain [`FactId`] under "next id"
//! semantics:
//!
//! - [`Self::next_fact_id`] returns the next id the store would mint — one
//!   past the highest stored fact, or `FactId::new(0)` on an empty store.
//! - `no_later_than(snapshot) -> View` is an exclusive upper bound: the
//!   view exposes facts with `id < snapshot`. `FactId::new(0)` is the
//!   empty-store view; every lookup returns [`FactLookup::Future`] or
//!   [`FactLookup::Unknown`].
//! - [`FactView::snapshot`] returns the bound the view was pinned at.
//! - Pagination cursors on the `walk_*` methods are inclusive lower bounds
//!   — `walk(cursor, ...)` filters items with `id >= cursor`. First page
//!   passes `FactId::new(0)`.
//!
//! [`Self::next_fact_id`] gives the scalar watermark; [`Self::now`] returns
//! a snapshot view directly (its method doc says why it isn't a default
//! composing the two).
//!
//! ## Transaction-scoped writes
//!
//! Callers enter a transaction via [`Self::with_tx`], submit commits through
//! the supplied [`Self::Tx`] handle, and return `Ok` to commit or
//! `Err`/panic to roll back. The submit sequence is match → resolve/mint →
//! validate → insert. It reads existing state to match declarations and
//! check cross-fact rules before inserting, so the whole sequence runs under
//! one transaction — a held mutex for the in-memory backend, a SQL
//! transaction for a SQL backend — to keep a concurrent writer from slipping
//! between the reads and the insert.
//!
//! The closure-scoped `with_tx` shape (rather than `begin_tx` / `commit_tx`)
//! lets the trait carry the brand-pattern `for<'brand>` HRTB.
//!
//! ## Cross-store safety
//!
//! The `for<'brand>` HRTB plus the invariant `'brand` on `Tx<'brand>` stops
//! a tx from one store reaching another store of the same type. Each
//! `with_tx(...)` mints a fresh existential `'brand`, so passing a tx from
//! `store_a.with_tx(...)` into `store_b.with_tx(...)`'s closure fails to
//! typecheck — the brands are distinct existentials and the lifetime is
//! invariant. The closure is the only way to name a `Tx<'brand>`, and the
//! returned future is bounded by `'brand`, so handles can't leak past it.
//!
//! `'brand` is a type-level marker, not a borrow; backends carry their state
//! on `&self`. [`MemoryTx`](crate::facts::memory::MemoryTx) is a zero-size
//! sentinel. A SQL backend puts a `sqlx::Transaction` inside `Tx<'brand>`;
//! the brand still gates which `with_tx` body it threads through.

use std::fmt::Debug;
use std::future::Future;
use std::hash::Hash;
use std::pin::Pin;

use serde::Serialize;

use crate::facts::ids::FactId;
use crate::facts::schema::{
    EdgeSubgraph, EntityStream, EquivClass, EventStream, FactPage, ImageStream,
};
use crate::facts::submit;
use crate::facts::submit::{FactLookup, StoredFact, SubmitError, SubmitResult};
use crate::nonempty::NonEmptyVec;

// ============================================================================
// PersistentId — the persistent-id bound alias
// ============================================================================

/// The bounds every backend's persistent id type must satisfy.
///
/// A supertrait plus a blanket impl, so the bound pile is named once instead
/// of repeated at every associated-type declaration. Each bound is used
/// somewhere in the store / view / submit surface:
///
/// - `Clone` — ids are copied out of resolution maps and into results.
/// - `Ord` — class members live in the `BTreeSet` inside
///   [`EquivClass`](crate::facts::schema::EquivClass); also supplies `Eq`.
/// - `Hash` — keys in the `HashMap` lookup indexes.
/// - `Serialize` — a [`Decl::Existing`](crate::facts::submit::Decl::Existing)
///   carries an id, so [`Commit::id`](crate::facts::submit::Commit::id)
///   serializes id types into the JCS hash input.
/// - `Send + Sync` — the store surface is `Send + Sync` and ids flow through
///   `Send` futures.
///
/// `Debug`, `DeserializeOwned`, `Display`, and `'static` are absent: the
/// trait surface never uses them on an id (ids are minted in-store and a
/// read-path `Decl::Existing` id arrives already typed). Concrete backend id
/// types may still derive them for their own needs.
pub trait PersistentId: Clone + Ord + Hash + Serialize + Send + Sync {}

impl<T> PersistentId for T where T: Clone + Ord + Hash + Serialize + Send + Sync {}

/// Producer-form commit consumed by [`FactStore::submit_commit`], pinned to
/// a store's three id kinds. An alias to keep signatures readable and
/// clippy's `type_complexity` quiet.
pub type SubmitCommitInput<S> = submit::Commit<
    <S as FactStore>::EntityId,
    <S as FactStore>::EventId,
    <S as FactStore>::ImageId,
>;

/// Output of [`FactStore::submit_commit`]. The [`SubmitCommitError`] carries
/// the store's three id types so a rejection surfaces the offending id typed,
/// not stringified.
pub type SubmitCommitOutput<S> = Result<
    SubmitResult<<S as FactStore>::EntityId, <S as FactStore>::EventId, <S as FactStore>::ImageId>,
    SubmitCommitError<
        <S as FactStore>::Error,
        <S as FactStore>::EntityId,
        <S as FactStore>::EventId,
        <S as FactStore>::ImageId,
    >,
>;

/// Result of [`FactView::fact`] for a view over store `S`.
pub type FactLookupOutput<S> = Result<
    FactLookup<<S as FactStore>::EntityId, <S as FactStore>::EventId, <S as FactStore>::ImageId>,
    <S as FactStore>::Error,
>;

// ============================================================================
// FactStore — writeable handle
// ============================================================================

/// Writeable fact-store handle. Owns the clock and the commit endpoint;
/// builds snapshot-scoped read views via [`Self::no_later_than`] /
/// [`Self::now`].
///
/// `Error` is backend failures (SQL connection, I/O). Submit-pipeline
/// failures (rule violations, index out-of-range) are domain errors and flow
/// through [`SubmitError`] on a separate channel.
///
/// `Sized` is a supertrait because the view traits take the store as a
/// type-parameter argument (`FactView<Self>`). Stores are always concrete
/// handles, never `dyn FactStore`, so the bound is free and lets the
/// `View<'a>` GAT name `FactView<Self>`.
pub trait FactStore: Send + Sync + Sized {
    /// Backend-specific error type for non-domain failures.
    type Error: Debug + Send + Sync;

    /// Persistent entity id this backend mints and references. Shape
    /// varies by backend (`u64`-newtypes in-memory, something else for
    /// SQL); the trait pins only [`PersistentId`].
    type EntityId: PersistentId;
    /// Persistent lifetime-event id this backend mints and references. See
    /// [`Self::EntityId`].
    type EventId: PersistentId;
    /// Persistent image id this backend mints and references. See
    /// [`Self::EntityId`].
    type ImageId: PersistentId;

    /// Branded transaction handle threaded through [`Self::submit_commit`].
    /// `'brand` is a fresh existential minted per [`Self::with_tx`] call; it
    /// tags handles to their call site so the type system can refuse
    /// cross-instance misuse, and has no runtime role.
    ///
    /// In-memory backends hold a `MutexGuard` in the handle; SQL backends
    /// hold a `sqlx::Transaction`. Dropping the handle before
    /// [`Self::with_tx`]'s closure completes rolls back.
    type Tx<'brand>: Send + 'brand
    where
        Self: 'brand;

    /// The borrowed snapshot-scoped read view this store produces. The bound
    /// aggregates [`FactView`] and the three per-subject view traits so
    /// consumers can call any read method on a `Self::View<'_>` without
    /// re-binding. The view reads its id kinds and error type off `Self`.
    type View<'a>: FactView<Self> + EntityView<Self> + EventView<Self> + ImageView<Self> + 'a
    where
        Self: 'a;

    /// Run `f` inside a fresh transaction. The closure receives `&Self` and
    /// `&mut Self::Tx<'brand>`, where `'brand` is a fresh existential per
    /// call, so a tx from one `with_tx` call can't reach another store's
    /// closure. This is the brand pattern (cf. `GhostCell`, `qcell::TCell`).
    ///
    /// `f` takes `&Self` rather than capturing one: the `for<'brand>` HRTB
    /// would force an externally-captured `&self` to be `'static`, so the
    /// store reference is threaded in instead.
    ///
    /// The store opens the transaction, runs `f`, commits on `Ok` and rolls
    /// back on `Err`. Backend-level failures (`begin`, `commit`) flow through
    /// `Result<R, Self::Error>`; submit-pipeline errors live inside `R` —
    /// typically `R = Result<T, SubmitCommitError<Self::Error>>`.
    fn with_tx<F, R>(&self, f: F) -> impl Future<Output = Result<R, Self::Error>> + Send
    where
        F: for<'brand> FnOnce(
                &'brand Self,
                &'brand mut Self::Tx<'brand>,
            ) -> Pin<Box<dyn Future<Output = R> + Send + 'brand>>
            + Send,
        R: Send;

    /// Submit a commit bundle inside the supplied transaction. Derives the
    /// [`CommitId`](crate::facts::ids::CommitId) via `Commit::id()` (JCS +
    /// SHA-256) and dedups against the commit cache, resolves every
    /// declaration to a persistent id via match-or-mint, rewrites each fact's
    /// bundle-local indices to the resolved ids, and inserts.
    ///
    /// The `tx` borrow is `&mut` so the caller can't overlap two
    /// `submit_commit` calls on one handle; sequential commits inside one
    /// transaction are fine.
    fn submit_commit<'brand, 'tx>(
        &'tx self,
        tx: &'tx mut Self::Tx<'brand>,
        commit: SubmitCommitInput<Self>,
    ) -> impl Future<Output = SubmitCommitOutput<Self>> + Send + 'tx
    where
        Self: 'brand,
        'brand: 'tx;

    /// The next [`FactId`] this store would mint — one past the highest
    /// stored fact, or `FactId::new(0)` on an empty store (the value
    /// [`Self::no_later_than`] accepts to pin an empty-store view).
    fn next_fact_id(&self) -> impl Future<Output = Result<FactId, Self::Error>> + Send;

    /// A read view pinned at `snapshot` (exclusive upper bound): the view
    /// exposes facts with `id < snapshot`. `FactId::new(0)` is the
    /// empty-store view — every lookup returns `Future` / `Unknown`.
    fn no_later_than(&self, snapshot: FactId) -> Self::View<'_>;

    /// Snapshot the current latest state. Implemented directly so a SQL
    /// backend can pin the snapshot in one round-trip (`SELECT max(fact_id)`
    /// inside the view-constructing query) rather than the two a default
    /// composing [`Self::next_fact_id`] + [`Self::no_later_than`] would take.
    /// Either form is a snapshot at some recent point; a writer landing
    /// mid-read isn't included.
    fn now(&self) -> impl Future<Output = Result<Self::View<'_>, Self::Error>> + Send;
}

/// Aggregate error for `submit_commit`: a submit-pipeline domain error or a
/// backend failure, in one enum so the return type stays `Result<_, _>`.
/// `Submit` carries every rule violation the pipeline found in one batch;
/// backend failures carry backend diagnostics.
///
/// `E` is the backend error; `EntId` / `EvtId` / `ImgId` are the backend's id
/// kinds, threaded into [`SubmitError`] so a rejection carries the offending
/// id typed. They appear only in the `Submit` arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitCommitError<E, EntId, EvtId, ImgId> {
    /// A submit-pipeline run rejected the bundle, carrying the non-empty
    /// batch of every rule it violated.
    Submit(NonEmptyVec<SubmitError<EntId, EvtId, ImgId>>),
    /// A backend failure (I/O, transaction abort, etc.).
    Backend(E),
}

impl<E, EntId, EvtId, ImgId> std::fmt::Display for SubmitCommitError<E, EntId, EvtId, ImgId>
where
    E: std::fmt::Display,
    EntId: std::fmt::Display,
    EvtId: std::fmt::Display,
    ImgId: std::fmt::Display,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Submit(errs) => {
                write!(
                    f,
                    "submit pipeline rejected commit ({} error(s)):",
                    errs.len()
                )?;
                for e in errs {
                    write!(f, "\n  - {e}")?;
                }
                Ok(())
            }
            Self::Backend(e) => write!(f, "backend failure: {e}"),
        }
    }
}

impl<E, EntId, EvtId, ImgId> std::error::Error for SubmitCommitError<E, EntId, EvtId, ImgId>
where
    E: std::fmt::Debug + std::fmt::Display,
    EntId: std::fmt::Debug + std::fmt::Display,
    EvtId: std::fmt::Debug + std::fmt::Display,
    ImgId: std::fmt::Debug + std::fmt::Display,
{
}

impl<E, EntId, EvtId, ImgId> From<SubmitError<EntId, EvtId, ImgId>>
    for SubmitCommitError<E, EntId, EvtId, ImgId>
{
    fn from(e: SubmitError<EntId, EvtId, ImgId>) -> Self {
        Self::Submit(NonEmptyVec::singleton(e))
    }
}

// ============================================================================
// FactView — schema-agnostic read handle
// ============================================================================

/// Snapshot-scoped read handle, subject-agnostic methods only — see
/// [`EntityView`] / [`EventView`] / [`ImageView`] for the rest.
///
/// Every method here and on the companion traits is filtered to facts with
/// `id < snapshot()` not retracted by any fact below that bound.
///
/// Generic over the [`FactStore`] `S`: id kinds and error type come from `S`,
/// so the trait has no associated types of its own.
pub trait FactView<S: FactStore>: Send + Sync {
    /// The exclusive-upper-bound [`FactId`] this view is pinned at; it exposes
    /// facts with `id < snapshot()`. `FactId::new(0)` is the empty-store view.
    fn snapshot(&self) -> FactId;

    /// Look up a fact by id, preserving the four outcomes (active, retracted,
    /// future, unknown).
    fn fact(&self, fact_id: FactId) -> impl Future<Output = FactLookupOutput<S>> + Send;

    /// Whether a commit with `id` was recorded at-or-before this snapshot. A
    /// retracted commit still counts as existing — it was once recorded, so
    /// re-retracting it targets something real.
    ///
    /// Commit visibility isn't gated by a `FactId` bound: the submit pipeline
    /// records a commit's metadata alongside its facts, so "recorded
    /// at-or-before this snapshot" is the same cut the per-fact bound
    /// expresses. The submit-time
    /// [`RetractCommit`](crate::facts::assertions::MetaAssertion::RetractCommit)
    /// validator uses this to reject a retraction whose target was never
    /// recorded ([`SubmitError::CommitNotFound`]).
    fn commit_known(
        &self,
        id: &crate::facts::ids::CommitId,
    ) -> impl Future<Output = Result<bool, S::Error>> + Send;

    /// Where `id` sits relative to this view's snapshot — see [`FactPlacement`].
    ///
    /// A read snapshot has no in-flight commit, so it never reports `InFlight`
    /// — only `Committed` or `Absent`. A commit-in-preparation view (the submit
    /// union view) additionally reports `InFlight` for facts this commit mints.
    fn placement(&self, id: FactId)
    -> impl Future<Output = Result<FactPlacement, S::Error>> + Send;
}

/// Where a [`FactId`] sits relative to the commit a view is preparing.
///
/// The meta-rules read this to classify a retract/supersede target: a target
/// must predate the in-flight commit ([`Self::Committed`]), not be minted
/// inside it ([`Self::InFlight`]) and not be missing ([`Self::Absent`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactPlacement {
    /// No such fact at this view's snapshot — a future id or one never minted.
    Absent,
    /// Minted in the in-flight commit this view is preparing.
    InFlight,
    /// A fact at-or-before the snapshot, predating any in-flight commit.
    Committed,
}

/// Store-pinned [`StoredFact`] alias, to keep signatures returning
/// `FactPage` / `EdgeSubgraph` over a store's id shape readable.
pub type StoredFactOf<S> =
    StoredFact<<S as FactStore>::EntityId, <S as FactStore>::EventId, <S as FactStore>::ImageId>;

// ============================================================================
// EntityView — entity-parametric reads
// ============================================================================

/// Entity-parametric reads over a snapshot view. Entities have one
/// canonical equivalence (`SameEntity`) and one canonical directed-edge
/// relation (`Topological`), both implicit — no relation parameter to pass.
pub trait EntityView<S: FactStore>: FactView<S> {
    /// The class representative of `member` under `SameEntity` at this
    /// snapshot. A member with no incident edges (or unknown to the store)
    /// is its own representative.
    fn entity_representative(
        &self,
        member: &S::EntityId,
    ) -> impl Future<Output = Result<S::EntityId, S::Error>> + Send;

    /// The full `SameEntity` equivalence class of `member` at this
    /// snapshot — representative plus every member.
    fn entity_class(
        &self,
        member: &S::EntityId,
    ) -> impl Future<Output = Result<EquivClass<S::EntityId>, S::Error>> + Send;

    /// Walk entity-touching facts via an index, class-scoped by `SameEntity`.
    ///
    /// `cursor` is an inclusive lower bound: only items with `id >= cursor`
    /// are returned. First page passes `FactId::new(0)`; continuations pass
    /// the previous page's returned cursor.
    fn walk_entities<'a>(
        &'a self,
        stream: &'a EntityStream<'a>,
        cursor: FactId,
        limit: std::num::NonZeroUsize,
    ) -> impl Future<Output = Result<FactPage<StoredFactOf<S>, S::EntityId>, S::Error>> + Send + 'a;

    /// Paginated backlink walk — "which facts mention this entity?".
    /// `cursor` is an inclusive lower bound; pagination mirrors
    /// [`Self::walk_entities`].
    fn all_facts_about_entity(
        &self,
        entity: &S::EntityId,
        cursor: FactId,
        limit: std::num::NonZeroUsize,
    ) -> impl Future<Output = Result<FactPage<StoredFactOf<S>, S::EntityId>, S::Error>> + Send;

    /// Closure of edge facts reachable from `seed` via the `Topological`
    /// relation, one page at a time.
    ///
    /// `cursor` is an inclusive lower bound. The first call passes
    /// `FactId::new(0)`; later calls pass one past the previous page's highest
    /// fact id. The union of pages through [`EdgeSubgraph::truncated`] `==
    /// false` is the full closed subgraph.
    fn entity_topological_subgraph(
        &self,
        seed: S::EntityId,
        cursor: FactId,
        limit: std::num::NonZeroUsize,
    ) -> impl Future<Output = Result<EdgeSubgraph<S::EntityId, StoredFactOf<S>>, S::Error>> + Send;
}

// ============================================================================
// EventView — event-parametric reads
// ============================================================================

/// Event-parametric reads over a snapshot view. Events have one canonical
/// equivalence (`SameEvent`), implicit; no edge relations today.
pub trait EventView<S: FactStore>: FactView<S> {
    /// The class representative of `member` under `SameEvent` at this
    /// snapshot.
    fn event_representative(
        &self,
        member: &S::EventId,
    ) -> impl Future<Output = Result<S::EventId, S::Error>> + Send;

    /// The full `SameEvent` equivalence class of `member` at this snapshot.
    fn event_class(
        &self,
        member: &S::EventId,
    ) -> impl Future<Output = Result<EquivClass<S::EventId>, S::Error>> + Send;

    /// Walk event-touching facts via an index, class-scoped by
    /// `SameEvent`. See [`EntityView::walk_entities`] for pagination.
    fn walk_events<'a>(
        &'a self,
        stream: &'a EventStream<'a>,
        cursor: FactId,
        limit: std::num::NonZeroUsize,
    ) -> impl Future<Output = Result<FactPage<StoredFactOf<S>, S::EventId>, S::Error>> + Send + 'a;

    /// Paginated backlink walk — "which facts mention this event?".
    fn all_facts_about_event(
        &self,
        event: &S::EventId,
        cursor: FactId,
        limit: std::num::NonZeroUsize,
    ) -> impl Future<Output = Result<FactPage<StoredFactOf<S>, S::EventId>, S::Error>> + Send;
}

// ============================================================================
// ImageView — image-parametric reads
// ============================================================================

/// Image-parametric reads over a snapshot view. Images have one canonical
/// equivalence (`SameArtifact`), implicit; no edge relations today.
pub trait ImageView<S: FactStore>: FactView<S> {
    /// The class representative of `member` under `SameArtifact` at this
    /// snapshot.
    fn image_representative(
        &self,
        member: &S::ImageId,
    ) -> impl Future<Output = Result<S::ImageId, S::Error>> + Send;

    /// The full `SameArtifact` equivalence class of `member` at this
    /// snapshot.
    fn image_class(
        &self,
        member: &S::ImageId,
    ) -> impl Future<Output = Result<EquivClass<S::ImageId>, S::Error>> + Send;

    /// Walk image-touching facts via an index, class-scoped by
    /// `SameArtifact`. See [`EntityView::walk_entities`] for pagination.
    fn walk_images<'a>(
        &'a self,
        stream: &'a ImageStream<'a>,
        cursor: FactId,
        limit: std::num::NonZeroUsize,
    ) -> impl Future<Output = Result<FactPage<StoredFactOf<S>, S::ImageId>, S::Error>> + Send + 'a;

    /// Paginated backlink walk — "which facts mention this image?".
    fn all_facts_about_image(
        &self,
        image: &S::ImageId,
        cursor: FactId,
        limit: std::num::NonZeroUsize,
    ) -> impl Future<Output = Result<FactPage<StoredFactOf<S>, S::ImageId>, S::Error>> + Send;
}
