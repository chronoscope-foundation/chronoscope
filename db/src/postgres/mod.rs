//! Postgres [`FactStore`] backend. Retraction rides recursive CTEs rewritten to
//! a single self-reference each (see the `queries` module), tiled clustering is
//! a Morton-range scan on the `quadkey` facet, and the `InViewport` reads are a
//! `PostGIS` `GiST` bounding-box pre-filter refined in Rust.
//!
//! One writable database, no base/overlay union: every read names its tables
//! directly and every query is a plain constant (see the `queries` module). The
//! json columns are `JSONB`; the shared `common::storage` string codecs bind
//! through a `$N::jsonb` cast and read back through `col::text`.
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
//! it with the subject backlinks. Tiled clustering scans the `quadkey` facet by
//! Morton range and folds each range through core's shared cell fold. The
//! `InViewport` walks query `facts_spatial`'s `GiST` index once per viewport half
//! and hand the candidates to the shared membership decision
//! (`common::spatial`).
//!
//! ## Not implemented
//!
//! The temporal-conflict witness *reads* have no Postgres implementation yet, so
//! the witness tables are written but never read. The `InTimeRange` /
//! `InViewportAndTimeRange` streams answer empty pages, a gap shared with the
//! other backends.

mod error;
mod maintain;
mod queries;
mod read;

// Stands up a throwaway cluster by shelling out to initdb/pg_ctl, so it belongs
// to the test build alone.
#[cfg(test)]
mod harness;

#[cfg(test)]
mod tests;

use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::time::Duration;

use sqlx::postgres::{PgPool, PgPoolOptions};
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

pub use self::error::PostgresFactStoreError;

