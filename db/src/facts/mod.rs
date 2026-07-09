//! SQLite [`FactStore`] backend.
//!
//! Rides the crate's shared pool (SpatiaLite loaded, WAL):
//! [`SqliteFactStore::new`] wraps a pool the caller built, so the fact-store
//! tables live beside the rest of the schema in one database.
//!
//! ## Transactions
//!
//! [`FactStore::with_tx`] opens `BEGIN IMMEDIATE` — the write lock up front,
//! so the whole match → mint → validate → stage sequence runs under writer
//! exclusion. Reads through [`SqliteTx`] see committed rows plus the
//! transaction's own staging (the connection's uncommitted-read visibility);
//! everything applies together on `Ok` or rolls back on `Err`.
//!
//! Each submit runs inside a submit scope — a sqlx savepoint transaction
//! nested in the write transaction — so a rejected submit unwinds its
//! staging and mints. Staged rows carry a NULL `commit_seq` until their
//! commit claims them; a claim that updates no row refuses on the spot, and
//! the pre-commit audit refuses the transaction if any row is left
//! unclaimed.
//!
//! ## Reads
//!
//! A [`SqliteFactView`] owns one pooled connection for its lifetime, inside
//! a deferred read transaction; dropping the view rolls it back and returns
//! the connection. WAL latches the read snapshot at the first read
//! statement, not at BEGIN — the fact-id bound is the semantic snapshot.
//! Retraction and equivalence resolve through the backend-shared algorithms
//! in [`chronoscope_core::store::retraction`] and
//! [`chronoscope_core::store::equiv`] over edges fetched by recursive CTE —
//! see [`read`] for the shapes. The class-stream walks
//! (`walk_entity_classes`, `walk_events`, `walk_image_classes`) answer empty
//! pages, the same stub shape as the in-memory backend's `walk_events`; the
//! conformance suite marks their cases ignored for this backend. The submit
//! matcher finds candidates only through these walks, so cross-commit
//! identity matching is inert on this backend until they land.

mod convert;
mod error;
mod queries;
mod read;
mod storage;

pub mod ids;

#[cfg(test)]
mod tests;

use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;

use sqlx::sqlite::SqlitePool;
use sqlx::{Acquire, Sqlite, SqliteConnection, Transaction};

use chronoscope_core::grammar::ids::{CommitId, FactId, SubjectKind};
use chronoscope_core::store::schema::{
    ClassPage, EntityStream, EquivClass, EventStream, FactPage, ImageStream,
};
use chronoscope_core::store::{
    ClassWalkPage, EntityView, EventView, FactPlacement, FactStore, FactView, FactWrite, ImageView,
    StoredFactOf, WalkPage,
};
use chronoscope_core::submit::{FactLookup, StoredCommit, StoredFact, SubmitResult};

pub use self::error::SqliteFactStoreError;
pub use self::ids::{SqliteEntityId, SqliteEventId, SqliteIds, SqliteImageId};

use self::error::sql;
use self::read::ReadBound;
use self::storage::{commit_to_json, facet_columns, fact_to_json, result_to_json, subject_rows};

use self::convert::{i64_to_u64, u64_to_i64};

pub(crate) use self::queries::verify_query_plans;

// Aliases to keep the spellings short.
type SqlStoredFact = StoredFact<SqliteIds>;
type SqlFactLookup = FactLookup<SqliteIds>;
type SqlSubmitResult = SubmitResult<SqliteIds>;
type Error = SqliteFactStoreError;

// ============================================================================
// SqliteFactStore
// ============================================================================

/// SQLite implementation of [`FactStore`] over a shared [`SqlitePool`].
#[derive(Debug, Clone)]
pub struct SqliteFactStore {
    pool: SqlitePool,
}

