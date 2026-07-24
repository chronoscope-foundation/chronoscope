//! Postgres [`FactStore`] backend. Retraction lands (the recursive CTEs
//! rewritten to a single self-reference each — see the `queries` module); the
//! `PostGIS` spatial reads remain a later unit.
//!
//! One writable database, no base/overlay union: every read names its tables
//! directly and every query is a plain constant (see the `queries` module). The
//! json columns are `JSONB`; the shared [`crate::common::storage`] string codecs
//! bind through a `$N::jsonb` cast and read back through `col::text`.
//!
//! ## Transactions
//!
//! [`FactStore::with_tx`] opens a READ COMMITTED transaction and immediately
//! takes the counters-row lock (`SELECT ... FOR UPDATE`), held through COMMIT.
//! That single lock serializes the whole match -> mint -> stage sequence — the
//! Postgres analogue of SQLite's `BEGIN IMMEDIATE` — and, because it is held to
//! commit, makes fact-id-assignment order == commit-visibility order, so the
//! scalar [`FactId`] snapshot stays sound. Ids are counter-minted from the
//! `fact_counters` row (subjects, fact ids, and commit seqs alike); a rolled-back
//! submit scope unwinds the counter bumps with its staging.
//!
//! ## Reads
//!
//! A [`PostgresFactView`] owns one pooled connection inside a read transaction;
//! the `WHERE fact_id < N` bound is the semantic snapshot, so consistency comes
//! from the commit-order == id-order invariant rather than the transaction
//! isolation. The read transaction pins READ COMMITTED for that reason: a long
//! paginated view runs to completion under any ambient database default.
//! Representatives and classes read the append-only `subject_reps`
//! log; the class-stream `ByName` / `ByExternalReference` / `BySourceUrl` / `All`
//! walks combine the facet indexes with that resolution, and depictions combine
//! it with the subject backlinks.
//!
//! ## Deferred (later units)
//!
//! The `PostGIS` spatial reads/writes and the temporal-conflict witness *reads*
//! are later units; here the spatial insert is skipped, the `InViewport` /
//! `InTimeRange` streams answer empty pages, and the witness tables are written
//! but not yet read.

mod error;
mod harness;
mod maintain;
mod queries;
mod read;

#[cfg(test)]
mod tests;

use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;

use sqlx::postgres::PgPool;
use sqlx::{Acquire, PgConnection, Postgres, Transaction};

use chronoscope_core::geo::{QuadLevel, TileId, Viewport};
use chronoscope_core::grammar::ids::{CommitId, FactId, SubjectKind};
use chronoscope_core::store::schema::{
    ClassPage, ClusterCell, EntityStream, EquivClass, ImageStream, RankKey, normalize_name,
};
use chronoscope_core::store::{
    ClassWalkPage, DepictionWalkPage, EntityView, EventView, FactPlacement, FactStore, FactView,
    FactWrite, ImageView, WalkPage,
};
use chronoscope_core::submit::{FactLookup, StoredCommit, StoredFact, SubmitResult};

pub(crate) use self::error::PostgresFactStoreError;

use crate::common::convert::{i64_to_u64, u64_to_i64};
use crate::common::ids::{SqlEntityId, SqlEventId, SqlIds, SqlImageId};
use crate::common::storage::{
    commit_to_json, external_ref_key, facet_columns, fact_to_json, named_entity, referenced_entity,
    result_to_json, sourced_image, subject_rows, witness_row,
};

use self::error::sql;
use self::read::{FacetKey, ReadBound};

// Aliases to keep the spellings short.
type SqlStoredFact = StoredFact<SqlIds>;
type SqlFactLookup = FactLookup<SqlIds>;
type SqlSubmitResult = SubmitResult<SqlIds>;
type Error = PostgresFactStoreError;

// ============================================================================
// PostgresFactStore
// ============================================================================

/// Postgres implementation of [`FactStore`] over one writable database. The pool
/// is `Arc`-backed (sqlx), so cloning the store is cheap.
#[derive(Debug, Clone)]
pub(crate) struct PostgresFactStore {
    pool: PgPool,
}

