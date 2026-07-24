//! Persistence layer — holding facts and serving them back.
//!
//! The store owns the [`FactStore`] / [`FactView`] trait surface plus the
//! machinery around it:
//!
//! - [`schema`] — the read query vocabulary ([`schema::Viewport`],
//!   [`schema::TimeRange`], [`schema::FactPage`], [`schema::EquivClass`]) and
//!   the per-subject stream enums ([`schema::EntityStream`] /
//!   [`schema::ImageStream`]). Each subject kind has a
//!   single canonical equivalence (and entities a single canonical edge
//!   relation), all implicit — there are no per-subject relation enums.
//! - [`pagination`] — the generic cursor→stream adapter every walk pages on,
//!   plus the backend-shared class/depiction pagers over materialized row
//!   sets.
//! - [`equiv`] / [`retraction`] — backend-shared resolution: equivalence
//!   components over identity edges and the effective-retraction fixpoint.
//!   Each backend fetches edges its own way; the resolution runs here so
//!   backends can't drift.
//! - [`memory`] — the in-memory [`memory::MemoryFactStore`] backend.
//! - `conformance` — backend-agnostic [`FactStore`] test suite, available to
//!   other crates behind the `test-support` feature.
//!
//! The store trait's associated types are written in [`submit`]'s
//! data types ([`Commit`](crate::submit::Commit),
//! [`StoredFact`],
//! [`SubmitResult`],
//! [`FactLookup`],
//! [`SubmitError`]), while `submit`'s pipeline and
//! matcher consume this trait. So `store` and `submit` are co-recursive peers,
//! read together, rather than a clean one-directional stack.
//!
//! - [`FactStore`] is the writeable handle: it owns the clock
//!   ([`FactStore::next_fact_id`], [`FactStore::now`]), accepts commits
//!   ([`FactStore::submit_commit`]), and builds snapshot-scoped read views
//!   ([`FactStore::no_later_than`]).
//! - [`FactView`] is a snapshot-scoped read handle with subject-agnostic
//!   methods (fact lookup, snapshot inquiry).
//!
//! Subject-parametric reads live on one companion trait per subject kind:
//! [`EntityView`], [`EventView`], [`ImageView`]. Each does equivalence
//! resolution, indexed walks, and backlink walks.
//!
//! A view is an exclusive read handle over one backing connection, held for
//! the view's lifetime: every reading method takes `&mut self` so a backend
//! can drive that connection (a `sqlx` executor is `&mut`). Concurrent reads
//! are more views on more connections, pinned at the same snapshot —
//! `no_later_than(view.snapshot().await?).await?`.
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
//! - [`FactStore::next_fact_id`] returns the next id the store would mint — one
//!   past the highest stored fact, or `FactId::new(0)` on an empty store.
//! - `no_later_than(snapshot) -> View` is an exclusive upper bound: the
//!   view exposes facts with `id < snapshot`. `FactId::new(0)` is the
//!   empty-store view; every lookup returns [`FactLookup::Future`] or
//!   [`FactLookup::Unknown`].
//! - [`FactView::snapshot`] returns the bound the view was pinned at.
//! - Every paginated walk pages on an opaque resume token: `None` opens the
//!   walk, `Some(token)` resumes at the previous page's token. The backlink
//!   walks (`all_facts_about_*`) page on
//!   [`Cursor`](FactStore::Cursor); the class walks (`walk_entity_classes` /
//!   `walk_image_classes`) page on [`ClassCursor`](FactStore::ClassCursor). A caller
//!   threads the token back verbatim, never constructing or inspecting it, so a
//!   walk's inclusive-vs-exclusive resume polarity stays the backend's own
//!   business. A page's token is `None` when the walk is exhausted, `Some` when
//!   there may be more, regardless of this page's size, since a conforming
//!   backend may return a short or empty page that still carries a token. Page
//!   size is never a completion signal; only the token is.
//!
//! [`FactStore::next_fact_id`] gives the scalar watermark; [`FactStore::now`] returns
//! a snapshot view directly (its method doc says why it isn't a default
//! composing the two).
//!
//! ## Transaction-scoped writes
//!
//! Callers enter a transaction via [`FactStore::with_tx`], submit commits through
//! the supplied [`FactStore::Tx`] handle, and return `Ok` to commit or
//! `Err`/panic to roll back. The transaction is the unit of atomicity: every
//! commit submitted through the handle applies together on `Ok`, and none of
//! them on `Err` — a multi-commit closure that fails partway leaves the
//! store untouched.
//!
//! The handle implements [`FactWrite`]: the full read surface over
//! committed ∪ staged state plus the primitives only a live transaction can
//! offer (minting, staging, commit recording). [`FactStore::submit_commit`] is a
//! provided method — the shared submit driver — built on those primitives,
//! so a backend implements the primitives and inherits the orchestration.
//! The submit sequence is match → resolve/mint → validate → stage. It reads
//! existing state to match declarations and check cross-fact rules before
//! staging, so the whole sequence runs under one transaction — a held mutex
//! for the in-memory backend, a SQL transaction for a SQL backend — to keep
//! a concurrent writer from slipping between the reads and the insert. The
//! driver runs each pipeline inside a submit scope
//! ([`FactWrite::with_submit_scope`]), so a rejection can't smuggle its
//! staging into history.
//!
//! The closure-scoped `with_tx` shape (rather than `begin_tx` / `commit_tx`)
//! lets the trait carry the brand-pattern `for<'brand>` HRTB.
//!
//! ## Cross-store safety
//!
//! A tx from one store can't reach another store of the same type. Each
//! `with_tx(...)` mints a fresh existential `'brand` under the `for<'brand>`
//! HRTB and hands the closure a `&'brand mut Tx<'brand>`; `&mut` is
//! invariant in its pointee, which is what keeps two calls' brands from
//! unifying. So a tx from `store_a.with_tx(...)` used inside
//! `store_b.with_tx(...)`'s closure fails to typecheck. The closure is the
//! only way to name a `Tx<'brand>`, and the returned future is bounded by
//! `'brand`, so handles can't leak past it.
//!
//! `BrandsCannotUnify`'s `compile_fail` doctest pins that guarantee, with
//! `conformance::cases::two_commits_share_one_with_tx_brand` the positive
//! intra-store check. Each backend's `Tx` additionally carries a
//! `PhantomData<fn(&'brand ()) -> &'brand ()>` marker as defense in depth:
//! it makes the handle type itself invariant, so the brand holds for an
//! owned `Tx<'brand>` as much as for the `&mut` every API hands out today.
//!
//! `'brand`'s role is the type-level tag; whether it doubles as a real
//! borrow is the backend's business.
//! [`MemoryTx`](crate::store::memory::MemoryTx) borrows the state its
//! `with_tx` frame holds; a SQL backend puts an owned `sqlx::Transaction`
//! inside `Tx<'brand>`. Either way the brand gates which `with_tx` body the
//! handle threads through.