impl SqliteFactStore {
    /// Wrap an already-built pool (migrations run, SpatiaLite loaded).
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

// ============================================================================
// Connection forms — the storage type is the handle's mode
// ============================================================================

/// A committed read view's connection: an owned deferred read transaction,
/// rolled back (returning its connection to the pool) on drop.
pub struct ViewTx(Transaction<'static, Sqlite>);

/// The `with_tx` write handle's connection, borrowed from the immediate
/// transaction the frame owns and must recover to commit. The marker makes
/// `'t` invariant — it is [`SqliteTx`]'s brand, and a covariant borrow
/// would let two `with_tx` closures' brands unify.
pub struct FrameConn<'t>(&'t mut SqliteConnection, PhantomData<fn(&'t ()) -> &'t ()>);

/// A submit scope's connection: an owned savepoint transaction nested in
/// the write transaction.
pub struct ScopeTx<'n>(Transaction<'n, Sqlite>);

/// The connection behind a handle. Transactions are connection-scoped, so a
/// handle is one connection for its lifetime either way; the three forms
/// differ only in ownership.
pub trait AsConn: conn_sealed::Sealed + Send + Sync {
    fn conn(&mut self) -> &mut SqliteConnection;
}

/// The connection forms carrying the write surface: [`FactWrite`] exists
/// only over these, so a read view lacks the write methods at compile time.
pub trait WriteConn: AsConn {}

mod conn_sealed {
    pub trait Sealed {}
    impl Sealed for super::ViewTx {}
    impl Sealed for super::FrameConn<'_> {}
    impl Sealed for super::ScopeTx<'_> {}
}

impl AsConn for ViewTx {
    fn conn(&mut self) -> &mut SqliteConnection {
        &mut self.0
    }
}

impl AsConn for FrameConn<'_> {
    fn conn(&mut self) -> &mut SqliteConnection {
        self.0
    }
}

impl AsConn for ScopeTx<'_> {
    fn conn(&mut self) -> &mut SqliteConnection {
        &mut self.0
    }
}

impl WriteConn for FrameConn<'_> {}
impl WriteConn for ScopeTx<'_> {}

// ============================================================================
// SqliteHandle — the one read/write handle over a connection
// ============================================================================

/// Read (and, for the write forms, write) handle over one connection. Core's
/// view traits are foreign in this crate, so they can't hang off a local
/// source trait the way the in-memory backend's do (orphan rule); this one
/// concrete type carries each view-trait impl once for every connection
/// form, and keeps its fields private so handles come only from the store.
pub struct SqliteHandle<C> {
    conn: C,
    bound: ReadBound,
}

/// Snapshot-scoped read view: an owned deferred read transaction plus the
/// pinned exclusive upper bound.
pub type SqliteFactView = SqliteHandle<ViewTx>;

/// Branded transaction handle for [`SqliteFactStore`] — the [`FactWrite`]
/// surface over one open `BEGIN IMMEDIATE` transaction.
pub type SqliteTx<'brand> = SqliteHandle<FrameConn<'brand>>;

/// Refuse the transaction if any visible `facts` row is unclaimed —
/// committed state never holds one, so a hit is this transaction's own
/// staging that no recorded commit stands behind.
async fn audit_unclaimed_staging(conn: &mut SqliteConnection) -> Result<(), Error> {
    let row: Option<(i64,)> = sqlx::query_as(queries::UNCLAIMED_STAGED_FACT.sql)
        .fetch_optional(&mut *conn)
        .await
        .map_err(sql("auditing staged rows"))?;
    match row {
        Some((fact_id,)) => Err(Error::UnclaimedStaging { fact_id }),
        None => Ok(()),
    }
}

async fn mint(conn: &mut SqliteConnection, kind: SubjectKind) -> Result<i64, Error> {
    let (query, context) = match kind {
        SubjectKind::Entity => (&queries::MINT_ENTITY, "minting entity id"),
        SubjectKind::Event => (&queries::MINT_EVENT, "minting event id"),
        SubjectKind::Image => (&queries::MINT_IMAGE, "minting image id"),
    };
    let (id,): (i64,) = sqlx::query_as(query.sql)
        .fetch_one(conn)
        .await
        .map_err(sql(context))?;
    Ok(id)
}