impl PostgresFactStore {
    /// Wrap a pool over an already-migrated fact-store database.
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// The underlying pool, for the harness smoke test.
    #[cfg(test)]
    pub(crate) fn pool(&self) -> &PgPool {
        &self.pool
    }
}

// ============================================================================
// Connection forms — the storage type is the handle's mode
// ============================================================================

/// A committed read view's connection: an owned read transaction, rolled back
/// (returning its connection to the pool) on drop.
pub(crate) struct ViewTx(Transaction<'static, Postgres>);

/// The `with_tx` write handle's connection, borrowed from the transaction the
/// frame owns and must recover to commit. `'t` is [`PostgresTx`]'s brand; the
/// marker pins this handle type's own invariance in it as defense in depth,
/// with the cross-store mechanism documented in [`chronoscope_core::store`].
pub(crate) struct FrameConn<'t>(&'t mut PgConnection, PhantomData<fn(&'t ()) -> &'t ()>);

/// A submit scope's connection: an owned savepoint transaction nested in the
/// write transaction.
pub(crate) struct ScopeTx<'n>(Transaction<'n, Postgres>);

/// The connection behind a handle. Transactions are connection-scoped, so a
/// handle is one connection for its lifetime either way; the three forms differ
/// only in ownership.
pub(crate) trait AsConn: conn_sealed::Sealed + Send + Sync {
    fn conn(&mut self) -> &mut PgConnection;
}

/// The connection forms carrying the write surface: [`FactWrite`] exists only
/// over these, so a read view lacks the write methods at compile time.
pub(crate) trait WriteConn: AsConn {}

mod conn_sealed {
    pub trait Sealed {}
    impl Sealed for super::ViewTx {}
    impl Sealed for super::FrameConn<'_> {}
    impl Sealed for super::ScopeTx<'_> {}
}

impl AsConn for ViewTx {
    fn conn(&mut self) -> &mut PgConnection {
        &mut self.0
    }
}

impl AsConn for FrameConn<'_> {
    fn conn(&mut self) -> &mut PgConnection {
        self.0
    }
}

impl AsConn for ScopeTx<'_> {
    fn conn(&mut self) -> &mut PgConnection {
        &mut self.0
    }
}

impl WriteConn for FrameConn<'_> {}
impl WriteConn for ScopeTx<'_> {}

// ============================================================================
// PostgresHandle — the one read/write handle over a connection
// ============================================================================

/// Read (and, for the write forms, write) handle over one connection. Core's
/// view traits are foreign in this crate, so they can't hang off a local source
/// trait (orphan rule); this one concrete type carries each view-trait impl once
/// for every connection form, and keeps its fields private so handles come only
/// from the store.
pub(crate) struct PostgresHandle<C> {
    conn: C,
    /// The read scope — the snapshot bound.
    bound: ReadBound,
}

/// Snapshot-scoped read view: an owned read transaction plus the pinned
/// exclusive upper bound.
pub(crate) type PostgresFactView = PostgresHandle<ViewTx>;

/// Branded transaction handle for [`PostgresFactStore`] — the [`FactWrite`]
/// surface over one open transaction.
pub(crate) type PostgresTx<'brand> = PostgresHandle<FrameConn<'brand>>;

/// Refuse the transaction if any visible `facts` row is unclaimed — committed
/// state never holds one, so a hit is this transaction's own staging that no
/// recorded commit stands behind.
async fn audit_unclaimed_staging(conn: &mut PgConnection) -> Result<(), Error> {
    let row: Option<(i64,)> = sqlx::query_as(queries::UNCLAIMED_STAGED_FACT)
        .fetch_optional(&mut *conn)
        .await
        .map_err(sql("auditing staged rows"))?;
    match row {
        Some((fact_id,)) => Err(Error::UnclaimedStaging { fact_id }),
        None => Ok(()),
    }
}