#[cfg(any(test, feature = "test-support"))]
pub mod conformance;
pub mod equiv;
pub mod memory;
pub mod pagination;
pub mod retraction;
pub mod schema;

use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;

use crate::geo::{QuadLevel, TileId, Viewport};
use crate::grammar::ids::{CommitId, FactId, IdScheme};
use crate::nonempty::NonEmptyVec;
use crate::store::schema::{
    ClassPage, ClusterCell, DepictionPage, EntityStream, EquivClass, FactPage, ImageStream, RankKey,
};
use crate::submit;
use crate::submit::{FactLookup, StoredCommit, StoredFact, SubmitError, SubmitResult};

// ============================================================================
// Store id-projection aliases
// ============================================================================

/// The entity id kind of store `S`'s scheme.
pub type EntityIdOf<S> = <<S as FactStore>::Ids as IdScheme>::Entity;
/// The lifetime-event id kind of store `S`'s scheme.
pub type EventIdOf<S> = <<S as FactStore>::Ids as IdScheme>::Event;
/// The image id kind of store `S`'s scheme.
pub type ImageIdOf<S> = <<S as FactStore>::Ids as IdScheme>::Image;

/// Producer-form commit consumed by [`FactStore::submit_commit`], pinned to
/// a store's id scheme. An alias to keep signatures readable and clippy's
/// `type_complexity` quiet.
pub type SubmitCommitInput<S> = submit::Commit<<S as FactStore>::Ids>;

/// Output of [`FactStore::submit_commit`]. The [`SubmitCommitError`] carries
/// the store's id scheme so a rejection surfaces the offending id typed, not
/// stringified.
pub type SubmitCommitOutput<S> = Result<
    SubmitResult<<S as FactStore>::Ids>,
    SubmitCommitError<<S as FactStore>::Error, <S as FactStore>::Ids>,
>;