use crate::common::convert::{i64_to_u64, u64_to_i64};
use crate::common::ids::{SqlEntityId, SqlEventId, SqlIds, SqlImageId};
use crate::common::storage::{
    commit_to_json, external_ref_key, facet_columns, fact_to_json, kind_tag, named_entity,
    referenced_entity, result_to_json, sourced_image, subject_rows, witness_row,
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
pub struct PostgresFactStore {
    pool: PgPool,
}

/// Connections the fact-store pool holds. A read view owns one for its whole
/// lifetime (a single read transaction), so what this caps is concurrent read
/// views, not concurrent queries: an in-flight HTTP read holds its connection
/// until the handler returns. The number is a headroom judgment, not a measured
/// one. Postgres ships `max_connections = 100`, so 32 leaves room for a second
/// instance, a concurrent loader's migration, and admin sessions. Lifting the
/// ceiling for real means making reads stateless so a view stops pinning a
/// connection.
const POOL_MAX_CONNECTIONS: u32 = 32;

/// How long a caller waits for a pooled connection. Under the cap above,
/// exhaustion means concurrent views outnumber connections, a queue that will
/// not drain inside one request, so a short wait turns it into an error an
/// operator can read while sqlx's 30s default outlives most client timeouts.
const POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

/// The fact schema's migrations, embedded at compile time. One source for both
/// constructors below: the loader applies them, the server checks them.
static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations-postgres/facts");

impl PostgresFactStore {
    /// Wrap a pool over an already-migrated fact-store database. Crate-internal
    /// because the migration precondition is unenforced; [`Self::connect`] and
    /// [`Self::connect_and_migrate`] establish it.
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Connect to a fact-store database that already carries the schema, and
    /// hand back the store over the new pool.
    ///
    /// The serving path. Creating the schema is a deliberate act
    /// ([`Self::connect_and_migrate`]) because a fact-store location is one
    /// mistyped environment variable away from the app database, which is also a
    /// Postgres URL: connecting must never leave the fact tables and the
    /// `PostGIS` extension behind in whatever database it named.
    ///
    /// # Errors
    /// Returns [`DbError::Config`](crate::DbError::Config) if the database
    /// carries no fact schema or one older than this binary's migrations, and
    /// [`DbError`](crate::DbError) if the connection fails.
    pub async fn connect(url: &str) -> crate::DbResult<Self> {
        let pool = connect_pool(url).await?;
        if let Err(absent) = require_migrated(&pool).await {
            // Drop the pool inside the caller's runtime, so its connections end
            // their server sessions before the refusal propagates.
            pool.close().await;
            return Err(absent);
        }
        Ok(Self::new(pool))
    }

    /// Connect to the fact-store database at `url`, apply the fact schema's
    /// migrations, and hand back the store over the new pool.
    ///
    /// The loading path, and the only place the fact schema comes into
    /// existence. sqlx wraps the run in a Postgres advisory lock, so concurrent
    /// loaders serialize on it and exactly one applies each migration. The first
    /// application creates the `PostGIS` extension, which needs a role
    /// privileged to do so.
    ///
    /// # Errors
    /// Returns [`DbError`](crate::DbError) if the connection or a migration
    /// fails.
    pub async fn connect_and_migrate(url: &str) -> crate::DbResult<Self> {
        let pool = connect_pool(url).await?;
        MIGRATOR.run(&pool).await?;
        Ok(Self::new(pool))
    }

    /// The underlying pool, for the harness smoke test.
    #[cfg(test)]
    pub(crate) fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Close the pool, awaiting each connection's teardown so every session
    /// terminates cleanly on the server before the process exits.
    pub async fn close(&self) {
        self.pool.close().await;
    }
}

/// The fact-store pool, shared by both constructors so a store's connection
/// budget doesn't depend on which one built it.
async fn connect_pool(url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(POOL_MAX_CONNECTIONS)
        .acquire_timeout(POOL_ACQUIRE_TIMEOUT)
        .connect(url)
        .await
}

/// Refuse a database that this binary's migrations have not all been applied to.
///
/// The two refusals read differently on purpose: an absent ledger means the
/// location is not a fact store at all (a typo, or the app database), while a
/// partial one means the schema is behind the code about to query it.
async fn require_migrated(pool: &PgPool) -> crate::DbResult<()> {
    // to_regclass answers NULL for an absent relation, where a regclass cast
    // would raise, so the "never migrated" case stays a value rather than an
    // error to pattern-match. Unqualified, so it resolves down the same
    // search_path the migrator writes through.
    let (ledger,): (bool,) = sqlx::query_as("SELECT to_regclass('_sqlx_migrations') IS NOT NULL")
        .fetch_one(pool)
        .await?;
    if !ledger {
        return Err(crate::DbError::Config(
            "the fact-store database carries no schema. The server connects to a \
             fact store that has already been migrated; create the schema with \
             `ingest build-db`, and check the configured location names the \
             fact-store database rather than the app one"
                .to_owned(),
        ));
    }
    let applied: Vec<(i64,)> = sqlx::query_as("SELECT version FROM _sqlx_migrations WHERE success")
        .fetch_all(pool)
        .await?;
    let applied: std::collections::BTreeSet<i64> = applied.into_iter().map(|(v,)| v).collect();
    let missing: Vec<String> = MIGRATOR
        .iter()
        .map(|migration| migration.version)
        .filter(|version| !applied.contains(version))
        .map(|version| version.to_string())
        .collect();
    if !missing.is_empty() {
        return Err(crate::DbError::Config(format!(
            "the fact-store database's schema is older than this binary: migrations {} \
             are unapplied. Run `ingest build-db` against it to bring the schema forward",
            missing.join(", "),
        )));
    }
    Ok(())
}

// ============================================================================
// Connection forms — the storage type is the handle's mode
// ============================================================================

/// A committed read view's connection: an owned read transaction, rolled back
/// (returning its connection to the pool) on drop.
pub struct ViewTx(Transaction<'static, Postgres>);

/// The `with_tx` write handle's connection, borrowed from the transaction the
/// frame owns and must recover to commit. `'t` is [`PostgresTx`]'s brand; the
/// marker pins this handle type's own invariance in it as defense in depth,
/// with the cross-store mechanism documented in [`chronoscope_core::store`].
pub struct FrameConn<'t>(&'t mut PgConnection, PhantomData<fn(&'t ()) -> &'t ()>);

/// A submit scope's connection: an owned savepoint transaction nested in the
/// write transaction.
pub struct ScopeTx<'n>(Transaction<'n, Postgres>);

/// The connection behind a handle. Transactions are connection-scoped, so a
/// handle is one connection for its lifetime either way; the three forms differ
/// only in ownership.
pub trait AsConn: conn_sealed::Sealed + Send + Sync {
    fn conn(&mut self) -> &mut PgConnection;
}

/// The connection forms carrying the write surface: [`FactWrite`] exists only
/// over these, so a read view lacks the write methods at compile time.
pub trait WriteConn: AsConn {}

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
pub struct PostgresHandle<C> {
    conn: C,
    /// The read scope — the snapshot bound.
    bound: ReadBound,
}

/// Snapshot-scoped read view: an owned read transaction plus the pinned
/// exclusive upper bound.
pub type PostgresFactView = PostgresHandle<ViewTx>;

/// Branded transaction handle for [`PostgresFactStore`] — the [`FactWrite`]
/// surface over one open transaction.
pub type PostgresTx<'brand> = PostgresHandle<FrameConn<'brand>>;

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

    /// The temporal (`InTimeRange`) streams answer empty pages, a gap shared
    /// with the other backends; the empty page is the contract's nothing-found
    /// answer and must stay quiet, since the submit matcher drains keyed walks
    /// on every submit with `Local` decls.
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
            EntityStream::InViewport(viewport) => {
                read::spatial_entity_page(conn, bound, viewport, after, limit).await
            }
            EntityStream::InTimeRange(_) | EntityStream::InViewportAndTimeRange { .. } => {
                Ok(empty_class_page())
            }
        }
    }

    async fn cluster_entities_in_viewport<'b>(
        &'b mut self,
        viewport: &'b Viewport,
        level: QuadLevel,
        rank: RankKey,
    ) -> Result<Vec<ClusterCell<SqlEntityId>>, Error> {
        read::cluster_entities_in_viewport(self.conn.conn(), self.bound, viewport, level, rank)
            .await
    }

    async fn cluster_tile_cells(
        &mut self,
        tile: TileId,
        rank: RankKey,
    ) -> Result<Vec<ClusterCell<SqlEntityId>>, Error> {
        read::cluster_tile_cells(self.conn.conn(), self.bound, tile, rank).await
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
            ImageStream::InViewport(viewport) => {
                read::spatial_image_page(conn, bound, viewport, after, limit).await
            }
            ImageStream::InTimeRange(_) | ImageStream::InViewportAndTimeRange { .. } => {
                Ok(empty_class_page())
            }
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
        // A region location writes a facts_spatial envelope, though it carries
        // no clustering key. Read before fact_to_json, which consumes the fact.
        let spatial = fact
            .located_subject()
            .map(|(location, subject)| (location.bounding_rects(), kind_tag(subject.kind())));
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
            .bind(facets.cluster.map(|cluster| cluster.quadkey))
            .bind(facets.cluster.map(|cluster| cluster.subject_kind))
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
        // A `Reference` / `Empty` / `Unbounded` location covers no rect, so a
        // located fact with nothing to index writes no spatial row.
        if let Some((rects, subject_kind)) = &spatial
            && !rects.is_empty()
        {
            let mut lonmin: Vec<f64> = Vec::with_capacity(rects.len());
            let mut latmin: Vec<f64> = Vec::with_capacity(rects.len());
            let mut lonmax: Vec<f64> = Vec::with_capacity(rects.len());
            let mut latmax: Vec<f64> = Vec::with_capacity(rects.len());
            for rect in rects {
                lonmin.push(rect.min_lon);
                latmin.push(rect.min_lat);
                lonmax.push(rect.max_lon);
                latmax.push(rect.max_lat);
            }
            sqlx::query(queries::INSERT_SPATIAL)
                .bind(fid_raw)
                .bind(*subject_kind)
                .bind(lonmin)
                .bind(latmin)
                .bind(lonmax)
                .bind(latmax)
                .execute(&mut *conn)
                .await
                .map_err(sql("inserting spatial envelope rows"))?;
        }
        // Witness, representative-log, and retraction maintenance run on the
        // same connection as the staging, so a rejected submit's savepoint
        // unwinds their rows with its fact rows. record_retraction runs last,
        // after the fact + facets + subjects are visible, so its own component
        // recompute (at the union bound) sees the staged retraction fact.
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