async fn mint(conn: &mut PgConnection, kind: SubjectKind) -> Result<i64, Error> {
    let (query, context) = match kind {
        SubjectKind::Entity => (queries::MINT_ENTITY, "minting entity id"),
        SubjectKind::Event => (queries::MINT_EVENT, "minting event id"),
        SubjectKind::Image => (queries::MINT_IMAGE, "minting image id"),
    };
    let (id,): (i64,) = sqlx::query_as(query)
        .fetch_one(conn)
        .await
        .map_err(sql(context))?;
    Ok(id)
}

async fn mint_fact_id(conn: &mut PgConnection) -> Result<i64, Error> {
    let (id,): (i64,) = sqlx::query_as(queries::MINT_FACT_ID)
        .fetch_one(conn)
        .await
        .map_err(sql("minting fact id"))?;
    Ok(id)
}

async fn mint_commit_seq(conn: &mut PgConnection) -> Result<i64, Error> {
    let (id,): (i64,) = sqlx::query_as(queries::MINT_COMMIT_SEQ)
        .fetch_one(conn)
        .await
        .map_err(sql("minting commit seq"))?;
    Ok(id)
}

/// The `(entity, event, image)` mint counters, read fresh per known-id check so
/// the row stays the one source of what the store has minted.
async fn counters(conn: &mut PgConnection) -> Result<(i64, i64, i64), Error> {
    sqlx::query_as(queries::MINT_COUNTERS)
        .fetch_one(conn)
        .await
        .map_err(sql("reading mint counters"))
}

// ============================================================================
// FactStore impl
// ============================================================================

impl FactStore for PostgresFactStore {
    type Error = PostgresFactStoreError;
    type Ids = SqlIds;
    type Cursor = FactId;
    type ClassCursor<Rep>
        = (Rep, FactId)
    where
        Rep: Send;
    type Tx<'brand> = PostgresTx<'brand>;
    type View<'a> = PostgresFactView;

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
            .begin()
            .await
            .map_err(sql("opening write transaction"))?;
        // Pin RC before any statement runs: the counters-row FOR UPDATE
        // serializer below degrades into spurious serialization-failure aborts
        // under a stricter isolation, so we don't lean on the session default.
        sqlx::query(queries::SET_ISOLATION)
            .execute(&mut *tx)
            .await
            .map_err(sql("setting transaction isolation"))?;
        // The counters-row lock, taken now and held through COMMIT, is the
        // serialization point: match -> mint -> stage runs under it, so
        // fact-id order equals commit order.
        sqlx::query(queries::LOCK_COUNTERS)
            .execute(&mut *tx)
            .await
            .map_err(sql("locking the counters row"))?;
        let mut handle = PostgresHandle {
            conn: FrameConn(&mut tx, PhantomData),
            bound: ReadBound::Union,
        };
        let result = f(self, &mut handle).await;
        if result.is_ok() {
            if let Err(refusal) = audit_unclaimed_staging(&mut tx).await {
                // The refusal is the diagnosis; a rollback failure on this
                // already-doomed transaction would only mask it.
                let _ = tx.rollback().await;
                return Err(refusal);
            }
            tx.commit().await.map_err(sql("committing transaction"))?;
        } else {
            // The closure's error is the diagnosis; the dropped transaction
            // rolls back regardless.
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
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(sql("opening view read transaction"))?;
        sqlx::query(queries::SET_ISOLATION)
            .execute(&mut *tx)
            .await
            .map_err(sql("setting view transaction isolation"))?;
        Ok(PostgresHandle {
            conn: ViewTx(tx),
            bound: ReadBound::Pinned(snapshot),
        })
    }

    async fn now(&self) -> Result<Self::View<'_>, Self::Error> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(sql("opening view read transaction"))?;
        sqlx::query(queries::SET_ISOLATION)
            .execute(&mut *tx)
            .await
            .map_err(sql("setting view transaction isolation"))?;
        let snapshot = FactId::new(read::next_fact_id(&mut tx).await?);
        Ok(PostgresHandle {
            conn: ViewTx(tx),
            bound: ReadBound::Pinned(snapshot),
        })
    }
}