/// The `(entity, event, image)` mint counters, read fresh per known-id
/// check so the row stays the one source of what the store has minted.
async fn counters(conn: &mut SqliteConnection) -> Result<(i64, i64, i64), Error> {
    sqlx::query_as(queries::MINT_COUNTERS.sql)
        .fetch_one(conn)
        .await
        .map_err(sql("reading mint counters"))
}

// ============================================================================
// FactStore impl
// ============================================================================

impl FactStore for SqliteFactStore {
    type Error = SqliteFactStoreError;
    type Ids = SqliteIds;
    type Cursor = FactId;
    type ClassCursor<Rep>
        = (Rep, FactId)
    where
        Rep: Send;
    type Tx<'brand> = SqliteTx<'brand>;
    type View<'a> = SqliteFactView;

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
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(sql("opening immediate transaction"))?;
        let mut handle = SqliteHandle {
            conn: FrameConn(&mut tx, PhantomData),
            bound: ReadBound::Union,
        };
        let result = f(self, &mut handle).await;
        if result.is_ok() {
            if let Err(refusal) = audit_unclaimed_staging(&mut tx).await {
                // The refusal is the diagnosis; a rollback failure on this
                // already-doomed transaction would only mask it, and the
                // dropped transaction rolls back regardless.
                let _ = tx.rollback().await;
                return Err(refusal);
            }
            tx.commit().await.map_err(sql("committing transaction"))?;
        } else {
            // The closure's error is the diagnosis; a rollback failure here
            // would only mask it, and the dropped transaction rolls back
            // regardless.
            let _ = tx.rollback().await;
        }
        Ok(result)
    }

    async fn next_fact_id(&self) -> Result<FactId, Self::Error> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(sql("acquiring clock connection"))?;
        Ok(FactId::new(read::next_fact_id(&mut conn).await?))
    }

    async fn no_later_than(&self, snapshot: FactId) -> Result<Self::View<'_>, Self::Error> {
        let tx = self
            .pool
            .begin_with("BEGIN DEFERRED")
            .await
            .map_err(sql("opening view read transaction"))?;
        Ok(SqliteHandle {
            conn: ViewTx(tx),
            bound: ReadBound::Pinned(snapshot),
        })
    }

    async fn now(&self) -> Result<Self::View<'_>, Self::Error> {
        let mut tx = self
            .pool
            .begin_with("BEGIN DEFERRED")
            .await
            .map_err(sql("opening view read transaction"))?;
        // Reading the watermark on the view's own transaction also latches
        // its WAL read snapshot right here.
        let snapshot = FactId::new(read::next_fact_id(&mut tx).await?);
        Ok(SqliteHandle {
            conn: ViewTx(tx),
            bound: ReadBound::Pinned(snapshot),
        })
    }
}

// ============================================================================
// View-trait impls — once, generic over the connection form
// ============================================================================
//
// Every body binds the handle's bound, borrows its connection, and delegates
// to `read::*`, so the three handle shapes cannot drift.

impl<C: AsConn> FactView<SqliteFactStore> for SqliteHandle<C> {
    /// A pinned view answers its stored bound; a write handle's union bound
    /// moves as facts stage — `MAX(fact_id) + 1` over what its connection
    /// sees.
    async fn snapshot(&mut self) -> Result<FactId, Error> {
        match self.bound {
            ReadBound::Pinned(snapshot) => Ok(snapshot),
            ReadBound::Union => Ok(FactId::new(read::next_fact_id(self.conn.conn()).await?)),
        }
    }

    async fn fact(&mut self, fact_id: FactId) -> Result<SqlFactLookup, Error> {
        read::fact_lookup(self.conn.conn(), self.bound, fact_id).await
    }

    async fn commit_known(&mut self, id: &CommitId) -> Result<bool, Error> {
        read::commit_known(self.conn.conn(), id).await
    }

    async fn placement(&mut self, id: FactId) -> Result<FactPlacement, Error> {
        read::placement(self.conn.conn(), self.bound, id).await
    }
}