/// Result of [`FactView::fact`] for a view over store `S`.
pub type FactLookupOutput<S> = Result<FactLookup<<S as FactStore>::Ids>, <S as FactStore>::Error>;

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

    /// The persistent id scheme this backend mints and references — its
    /// entity / event / image id kinds bundled behind one
    /// [`IdScheme`]. Shape varies by backend (`u64`-newtypes in-memory,
    /// something else for SQL); the trait pins only [`IdScheme`], whose
    /// associated [`SchemeId`](crate::grammar::ids::SchemeId) kinds carry the
    /// clone / order / hash / serde / schema / display bounds the store, view,
    /// and submit surface use.
    type Ids: IdScheme;

    /// Opaque pagination cursor. Each page of a paginated walk
    /// (`all_facts_about_*`, `walk_*`) reports a `next_cursor` of this type;
    /// a generic caller threads it straight back as the next `after` without
    /// constructing or inspecting it. Its shape is the backend's — a [`FactId`]
    /// in-memory, a compound key for a SQL backend keying off several columns.
    ///
    /// `Send` because the walk futures are `Send` and the cursor rides inside
    /// one, both in each page and in `paginate`'s
    /// resume state.
    type Cursor: Send;

    /// Opaque cursor for a class walk over subject `Rep`. Kept apart from
    /// [`Self::Cursor`] because a class walk pages a `(representative, fact_id)`
    /// key, not a bare fact id — in-memory it is `(Rep, FactId)`, a SQL backend
    /// a compound key over the same two columns. A generic caller threads it
    /// straight back as the next `after` without inspecting it.
    ///
    /// `Send` for the same reason as [`Self::Cursor`]; `Rep: Send` since the
    /// cursor embeds the representative.
    type ClassCursor<Rep>: Send
    where
        Rep: Send;

    /// Branded transaction handle threaded through [`Self::submit_commit`].
    /// `'brand` is a fresh existential minted per [`Self::with_tx`] call; it
    /// tags handles to their call site so the type system can refuse
    /// cross-instance misuse, and has no runtime role.
    ///
    /// [`FactWrite`] because the handle is the write surface the provided
    /// [`Self::submit_commit`] drives: reads over committed ∪ staged state
    /// plus minting, staging, and commit recording. The in-memory backend's
    /// handle borrows transaction state held by its `with_tx` frame — an
    /// owned guard in the handle would keep the closure's borrow alive past
    /// the apply step. A SQL backend can own its `sqlx::Transaction` in the
    /// handle. Dropping the handle before [`Self::with_tx`]'s closure
    /// completes rolls back.
    type Tx<'brand>: FactWrite<Self> + Send + 'brand
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
    /// The store opens the transaction, runs `f`, commits on `Ok(_)` and
    /// rolls back on `Err(_)` — the closure's `Result` is the commit
    /// decision, which is why the signature pins it rather than an opaque
    /// output. Everything staged through the handle applies together or not
    /// at all. Backend-level failures (`begin`, `commit`) flow through the
    /// outer `Result`; the closure's own outcome comes back inside it —
    /// typically `E = SubmitCommitError<Self::Error>`.
    ///
    /// Every read inside the closure goes through the tx handle. The store's
    /// own read surface is off-limits for the closure's duration: the
    /// in-memory backend holds the writer lock, so a store-level read inside
    /// the closure deadlocks, and a SQL backend's store-level reads see only
    /// committed state — a torn view of the transaction.
    fn with_tx<F, T, E>(
        &self,
        f: F,
    ) -> impl Future<Output = Result<Result<T, E>, Self::Error>> + Send
    where
        F: for<'brand> FnOnce(
                &'brand Self,
                &'brand mut Self::Tx<'brand>,
            )
                -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'brand>>
            + Send,
        T: Send,
        E: Send;

    /// Submit a commit bundle inside the supplied transaction. Derives the
    /// [`CommitId`] via `Commit::id()` (JCS + SHA-256) and dedups against the
    /// commit cache, resolves every declaration to a persistent id
    /// (`Existing` passes through, `Local` mints fresh), rewrites each fact's
    /// bundle-local indices to the resolved ids, and stages. A matcher match
    /// on a `Local` decl is persisted as a machine-authored identity judgment
    /// in a companion commit, reported via
    /// `SubmitResult::companion_commit_id`.
    ///
    /// Provided: the default body is the shared submit driver over the
    /// [`FactWrite`] primitives on [`Self::Tx`], so backends implement the
    /// primitives and inherit the orchestration.
    ///
    /// The driver runs the pipeline inside a submit scope (see
    /// [`FactWrite::with_submit_scope`]): after a failed submit nothing it
    /// staged or minted is observable, though a backend that cannot unwind
    /// voids the whole transaction — on error, return `Err` from the
    /// `with_tx` closure and retry in a fresh one.
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
        'brand: 'tx,
    {
        submit::driver::drive_submit::<Self, Self::Tx<'brand>>(tx, commit)
    }

    /// The next [`FactId`] this store would mint — one past the highest
    /// stored fact, or `FactId::new(0)` on an empty store (the value
    /// [`Self::no_later_than`] accepts to pin an empty-store view).
    fn next_fact_id(&self) -> impl Future<Output = Result<FactId, Self::Error>> + Send;

    /// A read view pinned at `snapshot` (exclusive upper bound): the view
    /// exposes facts with `id < snapshot`. `FactId::new(0)` is the
    /// empty-store view — every lookup returns `Future` / `Unknown`.
    ///
    /// Constructing a view acquires its backing connection, which can wait
    /// or fail; the view holds that one connection for its lifetime, and
    /// concurrent reads are more views on more connections.
    fn no_later_than(
        &self,
        snapshot: FactId,
    ) -> impl Future<Output = Result<Self::View<'_>, Self::Error>> + Send;

    /// Snapshot the current latest state. Implemented directly so a SQL
    /// backend can read the watermark on the view's own connection rather
    /// than the extra checkout a default composing [`Self::next_fact_id`] +
    /// [`Self::no_later_than`] would take. Either form is a snapshot at some
    /// recent point; a writer landing mid-read isn't included.
    fn now(&self) -> impl Future<Output = Result<Self::View<'_>, Self::Error>> + Send;
}