// ============================================================================
// View-trait impls — once, generic over the connection form
// ============================================================================

impl<C: AsConn> FactView<PostgresFactStore> for PostgresHandle<C> {
    /// A pinned view answers its stored bound; a write handle's union bound
    /// reads the mint counter (one past the highest fact staged on its
    /// connection).
    async fn snapshot(&mut self) -> Result<FactId, Error> {
        match self.bound.snapshot() {
            Some(snapshot) => Ok(snapshot),
            None => Ok(FactId::new(read::next_fact_id(self.conn.conn()).await?)),
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

impl<C: AsConn> EntityView<PostgresFactStore> for PostgresHandle<C> {
    async fn entity_representative(&mut self, member: &SqlEntityId) -> Result<SqlEntityId, Error> {
        read::representative(self.conn.conn(), self.bound, *member).await
    }

    async fn entity_class(
        &mut self,
        member: &SqlEntityId,
    ) -> Result<EquivClass<SqlEntityId>, Error> {
        read::equiv_class(self.conn.conn(), self.bound, *member).await
    }

    /// The spatial (`InViewport`) and temporal (`InTimeRange`) streams answer
    /// empty pages until their later units land; the empty page is the
    /// contract's nothing-found answer and must stay quiet, since the submit
    /// matcher drains keyed walks on every submit with `Local` decls.
    async fn walk_entity_classes<'b>(
        &'b mut self,
        stream: &'b EntityStream<'b>,
        after: Option<(SqlEntityId, FactId)>,
        limit: std::num::NonZeroUsize,
    ) -> Result<ClassWalkPage<PostgresFactStore, SqlEntityId>, Error> {
        let bound = self.bound;
        let conn = self.conn.conn();
        match stream {
            EntityStream::ByName { name, language } => {
                let key = FacetKey::Name {
                    norm: normalize_name(name),
                    language: language.as_str(),
                };
                read::keyed_class_page(conn, bound, key, named_entity, after, limit).await
            }
            EntityStream::ByExternalReference { reference } => {
                let key = FacetKey::ExternalRef(external_ref_key(reference)?);
                read::keyed_class_page(conn, bound, key, referenced_entity, after, limit).await
            }
            EntityStream::All => read::all_class_page(conn, bound, after, limit).await,
            EntityStream::InViewport(_)
            | EntityStream::InTimeRange(_)
            | EntityStream::InViewportAndTimeRange { .. } => Ok(empty_class_page()),
        }
    }

    /// Tiled clustering is a spatial read; it rides the same deferral as the
    /// `InViewport` streams (the pg spatial unit), answering empty until then.
    async fn cluster_entities_in_viewport<'b>(
        &'b mut self,
        _viewport: &'b Viewport,
        _level: QuadLevel,
        _rank: RankKey,
    ) -> Result<Vec<ClusterCell<SqlEntityId>>, Error> {
        Ok(Vec::new())
    }

    async fn cluster_tile_cells(
        &mut self,
        _tile: TileId,
        _rank: RankKey,
    ) -> Result<Vec<ClusterCell<SqlEntityId>>, Error> {
        Ok(Vec::new())
    }

    async fn all_facts_about_entity(
        &mut self,
        entity: &SqlEntityId,
        after: Option<FactId>,
        limit: std::num::NonZeroUsize,
    ) -> Result<WalkPage<PostgresFactStore, SqlEntityId>, Error> {
        read::backlink_page(self.conn.conn(), self.bound, *entity, after, limit).await
    }

    async fn walk_entity_depictions<'b>(
        &'b mut self,
        entity: &'b SqlEntityId,
        after: Option<(SqlImageId, FactId)>,
        limit: std::num::NonZeroUsize,
    ) -> Result<DepictionWalkPage<PostgresFactStore>, Error> {
        read::depiction_page(self.conn.conn(), self.bound, *entity, after, limit).await
    }
}