impl<C: AsConn> EntityView<SqliteFactStore> for SqliteHandle<C> {
    async fn entity_representative(
        &mut self,
        member: &SqliteEntityId,
    ) -> Result<SqliteEntityId, Error> {
        Ok(self.entity_class(member).await?.representative)
    }

    async fn entity_class(
        &mut self,
        member: &SqliteEntityId,
    ) -> Result<EquivClass<SqliteEntityId>, Error> {
        read::equiv_class(self.conn.conn(), self.bound, *member).await
    }

    /// The class-stream walks need the maintained-representative machinery
    /// this backend doesn't have; an empty page is the contract's
    /// nothing-found answer. It must stay quiet rather than error: the
    /// submit matcher drains these walks on every submit with `Local`
    /// decls, so an error here would break every write. The ignored
    /// conformance cases pin the gap.
    async fn walk_entity_classes<'b>(
        &'b mut self,
        _stream: &'b EntityStream<'b>,
        _after: Option<(SqliteEntityId, FactId)>,
        _limit: std::num::NonZeroUsize,
    ) -> Result<ClassWalkPage<SqliteFactStore, SqliteEntityId>, Error> {
        Ok(empty_class_page())
    }

    async fn all_facts_about_entity(
        &mut self,
        entity: &SqliteEntityId,
        after: Option<FactId>,
        limit: std::num::NonZeroUsize,
    ) -> Result<WalkPage<SqliteFactStore, SqliteEntityId>, Error> {
        read::backlink_page(self.conn.conn(), self.bound, *entity, after, limit).await
    }
}

impl<C: AsConn> EventView<SqliteFactStore> for SqliteHandle<C> {
    async fn event_representative(
        &mut self,
        member: &SqliteEventId,
    ) -> Result<SqliteEventId, Error> {
        Ok(*member)
    }

    async fn event_class(
        &mut self,
        member: &SqliteEventId,
    ) -> Result<EquivClass<SqliteEventId>, Error> {
        Ok(singleton_class(*member))
    }

    /// Mirrors the in-memory backend's identical stub: events have no
    /// stream index and no consumer — the projection reaches events through
    /// the entity's `HasEvent` hop. The suite-wide ignored event cases pin
    /// this.
    async fn walk_events<'b>(
        &'b mut self,
        _stream: &'b EventStream<'b>,
        _after: Option<FactId>,
        _limit: std::num::NonZeroUsize,
    ) -> Result<WalkPage<SqliteFactStore, SqliteEventId>, Error> {
        Ok(empty_fact_page())
    }

    async fn all_facts_about_event(
        &mut self,
        event: &SqliteEventId,
        after: Option<FactId>,
        limit: std::num::NonZeroUsize,
    ) -> Result<WalkPage<SqliteFactStore, SqliteEventId>, Error> {
        read::backlink_page(self.conn.conn(), self.bound, *event, after, limit).await
    }
}

impl<C: AsConn> ImageView<SqliteFactStore> for SqliteHandle<C> {
    async fn image_representative(
        &mut self,
        member: &SqliteImageId,
    ) -> Result<SqliteImageId, Error> {
        Ok(self.image_class(member).await?.representative)
    }

    async fn image_class(
        &mut self,
        member: &SqliteImageId,
    ) -> Result<EquivClass<SqliteImageId>, Error> {
        read::equiv_class(self.conn.conn(), self.bound, *member).await
    }

    /// Empty for the same reason as `walk_entity_classes`: no
    /// maintained-representative machinery, and the matcher drains this
    /// walk on every submit, so nothing-found must stay quiet.
    async fn walk_image_classes<'b>(
        &'b mut self,
        _stream: &'b ImageStream<'b>,
        _after: Option<(SqliteImageId, FactId)>,
        _limit: std::num::NonZeroUsize,
    ) -> Result<ClassWalkPage<SqliteFactStore, SqliteImageId>, Error> {
        Ok(empty_class_page())
    }

    async fn all_facts_about_image(
        &mut self,
        image: &SqliteImageId,
        after: Option<FactId>,
        limit: std::num::NonZeroUsize,
    ) -> Result<WalkPage<SqliteFactStore, SqliteImageId>, Error> {
        read::backlink_page(self.conn.conn(), self.bound, *image, after, limit).await
    }
}