/// Aggregate error for `submit_commit`: a submit-pipeline domain error, a
/// backend failure, or a driver-level failure, in one enum so the return
/// type stays `Result<_, _>`. `Submit` carries every rule violation the
/// pipeline found in one batch; backend failures carry backend diagnostics.
///
/// `E` is the backend error; `R` is the backend's id scheme, threaded into
/// [`SubmitError`] so a rejection carries the offending id typed. The scheme
/// appears only in the `Submit` arm. The driver arms hold rendered `String`
/// messages: their sources (`serde_json::Error`, formatted diagnoses) aren't
/// `Clone`/`Eq`, and this type is value-compared in tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitCommitError<E, R: IdScheme> {
    /// A submit-pipeline run rejected the bundle, carrying the non-empty
    /// batch of every rule it violated.
    Submit(NonEmptyVec<SubmitError<R::Entity, R::Event, R::Image>>),
    /// A backend failure (I/O, transaction abort, etc.).
    Backend(E),
    /// Commit-id derivation failed — JCS encoding or digest parse.
    Hashing {
        /// The rendered encoding failure.
        message: String,
    },
    /// The submit driver detected a broken invariant of its own — a store
    /// bug, surfaced loudly rather than persisted.
    Internal {
        /// The rendered diagnosis, with context from the detection site.
        message: String,
    },
}

impl<E, R: IdScheme> std::fmt::Display for SubmitCommitError<E, R>
where
    E: std::fmt::Display,
    R::Entity: std::fmt::Display,
    R::Event: std::fmt::Display,
    R::Image: std::fmt::Display,
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
            Self::Hashing { message } => write!(f, "commit hash encoding failed: {message}"),
            Self::Internal { message } => {
                write!(f, "submit driver invariant violated: {message}")
            }
        }
    }
}

impl<E, R: IdScheme> std::error::Error for SubmitCommitError<E, R>
where
    E: std::fmt::Debug + std::fmt::Display,
    R::Entity: std::fmt::Display,
    R::Event: std::fmt::Display,
    R::Image: std::fmt::Display,
{
}