impl<C: AsConn> EventView<PostgresFactStore> for PostgresHandle<C> {
    async fn event_representative(&mut self, member: &SqlEventId) -> Result<SqlEventId, Error> {
        Ok(*member)
    }

    async fn event_class(&mut self, member: &SqlEventId) -> Result<EquivClass<SqlEventId>, Error> {
        Ok(singleton_class(*member))
    }

    async fn all_facts_about_event(
        &mut self,
        event: &SqlEventId,
        after: Option<FactId>,
        limit: std::num::NonZeroUsize,
    ) -> Result<WalkPage<PostgresFactStore, SqlEventId>, Error> {
        read::backlink_page(self.conn.conn(), self.bound, *event, after, limit).await
    }

    async fn all_has_events_about_event(
        &mut self,
        event: &SqlEventId,
        after: Option<FactId>,
        limit: std::num::NonZeroUsize,
    ) -> Result<WalkPage<PostgresFactStore, SqlEventId>, Error> {
        read::has_event_backlink_page(self.conn.conn(), self.bound, *event, after, limit).await
    }
}

impl<C: AsConn> ImageView<PostgresFactStore> for PostgresHandle<C> {
    async fn image_representatives(
        &mut self,
        members: &[SqlImageId],
    ) -> Result<std::collections::HashMap<SqlImageId, SqlImageId>, Error> {
        read::representatives(self.conn.conn(), self.bound, members).await
    }

    async fn image_class(&mut self, member: &SqlImageId) -> Result<EquivClass<SqlImageId>, Error> {
        read::equiv_class(self.conn.conn(), self.bound, *member).await
    }

    async fn walk_image_classes<'b>(
        &'b mut self,
        stream: &'b ImageStream<'b>,
        after: Option<(SqlImageId, FactId)>,
        limit: std::num::NonZeroUsize,
    ) -> Result<ClassWalkPage<PostgresFactStore, SqlImageId>, Error> {
        let bound = self.bound;
        let conn = self.conn.conn();
        match stream {
            ImageStream::BySourceUrl { url } => {
                let key = FacetKey::SourceUrl(url.as_str());
                read::keyed_class_page(conn, bound, key, sourced_image, after, limit).await
            }
            ImageStream::All => read::all_class_page(conn, bound, after, limit).await,
            ImageStream::InViewport(_)
            | ImageStream::InTimeRange(_)
            | ImageStream::InViewportAndTimeRange { .. } => Ok(empty_class_page()),
        }
    }

    async fn all_facts_about_image(
        &mut self,
        image: &SqlImageId,
        after: Option<FactId>,
        limit: std::num::NonZeroUsize,
    ) -> Result<WalkPage<PostgresFactStore, SqlImageId>, Error> {
        read::backlink_page(self.conn.conn(), self.bound, *image, after, limit).await
    }
}

// ============================================================================
// FactWrite impl — the write connection forms only
// ============================================================================