// ============================================================================
// FactWrite impl — the write connection forms only
// ============================================================================

impl<C: WriteConn> FactWrite<SqliteFactStore> for SqliteHandle<C> {
    type Nested<'n>
        = SqliteHandle<ScopeTx<'n>>
    where
        Self: 'n;

    async fn with_submit_scope<'s, R, E, F>(&'s mut self, f: F) -> Result<Result<R, E>, Error>
    where
        Self: 's,
        R: Send,
        E: std::fmt::Debug + Send,
        F: for<'n> FnOnce(
                &'n mut Self::Nested<'s>,
            ) -> Pin<Box<dyn Future<Output = Result<R, E>> + Send + 'n>>
            + Send,
    {
        // sqlx tracks transaction depth on the connection, so this begin
        // opens a savepoint nested in the write transaction.
        let scope_tx = self
            .conn
            .conn()
            .begin()
            .await
            .map_err(sql("opening submit scope"))?;
        let mut scope = SqliteHandle {
            conn: ScopeTx(scope_tx),
            bound: ReadBound::Union,
        };
        let result = f(&mut scope).await;
        if result.is_ok() {
            scope
                .conn
                .0
                .commit()
                .await
                .map_err(sql("committing submit scope"))?;
        }
        // A failed scope's savepoint rolls back when its transaction drops.
        Ok(result)
    }

    async fn mint_entity(&mut self) -> Result<SqliteEntityId, Error> {
        Ok(SqliteEntityId(
            mint(self.conn.conn(), SubjectKind::Entity).await?,
        ))
    }

    async fn mint_event(&mut self) -> Result<SqliteEventId, Error> {
        Ok(SqliteEventId(
            mint(self.conn.conn(), SubjectKind::Event).await?,
        ))
    }

    async fn mint_image(&mut self) -> Result<SqliteImageId, Error> {
        Ok(SqliteImageId(
            mint(self.conn.conn(), SubjectKind::Image).await?,
        ))
    }

    // Ids mint dense from zero, so the known checks bound both sides — a
    // wire-supplied negative id is as unknown as one past the counter.

    async fn entity_known(&mut self, id: &SqliteEntityId) -> Result<bool, Error> {
        let (entities, _, _) = counters(self.conn.conn()).await?;
        Ok((0..entities).contains(&id.0))
    }

    async fn event_known(&mut self, id: &SqliteEventId) -> Result<bool, Error> {
        let (_, events, _) = counters(self.conn.conn()).await?;
        Ok((0..events).contains(&id.0))
    }

    async fn image_known(&mut self, id: &SqliteImageId) -> Result<bool, Error> {
        let (_, _, images) = counters(self.conn.conn()).await?;
        Ok((0..images).contains(&id.0))
    }

    async fn stage_fact(&mut self, fact: SqlStoredFact) -> Result<FactId, Error> {
        let conn = self.conn.conn();
        let facets = facet_columns(&fact)?;
        let subjects = subject_rows(&fact);
        let fact_json = fact_to_json(fact)?;
        // A RetractCommit facet stores the target's surrogate seq; an
        // unrecorded target resolves NULL, and the validator rejects the
        // bundle before anything reads it. fact_json keeps the raw hash.
        let retracts_commit_seq = match &facets.retracts_commit_id {
            Some(hash) => read::commit_seq(&mut *conn, hash).await?,
            None => None,
        };
        // SQL assigns the dense id (see INSERT_FACT); rows are never
        // deleted, so ids stay dense for the store's life.
        let staged = sqlx::query(queries::INSERT_FACT.sql)
            .bind(&fact_json)
            .bind(&facets.name_norm)
            .bind(&facets.name_language)
            .bind(&facets.external_ref)
            .bind(&facets.source_url)
            .bind(&facets.date_earliest)
            .bind(&facets.date_latest)
            .bind(facets.lat)
            .bind(facets.lon)
            .bind(facets.radius_m)
            .bind(facets.edge_kind)
            .bind(facets.edge_a)
            .bind(facets.edge_b)
            .bind(facets.retracts_fact_id)
            .bind(retracts_commit_seq)
            .execute(&mut *conn)
            .await
            .map_err(sql("staging fact row"))?;
        // fact_id is the rowid alias, so the insert's rowid is the new id.
        let fid_raw = staged.last_insert_rowid();
        for (kind, subject) in subjects {
            // current_rep seeds as the subject's own id — the representative
            // of a singleton class.
            sqlx::query(queries::INSERT_SUBJECT.sql)
                .bind(fid_raw)
                .bind(kind)
                .bind(subject)
                .bind(subject)
                .execute(&mut *conn)
                .await
                .map_err(sql("inserting fact subject row"))?;
        }
        Ok(FactId::new(i64_to_u64(fid_raw, "staged fact id")?))
    }

    async fn cached_result(&mut self, id: &CommitId) -> Result<Option<SqlSubmitResult>, Error> {
        read::cached_result(self.conn.conn(), id).await
    }

    async fn record_commit(
        &mut self,
        commit: StoredCommit,
        result: &SqlSubmitResult,
    ) -> Result<(), Error> {
        let conn = self.conn.conn();
        let commit_json = commit_to_json(&commit, result)?;
        let result_json = result_to_json(result)?;
        let inserted = sqlx::query(queries::INSERT_COMMIT.sql)
            .bind(commit.commit_id.as_str())
            .bind(&commit_json)
            .bind(&result_json)
            .execute(&mut *conn)
            .await
            .map_err(sql("recording commit metadata"))?;
        // commit_seq is the rowid alias, so the insert's rowid is the seq.
        let commit_seq = inserted.last_insert_rowid();
        // The `commit_seq IS NULL` guard makes a claim of an unknown or
        // already-owned row update nothing — refused here, named.
        for fid in &commit.fact_ids {
            let fid_bind = u64_to_i64(fid.get(), "recorded fact id")?;
            let claimed = sqlx::query(queries::CLAIM_FACT.sql)
                .bind(commit_seq)
                .bind(fid_bind)
                .execute(&mut *conn)
                .await
                .map_err(sql("claiming recorded fact"))?;
            if claimed.rows_affected() != 1 {
                return Err(Error::FactClaim {
                    commit_id: commit.commit_id.as_str().to_owned(),
                    fact_id: fid.get(),
                });
            }
        }
        Ok(())
    }
}

// ============================================================================
// Empty-page shapes
// ============================================================================

fn empty_class_page<Rep>() -> ClassPage<Rep, (Rep, FactId)> {
    ClassPage {
        rows: Vec::new(),
        next: None,
        next_class: None,
    }
}

fn empty_fact_page<S>() -> FactPage<StoredFactOf<SqliteFactStore>, S, FactId> {
    FactPage {
        items: Vec::new(),
        next_cursor: None,
    }
}

fn singleton_class<S: Ord + Copy>(member: S) -> EquivClass<S> {
    EquivClass {
        representative: member,
        members: std::iter::once(member).collect(),
    }
}

/// Negative compile-time check that the write handle is invariant in its
/// brand lifetime: shrinking the brand is a coercion the compiler must
/// refuse, or two `with_tx` closures' brands could unify and a handle could
/// cross stores.
///
/// ```compile_fail
/// use chronoscope_db::facts::{FrameConn, SqliteHandle};
///
/// fn shrink_brand<'long: 'short, 'short>(
///     handle: SqliteHandle<FrameConn<'long>>,
/// ) -> SqliteHandle<FrameConn<'short>> {
///     handle
/// }
/// ```
#[cfg(doctest)]
struct BrandIsInvariant;