impl<E, R: IdScheme> From<SubmitError<R::Entity, R::Event, R::Image>> for SubmitCommitError<E, R> {
    fn from(e: SubmitError<R::Entity, R::Event, R::Image>) -> Self {
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
/// The handle is exclusive: reads take `&mut self` so a backend can serve
/// them off a `&mut` connection or transaction. For concurrent reads, pin
/// more views at the same snapshot via
/// `no_later_than(view.snapshot().await?).await?`.
///
/// Generic over the [`FactStore`] `S`: id kinds and error type come from `S`,
/// so the trait has no associated types of its own.
pub trait FactView<S: FactStore>: Send + Sync {
    /// The exclusive-upper-bound [`FactId`] this view answers under; it
    /// exposes facts with `id < snapshot()`. `FactId::new(0)` is the
    /// empty-store view. A pinned view answers its stored bound without I/O;
    /// a live transaction computes it, and it moves as facts stage.
    fn snapshot(&mut self) -> impl Future<Output = Result<FactId, S::Error>> + Send;

    /// Look up a fact by id, preserving the four outcomes (active, retracted,
    /// future, unknown).
    fn fact(&mut self, fact_id: FactId) -> impl Future<Output = FactLookupOutput<S>> + Send;

    /// Whether a commit with `id` was recorded at-or-before this snapshot. A
    /// retracted commit still counts as existing — it was once recorded, so
    /// re-retracting it targets something real.
    ///
    /// Commit visibility isn't gated by a `FactId` bound: the submit pipeline
    /// records a commit's metadata alongside its facts, so "recorded
    /// at-or-before this snapshot" is the same cut the per-fact bound
    /// expresses. The submit-time
    /// [`RetractCommit`](crate::grammar::assertions::MetaAssertion::RetractCommit)
    /// validator uses this to reject a retraction whose target was never
    /// recorded ([`SubmitError::CommitNotFound`]).
    fn commit_known(
        &mut self,
        id: &crate::grammar::ids::CommitId,
    ) -> impl Future<Output = Result<bool, S::Error>> + Send;

    /// Where `id` sits relative to this view's snapshot — see [`FactPlacement`].
    ///
    /// A read snapshot has no in-flight commit, so it never reports `InFlight`
    /// — only `Committed` or `Absent`. A commit-in-preparation view (the submit
    /// union view) additionally reports `InFlight` for facts this commit mints.
    fn placement(
        &mut self,
        id: FactId,
    ) -> impl Future<Output = Result<FactPlacement, S::Error>> + Send;
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
/// `FactPage` over a store's id shape readable.
pub type StoredFactOf<S> = StoredFact<<S as FactStore>::Ids>;

/// One page of a store `S`'s paginated walk over subject `Subj`: rows of the
/// store's stored facts resumed by its opaque [`FactStore::Cursor`]. An alias to
/// keep the walk return types readable and clippy's `type_complexity` quiet.
pub type WalkPage<S, Subj> = FactPage<StoredFactOf<S>, Subj, <S as FactStore>::Cursor>;

/// One page of a store `S`'s class walk over subject `Subj`:
/// `(representative, fact_id)` rows resumed by its opaque
/// [`FactStore::ClassCursor`]. The class-walk analogue of [`WalkPage`].
pub type ClassWalkPage<S, Subj> = ClassPage<Subj, <S as FactStore>::ClassCursor<Subj>>;

/// One page of a store `S`'s entity-depiction walk: raw depiction
/// [`StoredFact`]s under their depicted-image `SameArtifact` rep, resumed by the
/// store's opaque image [`FactStore::ClassCursor`]. The fact-carrying analogue
/// of [`ClassWalkPage`], keyed on the image id.
pub type DepictionWalkPage<S> =
    DepictionPage<StoredFactOf<S>, ImageIdOf<S>, <S as FactStore>::ClassCursor<ImageIdOf<S>>>;

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
        &mut self,
        member: &EntityIdOf<S>,
    ) -> impl Future<Output = Result<EntityIdOf<S>, S::Error>> + Send;

    /// The full `SameEntity` equivalence class of `member` at this
    /// snapshot — representative plus every member.
    fn entity_class(
        &mut self,
        member: &EntityIdOf<S>,
    ) -> impl Future<Output = Result<EquivClass<EntityIdOf<S>>, S::Error>> + Send;

    /// Walk entity-touching facts via an index as `(representative, fact_id)`
    /// rows, class-scoped by `SameEntity`.
    ///
    /// `after` is a resume token: `None` opens the walk from the first row,
    /// `Some(cursor)` resumes at the previous page's returned `next`. Rows come
    /// ordered by `(representative, fact_id)`, so a class's rows stay contiguous
    /// across page boundaries.
    ///
    /// A page's `next_class` must resume the walk strictly past the last row's
    /// representative, and must be `Some` whenever a representative greater than
    /// it remains. A rep-enumerating consumer (the viewport listing) drives the
    /// walk by `next_class` to visit each class once, so a backend that reports
    /// `None` while a greater representative remains truncates that consumer.
    fn walk_entity_classes<'a>(
        &'a mut self,
        stream: &'a EntityStream<'a>,
        after: Option<S::ClassCursor<EntityIdOf<S>>>,
        limit: std::num::NonZeroUsize,
    ) -> impl Future<Output = Result<ClassWalkPage<S, EntityIdOf<S>>, S::Error>> + Send + 'a;