impl<C: WriteConn> FactWrite<PostgresFactStore> for PostgresHandle<C> {
    type Nested<'n>
        = PostgresHandle<ScopeTx<'n>>
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
        // sqlx tracks transaction depth on the connection, so this begin opens a
        // savepoint nested in the write transaction.
        let scope_tx = self
            .conn
            .conn()
            .begin()
            .await
            .map_err(sql("opening submit scope"))?;
        let mut scope = PostgresHandle {
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

    async fn mint_entity(&mut self) -> Result<SqlEntityId, Error> {
        Ok(SqlEntityId(
            mint(self.conn.conn(), SubjectKind::Entity).await?,
        ))
    }

    async fn mint_event(&mut self) -> Result<SqlEventId, Error> {
        Ok(SqlEventId(
            mint(self.conn.conn(), SubjectKind::Event).await?,
        ))
    }

    async fn mint_image(&mut self) -> Result<SqlImageId, Error> {
        Ok(SqlImageId(
            mint(self.conn.conn(), SubjectKind::Image).await?,
        ))
    }

    // Ids mint dense from zero, so the known checks bound both sides — a
    // wire-supplied negative id is as unknown as one past the counter.

    async fn entity_known(&mut self, id: &SqlEntityId) -> Result<bool, Error> {
        let (entities, _, _) = counters(self.conn.conn()).await?;
        Ok((0..entities).contains(&id.0))
    }

    async fn event_known(&mut self, id: &SqlEventId) -> Result<bool, Error> {
        let (_, events, _) = counters(self.conn.conn()).await?;
        Ok((0..events).contains(&id.0))
    }

    async fn image_known(&mut self, id: &SqlImageId) -> Result<bool, Error> {
        let (_, _, images) = counters(self.conn.conn()).await?;
        Ok((0..images).contains(&id.0))
    }

    async fn stage_fact(&mut self, fact: SqlStoredFact) -> Result<FactId, Error> {
        let conn = self.conn.conn();
        let facets = facet_columns(&fact)?;
        let subjects = subject_rows(&fact);
        let witness = witness_row(&fact);
        let fact_json = fact_to_json(fact)?;
        // A RetractCommit facet stores the target's surrogate seq; an unrecorded
        // target resolves NULL, and the validator rejects the bundle before
        // anything reads it. fact_json keeps the raw hash.
        let retracts_commit_seq = match &facets.retracts_commit_id {
            Some(hash) => read::commit_seq(&mut *conn, hash).await?,
            None => None,
        };
        // Mint the dense fact id, then insert the row bound to it — no MAX(col)+1
        // splice; the counter is the id source under the held row lock.
        let fid_raw = mint_fact_id(&mut *conn).await?;
        sqlx::query(queries::INSERT_FACT)
            .bind(fid_raw)
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
            .bind(facets.event_owner)
            .bind(facets.retracts_fact_id)
            .bind(retracts_commit_seq)
            .execute(&mut *conn)
            .await
            .map_err(sql("staging fact row"))?;
        for (kind, subject) in subjects {
            sqlx::query(queries::INSERT_SUBJECT)
                .bind(fid_raw)
                .bind(kind)
                .bind(subject)
                .execute(&mut *conn)
                .await
                .map_err(sql("inserting fact subject row"))?;
        }
        // The spatial insert (facts_spatial) is a later unit; witness,
        // representative-log, and retraction maintenance run on the same
        // connection as the staging, so a rejected submit's savepoint unwinds
        // their rows with its fact rows. record_retraction runs last, after the
        // fact + facets + subjects are visible, so its own component recompute
        // (at the union bound) sees the staged retraction fact.
        if let Some(witness) = witness {
            maintain::record_witness(&mut *conn, fid_raw, witness).await?;
        }
        if let (Some(kind), Some(a), Some(b)) = (facets.edge_kind, facets.edge_a, facets.edge_b) {
            maintain::record_identity_edge(&mut *conn, kind, a, b, fid_raw).await?;
        }
        if facets.retracts_fact_id.is_some() || retracts_commit_seq.is_some() {
            maintain::record_retraction(&mut *conn, fid_raw).await?;
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
        let commit_seq = mint_commit_seq(&mut *conn).await?;
        sqlx::query(queries::INSERT_COMMIT)
            .bind(commit_seq)
            .bind(commit.commit_id.as_str())
            .bind(&commit_json)
            .bind(&result_json)
            .execute(&mut *conn)
            .await
            .map_err(sql("recording commit metadata"))?;
        // The `commit_seq IS NULL` guard makes a claim of an unknown or
        // already-owned row update nothing — refused here, named.
        for fid in &commit.fact_ids {
            let fid_bind = u64_to_i64(fid.get(), "recorded fact id")?;
            let claimed = sqlx::query(queries::CLAIM_FACT)
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

fn singleton_class<S: Ord + Copy>(member: S) -> EquivClass<S> {
    EquivClass {
        representative: member,
        members: std::iter::once(member).collect(),
    }
}