    /// Cluster the entities whose location falls inside `viewport` into one
    /// [`ClusterCell`] per non-empty tile at `level`, ranked by `rank`.
    ///
    /// The tile grid is the bucketing key; the fold is viewport-free, so a
    /// fringe tile the viewport only partly covers still yields a cell. Each
    /// tile keeps its `(quadkey, fact_id)`-lowest `CLUSTER_TILE_N` candidates
    /// before retraction, folds them to their `SameEntity` representatives, and
    /// reports the minimal survivor's entity and position, classifying the cell
    /// as a singleton, a splittable cluster, or a co-located group.
    ///
    /// Unpaginated — the viewport is tile-count-bounded ([`CLUSTER_TILE_CAP`]),
    /// so the whole cell set returns at once.
    ///
    /// [`CLUSTER_TILE_N`]: crate::store::schema::CLUSTER_TILE_N
    /// [`CLUSTER_TILE_CAP`]: crate::store::schema::CLUSTER_TILE_CAP
    fn cluster_entities_in_viewport<'a>(
        &'a mut self,
        viewport: &'a Viewport,
        level: QuadLevel,
        rank: RankKey,
    ) -> impl Future<Output = Result<Vec<ClusterCell<EntityIdOf<S>>>, S::Error>> + Send + 'a;

    /// Cluster the entities inside container `tile` into one [`ClusterCell`] per
    /// non-empty sub-tile at `tile.level() + CELL_DEPTH`, ranked by `rank`.
    ///
    /// The container is one contiguous Morton block; its `4^CELL_DEPTH` children
    /// at `level + CELL_DEPTH` partition it with no gap, and each child is a
    /// bucket. Every bucket keeps its `(quadkey, fact_id)`-lowest
    /// [`CLUSTER_TILE_N`] candidates before retraction — a **per-sub-tile**
    /// budget, so a dense low-Morton corner can't starve the container's other
    /// sub-tiles — folds them to their `SameEntity` representatives, and reports
    /// the minimal survivor per bucket, classifying each cell as a singleton, a
    /// splittable cluster, or a co-located group.
    ///
    /// The cell geometry is viewport-free: a container yields the same cells
    /// however a viewport is drawn over it, so it is keyed by `(snapshot, level,
    /// x, y)`. `level + CELL_DEPTH` folds against the finest level (see
    /// [`TileId::child_ranges`]), so a container at level 24 folds as one cell.
    /// The [`TileId`] is already validated on-grid, so this read cannot fault on
    /// the coordinate.
    ///
    /// [`CLUSTER_TILE_N`]: crate::store::schema::CLUSTER_TILE_N
    /// [`CELL_DEPTH`]: crate::store::schema::CELL_DEPTH
    /// [`TileId::child_ranges`]: crate::geo::TileId::child_ranges
    fn cluster_tile_cells(
        &mut self,
        tile: TileId,
        rank: RankKey,
    ) -> impl Future<Output = Result<Vec<ClusterCell<EntityIdOf<S>>>, S::Error>> + Send;

    /// Paginated backlink walk — "which facts mention this entity?". `after` is
    /// a resume token: `None` opens the walk, `Some(cursor)` resumes at the
    /// previous page's returned `next_cursor`. Each page's `next_cursor` is an
    /// opaque token: `Some` means thread it back as the next `after` — there may
    /// be more, regardless of page size — and `None` means the walk is exhausted.
    fn all_facts_about_entity(
        &mut self,
        entity: &EntityIdOf<S>,
        after: Option<S::Cursor>,
        limit: std::num::NonZeroUsize,
    ) -> impl Future<Output = Result<WalkPage<S, EntityIdOf<S>>, S::Error>> + Send;

    /// Page the images depicting `entity` — the far end of every
    /// [`JudgmentAssertion::Depiction`](crate::grammar::assertions::JudgmentAssertion::Depiction)
    /// whose entity is in `entity`'s `SameEntity` class — as raw depiction facts
    /// under their image `SameArtifact` rep.
    ///
    /// Rows come ordered by `(image_rep, fact_id)`, so an image's depiction
    /// facts stay contiguous. A page carries whole images: `limit` counts
    /// distinct images, and every depiction fact of each included image is
    /// present, so an image never straddles a page boundary. `after` is a resume
    /// token — `None` opens the walk, `Some(cursor)` resumes at the previous
    /// page's `next_class`; `None` on a page's `next_class` means the walk is
    /// exhausted.
    fn walk_entity_depictions<'a>(
        &'a mut self,
        entity: &'a EntityIdOf<S>,
        after: Option<S::ClassCursor<ImageIdOf<S>>>,
        limit: std::num::NonZeroUsize,
    ) -> impl Future<Output = Result<DepictionWalkPage<S>, S::Error>> + Send + 'a;
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
        &mut self,
        member: &EventIdOf<S>,
    ) -> impl Future<Output = Result<EventIdOf<S>, S::Error>> + Send;

    /// The full `SameEvent` equivalence class of `member` at this snapshot.
    fn event_class(
        &mut self,
        member: &EventIdOf<S>,
    ) -> impl Future<Output = Result<EquivClass<EventIdOf<S>>, S::Error>> + Send;

    /// Paginated backlink walk — "which facts mention this event?". `after` is a
    /// resume token: `None` opens the walk, `Some(cursor)` resumes at the
    /// previous page's returned `next_cursor`.
    fn all_facts_about_event(
        &mut self,
        event: &EventIdOf<S>,
        after: Option<S::Cursor>,
        limit: std::num::NonZeroUsize,
    ) -> impl Future<Output = Result<WalkPage<S, EventIdOf<S>>, S::Error>> + Send;

    /// Every `HasEvent` ever asserted on `event`, active *or retracted*,
    /// ascending by fact id. Unlike [`Self::all_facts_about_event`] this keeps
    /// retracted facts and returns only `HasEvent` facts.
    ///
    /// The submit-time ownership rule reads this to pin an event's
    /// `{entity, kind}` at its earliest-ever `HasEvent`: seeing the retracted
    /// original is what lets it reject a re-home or re-type staged as a
    /// retract-plus-re-add, which the active-only neighbourhood can't see. Page
    /// semantics match the other backlink walks — a page may come back short (a
    /// non-`HasEvent` candidate is skipped without consuming a slot) yet still
    /// carry a resume cursor.
    fn all_has_events_about_event(
        &mut self,
        event: &EventIdOf<S>,
        after: Option<S::Cursor>,
        limit: std::num::NonZeroUsize,
    ) -> impl Future<Output = Result<WalkPage<S, EventIdOf<S>>, S::Error>> + Send;
}

// ============================================================================
// ImageView — image-parametric reads
// ============================================================================

/// Image-parametric reads over a snapshot view. Images have one canonical
/// equivalence (`SameArtifact`), implicit; no edge relations today.
pub trait ImageView<S: FactStore>: FactView<S> {
    /// The `SameArtifact` representative of each `member` at this snapshot, in
    /// one resolution. A member with no `SameArtifact` class maps to itself.
    /// This is the image view's representative primitive;
    /// [`image_representative`](Self::image_representative) derives the
    /// singleton case from it.
    fn image_representatives(
        &mut self,
        members: &[ImageIdOf<S>],
    ) -> impl Future<
        Output = Result<std::collections::HashMap<ImageIdOf<S>, ImageIdOf<S>>, S::Error>,
    > + Send;

    /// The class representative of `member` under `SameArtifact` at this
    /// snapshot — the singleton case of
    /// [`image_representatives`](Self::image_representatives). A member with no
    /// `SameArtifact` class is its own representative.
    fn image_representative(
        &mut self,
        member: &ImageIdOf<S>,
    ) -> impl Future<Output = Result<ImageIdOf<S>, S::Error>> + Send {
        async move {
            let reps = self
                .image_representatives(std::slice::from_ref(member))
                .await?;
            Ok(reps.get(member).cloned().unwrap_or_else(|| member.clone()))
        }
    }

    /// The full `SameArtifact` equivalence class of `member` at this
    /// snapshot.
    fn image_class(
        &mut self,
        member: &ImageIdOf<S>,
    ) -> impl Future<Output = Result<EquivClass<ImageIdOf<S>>, S::Error>> + Send;

    /// Walk image-touching facts via an index as `(representative, fact_id)`
    /// rows, class-scoped by `SameArtifact`. See
    /// [`EntityView::walk_entity_classes`] for the row shape and pagination.
    fn walk_image_classes<'a>(
        &'a mut self,
        stream: &'a ImageStream<'a>,
        after: Option<S::ClassCursor<ImageIdOf<S>>>,
        limit: std::num::NonZeroUsize,
    ) -> impl Future<Output = Result<ClassWalkPage<S, ImageIdOf<S>>, S::Error>> + Send + 'a;

    /// Paginated backlink walk — "which facts mention this image?". `after` is a
    /// resume token: `None` opens the walk, `Some(cursor)` resumes at the
    /// previous page's returned `next_cursor`.
    fn all_facts_about_image(
        &mut self,
        image: &ImageIdOf<S>,
        after: Option<S::Cursor>,
        limit: std::num::NonZeroUsize,
    ) -> impl Future<Output = Result<WalkPage<S, ImageIdOf<S>>, S::Error>> + Send;
}

// ============================================================================
// FactWrite — transaction-scoped write surface
// ============================================================================

/// The write surface a live transaction offers: the full read surface over
/// committed ∪ staged state (the view supertraits) plus the primitives only
/// an open transaction can supply — minting, staging, and commit recording.
///
/// Bound on [`FactStore::Tx`], so the provided
/// [`submit_commit`](FactStore::submit_commit) — the shared submit driver —
/// runs against any backend's transaction. Reads through the handle see
/// committed facts plus everything staged earlier in the same transaction; a
/// staged fact's id is provisional only in that the transaction may roll
/// back — commit assigns it unchanged.
pub trait FactWrite<S: FactStore>:
    FactView<S> + EntityView<S> + EventView<S> + ImageView<S>
{
    /// Mint a fresh entity id. Durable only if the transaction commits.
    fn mint_entity(&mut self) -> impl Future<Output = Result<EntityIdOf<S>, S::Error>> + Send;

    /// Mint a fresh lifetime-event id. Durable only if the transaction
    /// commits.
    fn mint_event(&mut self) -> impl Future<Output = Result<EventIdOf<S>, S::Error>> + Send;

    /// Mint a fresh image id. Durable only if the transaction commits.
    fn mint_image(&mut self) -> impl Future<Output = Result<ImageIdOf<S>, S::Error>> + Send;

    /// Whether `id` was minted by this store — committed or earlier in this
    /// transaction. Gates a `Decl::Existing` before resolution.
    fn entity_known(
        &mut self,
        id: &EntityIdOf<S>,
    ) -> impl Future<Output = Result<bool, S::Error>> + Send;

    /// The event analogue of [`Self::entity_known`].
    fn event_known(
        &mut self,
        id: &EventIdOf<S>,
    ) -> impl Future<Output = Result<bool, S::Error>> + Send;

    /// The image analogue of [`Self::entity_known`].
    fn image_known(
        &mut self,
        id: &ImageIdOf<S>,
    ) -> impl Future<Output = Result<bool, S::Error>> + Send;

    /// Stage a substituted fact, returning the [`FactId`] it holds from here
    /// on — visible to every read through this handle immediately, durable
    /// when the transaction commits. Ids ascend in staging order.
    fn stage_fact(
        &mut self,
        fact: StoredFactOf<S>,
    ) -> impl Future<Output = Result<FactId, S::Error>> + Send;

    /// The cached [`SubmitResult`] under a [`CommitId`], from committed state
    /// or a commit recorded earlier in this transaction. Content-address
    /// dedup reads this so a re-submitted bundle replays its original result
    /// instead of re-running the pipeline.
    fn cached_result(
        &mut self,
        id: &CommitId,
    ) -> impl Future<Output = Result<Option<SubmitResult<S::Ids>>, S::Error>> + Send;

    /// Record a commit's metadata and cache its result for dedup. Recording
    /// also moves exactly the commit's own staged facts (its
    /// `StoredCommit::fact_ids`) behind the committed/in-flight boundary
    /// [`FactView::placement`] reports, so a later commit in the same
    /// transaction can retract them; other staged facts stay in flight until
    /// their own commit records.
    fn record_commit(
        &mut self,
        commit: StoredCommit,
        result: &SubmitResult<S::Ids>,
    ) -> impl Future<Output = Result<(), S::Error>> + Send;

    /// The write handle a submit scope lends its closure — the same write
    /// surface, one scope deeper.
    type Nested<'n>: FactWrite<S> + Send
    where
        Self: 'n;

    /// Run `f` inside a submit scope: keep its staging and mints on inner
    /// `Ok`, unwind them on inner `Err` — [`FactStore::with_tx`] in
    /// miniature, and scopes nest (a producer commit's encloses its
    /// companion's). How a backend unwinds is its own business: a SQL
    /// savepoint rolls back; the in-memory overlay poisons the transaction,
    /// refusing all further work. The driver wraps every commit's pipeline
    /// run in one, so a rejected submit's leftovers never share the
    /// transaction with later submits, and the inner `Err` flows out
    /// untouched. `E: Debug` lets a backend that voids the transaction
    /// record the failure as its refusal cause.
    fn with_submit_scope<'s, R, E, F>(
        &'s mut self,
        f: F,
    ) -> impl Future<Output = Result<Result<R, E>, S::Error>> + Send
    where
        Self: 's,
        R: Send,
        E: Debug + Send,
        F: for<'n> FnOnce(
                &'n mut Self::Nested<'s>,
            ) -> Pin<Box<dyn Future<Output = Result<R, E>> + Send + 'n>>
            + Send;
}

/// Negative compile-time check for the cross-store guarantee: a tx opened by
/// one store's [`FactStore::with_tx`] can't be used inside another store's
/// `with_tx` closure. Written against the in-memory backend, since the
/// mechanism is the trait signature's, not any one backend's.
///
/// `swap` demands the two brands unify; the `async fn` is never called, and an
/// uncalled one is still typechecked. [`BrandsNestWithoutSmuggling`] is the
/// positive control: the same nesting minus the cross-use, as a normal
/// doctest. A `compile_fail` block passes on any compile error, so the pair is
/// what pins the rejection to the smuggle — break the shape itself and the
/// control fails loudly instead of this block passing for the wrong reason.
///
/// ```compile_fail
/// use chronoscope_core::store::FactStore;
/// use chronoscope_core::store::memory::MemoryFactStore;
///
/// async fn smuggle() {
///     let a = MemoryFactStore::new();
///     let b = MemoryFactStore::new();
///     let _ = a
///         .with_tx(move |_, ta| {
///             Box::pin(async move {
///                 let _ = b
///                     .with_tx(move |_, tb| {
///                         Box::pin(async move {
///                             std::mem::swap(ta, tb);
///                             Ok::<(), ()>(())
///                         })
///                     })
///                     .await;
///                 Ok::<(), ()>(())
///             })
///         })
///         .await;
/// }
/// ```
#[cfg(doctest)]
struct BrandsCannotUnify;

/// Positive control for [`BrandsCannotUnify`]: two stores' `with_tx` calls
/// nest, each closure using only its own tx, and that compiles.
///
/// Same shape, same backend, same uncalled-`async fn` trick — only the smuggle
/// is gone. Each handle is put to work (a mint) so the brands are exercised
/// rather than merely bound.
///
/// ```
/// use chronoscope_core::store::memory::MemoryFactStore;
/// use chronoscope_core::store::{FactStore, FactWrite};
///
/// async fn nest() {
///     let a = MemoryFactStore::new();
///     let b = MemoryFactStore::new();
///     let _ = a
///         .with_tx(move |_, ta| {
///             Box::pin(async move {
///                 let _ = ta.mint_entity().await;
///                 let _ = b
///                     .with_tx(move |_, tb| {
///                         Box::pin(async move {
///                             let _ = tb.mint_entity().await;
///                             Ok::<(), ()>(())
///                         })
///                     })
///                     .await;
///                 Ok::<(), ()>(())
///             })
///         })
///         .await;
/// }
/// ```
#[cfg(doctest)]
struct BrandsNestWithoutSmuggling;
