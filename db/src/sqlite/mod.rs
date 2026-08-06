//! SQLite [`FactStore`] backend.
//!
//! Two-layer layout: the fact tables live in a writable `ovl` overlay and,
//! optionally, a frozen read-only `base` beneath it, both attached on a pool
//! whose `main` holds no fact tables (SpatiaLite loaded, WAL). Writes and
//! overlay-only state qualify `ovl.`; data reads stay unqualified and, when a
//! base is mounted, resolve through per-table temp union views spanning
//! base ∪ overlay (see the `queries` module and `UNION_VIEW_TABLES`). Mounting
//! is sound because the fact schema is append-only — merges and retractions of
//! base subjects append overlay rows, base rows are never touched — so a frozen
//! base under an overlay reads correctly, and the overlay mints ids past the
//! base's max so the id spaces stay disjoint across the union. The store owns
//! that pool — [`SqliteFactStore::open`] ensures the overlay's schema, mounts
//! any base read-only, seeds the overlay's id continuations past the base, and
//! verifies the query plans; [`new`](SqliteFactStore::new) wraps an
//! overlay-only pool a caller already attached.
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
//! Retraction resolves through the backend-shared fixpoint in
//! [`chronoscope_core::store::retraction`] over edges fetched by recursive
//! CTE — see the `read` module for the shapes.
//!
//! Representatives and classes read the append-only `subject_reps` log: one
//! descending seek resolves any member at any snapshot (no row = self), and
//! the reverse gather answers a whole class. The log is maintained at write
//! time (the `maintain` module) — staging an identity edge logs the merge it
//! performs, staging a retraction that touches identity edges recomputes
//! the affected components — so historical views cost the same one seek as
//! now. The class-stream walks (`walk_entity_classes`,
//! `walk_image_classes`) combine the facet indexes with that resolution,
//! and `walk_entity_depictions` combines it with the subject backlinks;
//! the spatial streams (`InViewport` for entities and images) fetch candidates
//! from the `facts_spatial` SpatiaLite geometry index — one covering-rect
//! envelope per location, written at stage time — filter single circles by
//! ellipsoidal `ST_Distance` in SQL, and refine the rest through core's shared
//! region predicate; the temporal streams still answer empty pages until their
//! index exists, with their conformance cases ignored.

mod error;
mod maintain;
mod queries;
mod read;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod witness_tests;

use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

use sqlx::sqlite::SqlitePool;
use sqlx::{Acquire, Sqlite, SqliteConnection, Transaction};

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

pub use self::error::SqliteFactStoreError;

use crate::common::convert::{i64_to_u64, u64_to_i64};
use crate::common::ids::{SqlEntityId, SqlEventId, SqlIds, SqlImageId};
use crate::common::storage::{
    commit_to_json, external_ref_key, facet_columns, fact_to_json, kind_tag, named_entity,
    referenced_entity, result_to_json, sourced_image, subject_rows, witness_row,
};

use self::error::sql;
use self::queries::FactQueries;
use self::read::{FacetKey, ReadBound};

// Aliases to keep the spellings short.
type SqlStoredFact = StoredFact<SqlIds>;
type SqlFactLookup = FactLookup<SqlIds>;
type SqlSubmitResult = SubmitResult<SqlIds>;
type Error = SqliteFactStoreError;

// ============================================================================
// SqliteFactStore
// ============================================================================

/// The fact tables a mounted base ∪ overlay spans through a per-connection temp
/// union view, so the unqualified data reads see both layers with no per-query
/// branching. Excludes the overlay-only counters (seeded, never unioned) and
/// the spatial shadow tables (the spatial read unions per rtree branch instead,
/// since an rtree can't drive its index through a view). Built in
/// [`create_facts_pool`](crate::create_facts_pool)'s `after_connect`.
pub(crate) const UNION_VIEW_TABLES: &[&str] = &[
    "facts",
    "fact_subjects",
    "fact_commits",
    "subject_reps",
    "existence_witness",
    "event_witness",
    "has_event",
    "construction_start",
    "demolition_completed",
];

/// The stored-facts codec version. Bump on ANY change that would make a
/// previously built facts-DB artifact decode wrongly or read incompletely:
/// the stored `fact_json` / `result_json` shapes, the id encoding conventions,
/// or the facts-schema columns/indexes a read relies on. `ingest build-db`
/// stamps it into the facts file's `PRAGMA user_version` after a successful
/// build, and both [`validate_facts_file`] and the dev mount refuse a file
/// that doesn't carry it — a stale codec, or an interrupted build (which never
/// reached the stamp), fails loudly instead of decoding garbage.
pub const FACTS_CODEC_VERSION: i32 = 2;

/// Why a facts database file is not a valid, current build. Both consumers of
/// a pre-built base pin — [`SqliteFactStore::open`] mounting one, and the dev
/// server's [`validate_facts_file`] pre-check — validate through it and render
/// their own remedy around this: `open`'s mount refusal names the build
/// command, the dev mount the fetch command.
#[derive(Debug, thiserror::Error)]
pub enum FactsFileError {
    /// The file is missing or its bytes can't be read.
    #[error("facts database {path} is unreadable: {source}")]
    Unreadable {
        /// The offending path.
        path: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The file exists but isn't a SQLite database (too short, or the magic
    /// header is wrong).
    #[error("facts database {path} is not a SQLite database ({detail})")]
    NotSqlite {
        /// The offending path.
        path: String,
        /// What was wrong.
        detail: String,
    },
    /// The database carries the wrong codec stamp — a different version, or
    /// `0` from an unstamped / interrupted build.
    #[error(
        "facts database {path} carries facts codec version {found}, but this build expects \
         {expected} (an unstamped or partial build reads 0)"
    )]
    CodecMismatch {
        /// The offending path.
        path: String,
        /// The version read from the file.
        found: i32,
        /// The version this build requires.
        expected: i32,
    },
}

/// Validate a pre-built facts database by header inspection alone — no SQLite
/// connection, so it works on a read-only artifact with no `-shm`/`-wal`
/// access. Checks the file exists, is non-empty with the SQLite magic header,
/// and carries [`FACTS_CODEC_VERSION`] in its `user_version` (file-header
/// bytes 60..64, big-endian; the stamp is `ingest build-db`'s last step, so a
/// partial build reads `0`). The one definition of "is this a valid, current
/// facts DB", shared by [`SqliteFactStore::open`] validating a base pin and the
/// dev server's pre-mount check.
///
/// # Errors
/// Returns [`FactsFileError`] naming the path and the specific failure.
pub fn validate_facts_file(path: &std::path::Path) -> Result<(), FactsFileError> {
    use std::io::Read;

    let ps = || path.display().to_string();
    let unreadable = |source| FactsFileError::Unreadable { path: ps(), source };

    let len = std::fs::metadata(path).map_err(unreadable)?.len();
    if len < 64 {
        return Err(FactsFileError::NotSqlite {
            path: ps(),
            detail: format!("{len} bytes, shorter than the 64-byte header"),
        });
    }
    let mut header = [0u8; 64];
    std::fs::File::open(path)
        .map_err(unreadable)?
        .read_exact(&mut header)
        .map_err(unreadable)?;
    if &header[0..16] != b"SQLite format 3\0" {
        return Err(FactsFileError::NotSqlite {
            path: ps(),
            detail: "magic header mismatch".to_owned(),
        });
    }
    let found = i32::from_be_bytes([header[60], header[61], header[62], header[63]]);
    if found != FACTS_CODEC_VERSION {
        return Err(FactsFileError::CodecMismatch {
            path: ps(),
            found,
            expected: FACTS_CODEC_VERSION,
        });
    }
    Ok(())
}

/// Where a fact store's databases live. `app` is the connection's `main` (a
/// throwaway `sqlite::memory:` for the standalone store — the fact tables never
/// live there); `overlay` is the writable facts database, attached as `ovl`,
/// given as a filesystem path (a leading `sqlite:` is stripped). `base` is an
/// optional frozen read-only database a union view reads beneath the overlay —
/// `None` for an overlay-only store (the producer, most tests), `Some(pin)`
/// when serving a pre-built artifact under a fresh writable overlay.
#[derive(Debug, Clone)]
pub struct FactStoreLocations {
    /// The `sqlite:` URL for the connection's `main` database.
    app: String,
    /// The writable facts database — a filesystem path attached as `ovl`.
    overlay: String,
    /// A frozen read-only base beneath the overlay in a union view, or `None`
    /// for overlay-only.
    base: Option<String>,
}

/// A tempdir path as the `String` the `ATTACH` binds — the one place a `&Path`
/// database location becomes the stored string, so every fixture routes its
/// path conversion through the same UTF-8 guard.
fn path_string(path: &std::path::Path, role: &str) -> crate::DbResult<String> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        crate::DbError::Config(format!(
            "facts {role} path is not valid UTF-8: {}",
            path.display()
        ))
    })
}

impl FactStoreLocations {
    /// A standalone overlay-only store over one writable facts file, with a
    /// throwaway in-memory `main` — the shape the producer (`ingest build-db`),
    /// conformance, and the api-test fixtures use.
    pub fn standalone(overlay: impl Into<String>) -> Self {
        Self {
            app: "sqlite::memory:".to_owned(),
            overlay: overlay.into(),
            base: None,
        }
    }

    /// [`standalone`](Self::standalone) from a filesystem path.
    ///
    /// # Errors
    /// Returns [`DbError`](crate::DbError) if the path is not valid UTF-8.
    pub fn standalone_at(overlay: &std::path::Path) -> crate::DbResult<Self> {
        Ok(Self::standalone(path_string(overlay, "overlay")?))
    }

    /// A mounted store: a frozen `base` artifact read beneath a fresh writable
    /// `overlay`, with a throwaway in-memory `main`. The serving shape —
    /// submissions land in the (typically discarded) overlay, reads span
    /// base ∪ overlay.
    pub fn mounted(base: impl Into<String>, overlay: impl Into<String>) -> Self {
        Self {
            app: "sqlite::memory:".to_owned(),
            overlay: overlay.into(),
            base: Some(base.into()),
        }
    }

    /// [`mounted`](Self::mounted) from filesystem paths.
    ///
    /// # Errors
    /// Returns [`DbError`](crate::DbError) if either path is not valid UTF-8.
    pub fn mounted_at(base: &std::path::Path, overlay: &std::path::Path) -> crate::DbResult<Self> {
        Ok(Self::mounted(
            path_string(base, "base")?,
            path_string(overlay, "overlay")?,
        ))
    }

    /// The overlay as a bare filesystem path (for the `ATTACH`), stripping a
    /// `sqlite:` scheme prefix if present.
    fn overlay_path(&self) -> &str {
        self.overlay
            .strip_prefix("sqlite:")
            .unwrap_or(&self.overlay)
    }

    /// The base as a bare filesystem path, stripping a `sqlite:` prefix, or
    /// `None` for an overlay-only store.
    fn base_path(&self) -> Option<&str> {
        self.base
            .as_deref()
            .map(|base| base.strip_prefix("sqlite:").unwrap_or(base))
    }
}

/// Create (if absent) and migrate a fresh facts database at `overlay_path`,
/// opening the path directly as `main` — sqlx migrations create tables in
/// `main`, so this is the only way to build the fact schema in the file; the
/// serving pool then attaches the file as `ovl`. Does NOT stamp the codec
/// version: the stamp certifies a *completed* build, so only `ingest build-db`
/// writes it, after the ingest succeeds. Idempotent: an already-migrated file
/// skips applied migrations.
///
/// # Errors
/// Returns [`DbError`](crate::DbError) if the pool or migrations fail.
pub async fn create_facts_file(overlay_path: &str) -> crate::DbResult<()> {
    let pool = crate::create_facts_pool(&format!("sqlite:{overlay_path}"), None).await?;
    sqlx::migrate!("./migrations/facts").run(&pool).await?;
    pool.close().await;
    Ok(())
}

/// Seed the overlay's per-kind subject-id counters past the base's, so a fresh
/// overlay over a populated base never re-mints a base subject id. The fact-id
/// and commit-seq continuations take a per-branch `MAX` across base and overlay
/// at insert time, but the typed counters are overlay-only state that can't span
/// layers — so their high-water marks are copied forward once at mount, taking
/// the max of each so a reused overlay that already minted past the base keeps
/// its own frontier. Runs only on a base-mounted pool, where `base.*` resolves.
async fn seed_overlay_continuations(pool: &SqlitePool) -> crate::DbResult<()> {
    let (entity, event, image): (i64, i64, i64) = sqlx::query_as(queries::BASE_COUNTERS.sql)
        .fetch_one(pool)
        .await?;
    sqlx::query(queries::SEED_COUNTERS.sql)
        .bind(entity)
        .bind(event)
        .bind(image)
        .execute(pool)
        .await?;
    Ok(())
}

/// SQLite implementation of [`FactStore`] over a base ∪ overlay pool: `main`
/// holds no fact tables, the writable `ovl` overlay holds the store's own, and
/// (when mounted) a frozen `base` supplies the layer beneath.
#[derive(Debug, Clone)]
pub struct SqliteFactStore {
    pool: SqlitePool,
    /// The base-aware SQL resolved once for this store's fixed layer shape (see
    /// [`FactQueries`]), so the hot paths bind a cached `&str` rather than
    /// re-`format!`-ing per fact / member / page. Shared behind an `Arc` so
    /// cloning the store — the api `AppState`, the tests — stays cheap.
    queries: Arc<FactQueries>,
}

impl SqliteFactStore {
    /// Wrap an already-built overlay-only pool (fact tables visible as `ovl.*`,
    /// no base). The pool's owner set up the attach and the fact migrations.
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            queries: Arc::new(FactQueries::resolve(false)),
        }
    }

    /// Mount the fact store at `locations`. The one construction path, uniform
    /// across the producer and serving:
    ///
    /// - ensure the overlay carries the fact schema ([`create_facts_file`], a
    ///   no-op on an already-migrated file);
    /// - validate any base pin ([`validate_facts_file`]: exists, SQLite, current
    ///   codec stamp), so serving a missing, partial, or stale artifact fails
    ///   loud rather than fabricating an empty layer;
    /// - build the pool — the overlay attaches read-write, the base (if any)
    ///   read-only+immutable, and the union views span both;
    /// - seed the overlay's id counters past the base's, so a fresh overlay over
    ///   a populated base never re-mints a base subject id;
    /// - verify the fact-query plans against the layers this pool actually
    ///   mounts.
    ///
    /// A base-less mount is the producer (`ingest build-db`, a fixture base,
    /// which stamps afterward) and the fresh test/conformance fixtures. A
    /// mount with a base serves that finished artifact under a scratch overlay.
    ///
    /// # Errors
    /// Returns [`DbError`](crate::DbError) if the overlay build, base
    /// validation, pool, seeding, or plan verification fails.
    pub async fn open(locations: FactStoreLocations) -> crate::DbResult<Self> {
        create_facts_file(locations.overlay_path()).await?;
        if let Some(base) = locations.base_path() {
            validate_facts_file(std::path::Path::new(base)).map_err(|e| {
                crate::DbError::Config(format!(
                    "{e}; build one with `ingest build-db` and point CHRONOSCOPE_FACTS_DB at it"
                ))
            })?;
        }
        let has_base = locations.base_path().is_some();
        let pool = crate::create_facts_pool(
            &locations.app,
            Some(crate::FactMount {
                overlay: locations.overlay_path(),
                base: locations.base_path(),
            }),
        )
        .await?;
        if has_base {
            seed_overlay_continuations(&pool).await?;
        }
        let fact_queries = FactQueries::resolve(has_base);
        // Verify the exact strings this store will bind, so the plan gate can't
        // drift from the resolved set.
        queries::verify_query_plans(&pool, &fact_queries).await?;
        Ok(Self {
            pool,
            queries: Arc::new(fact_queries),
        })
    }

    /// Stamp [`FACTS_CODEC_VERSION`] into the overlay's `user_version` — the
    /// certificate that this facts database is a completed build. `ingest
    /// build-db` calls it after the ingest succeeds, so an interrupted build
    /// leaves `0` and is rejected by [`validate_facts_file`] and the dev
    /// mount. The value is a compile-time constant, not input, and a `PRAGMA`
    /// takes no binds.
    ///
    /// The stamp write lands in the overlay's WAL; a `TRUNCATE` checkpoint
    /// then folds it into the database-file header, so [`validate_facts_file`]
    /// — which reads the raw header, not a connection — sees the stamp
    /// regardless of when the pool later checkpoints on close. Post-ingest
    /// there are no held readers, so the checkpoint completes.
    ///
    /// # Errors
    /// Returns [`DbError`](crate::DbError) if the write or checkpoint fails.
    pub async fn stamp_codec_version(&self) -> crate::DbResult<()> {
        sqlx::query(&format!("PRAGMA ovl.user_version = {FACTS_CODEC_VERSION}"))
            .execute(&self.pool)
            .await?;
        sqlx::query("PRAGMA ovl.wal_checkpoint(TRUNCATE)")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Close the pool, awaiting connection teardown inside the runtime so
    /// SpatiaLite's `dlclose` finishes before process exit.
    pub async fn close(&self) {
        self.pool.close().await;
    }
}

// ============================================================================
// Connection forms — the storage type is the handle's mode
// ============================================================================

/// A committed read view's connection: an owned deferred read transaction,
/// rolled back (returning its connection to the pool) on drop.
pub struct ViewTx(Transaction<'static, Sqlite>);

/// The `with_tx` write handle's connection, borrowed from the immediate
/// transaction the frame owns and must recover to commit. `'t` is
/// [`SqliteTx`]'s brand; the marker pins this handle type's own invariance in
/// it as defense in depth, with the cross-store mechanism documented in
/// [`chronoscope_core::store`].
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
    /// The read scope — the snapshot bound.
    bound: ReadBound,
    /// The store's cached base-aware SQL, threaded to the reads so they bind a
    /// resolved `&str` instead of re-building it per call.
    queries: Arc<FactQueries>,
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
    type Ids = SqlIds;
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
            queries: self.queries.clone(),
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
        Ok(FactId::new(
            read::next_fact_id(&mut conn, &self.queries).await?,
        ))
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
            queries: self.queries.clone(),
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
        let snapshot = FactId::new(read::next_fact_id(&mut tx, &self.queries).await?);
        Ok(SqliteHandle {
            conn: ViewTx(tx),
            bound: ReadBound::Pinned(snapshot),
            queries: self.queries.clone(),
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
        match self.bound.snapshot() {
            Some(snapshot) => Ok(snapshot),
            None => {
                let fq = &*self.queries;
                Ok(FactId::new(read::next_fact_id(self.conn.conn(), fq).await?))
            }
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
    async fn entity_representative(&mut self, member: &SqlEntityId) -> Result<SqlEntityId, Error> {
        let fq = &*self.queries;
        read::representative(self.conn.conn(), self.bound, fq, *member).await
    }

    async fn entity_class(
        &mut self,
        member: &SqlEntityId,
    ) -> Result<EquivClass<SqlEntityId>, Error> {
        let fq = &*self.queries;
        read::equiv_class(self.conn.conn(), self.bound, fq, *member).await
    }

    /// The temporal streams wait on their date index; their empty page is
    /// the contract's nothing-found answer, and it must stay quiet rather
    /// than error because the submit matcher drains walks on every submit
    /// with `Local` decls.
    async fn walk_entity_classes<'b>(
        &'b mut self,
        stream: &'b EntityStream<'b>,
        after: Option<(SqlEntityId, FactId)>,
        limit: std::num::NonZeroUsize,
    ) -> Result<ClassWalkPage<SqliteFactStore, SqlEntityId>, Error> {
        let bound = self.bound;
        let fq = &*self.queries;
        let conn = self.conn.conn();
        match stream {
            EntityStream::ByName { name, language } => {
                let key = FacetKey::Name {
                    norm: normalize_name(name),
                    language: language.as_str(),
                };
                read::keyed_class_page(conn, bound, fq, key, named_entity, after, limit).await
            }
            EntityStream::ByExternalReference { reference } => {
                let key = FacetKey::ExternalRef(external_ref_key(reference)?);
                read::keyed_class_page(conn, bound, fq, key, referenced_entity, after, limit).await
            }
            EntityStream::All => read::all_class_page(conn, bound, fq, after, limit).await,
            EntityStream::InViewport(viewport) => {
                read::spatial_entity_page(conn, bound, fq, viewport, after, limit).await
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
        let fq = &*self.queries;
        read::cluster_entities_in_viewport(self.conn.conn(), self.bound, fq, viewport, level, rank)
            .await
    }

    async fn cluster_tile_cells(
        &mut self,
        tile: TileId,
        rank: RankKey,
    ) -> Result<Vec<ClusterCell<SqlEntityId>>, Error> {
        let fq = &*self.queries;
        read::cluster_tile_cells(self.conn.conn(), self.bound, fq, tile, rank).await
    }

    async fn all_facts_about_entity(
        &mut self,
        entity: &SqlEntityId,
        after: Option<FactId>,
        limit: std::num::NonZeroUsize,
    ) -> Result<WalkPage<SqliteFactStore, SqlEntityId>, Error> {
        read::backlink_page(self.conn.conn(), self.bound, *entity, after, limit).await
    }

    async fn walk_entity_depictions<'b>(
        &'b mut self,
        entity: &'b SqlEntityId,
        after: Option<(SqlImageId, FactId)>,
        limit: std::num::NonZeroUsize,
    ) -> Result<DepictionWalkPage<SqliteFactStore>, Error> {
        let fq = &*self.queries;
        read::depiction_page(self.conn.conn(), self.bound, fq, *entity, after, limit).await
    }
}

impl<C: AsConn> EventView<SqliteFactStore> for SqliteHandle<C> {
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
    ) -> Result<WalkPage<SqliteFactStore, SqlEventId>, Error> {
        read::backlink_page(self.conn.conn(), self.bound, *event, after, limit).await
    }

    async fn all_has_events_about_event(
        &mut self,
        event: &SqlEventId,
        after: Option<FactId>,
        limit: std::num::NonZeroUsize,
    ) -> Result<WalkPage<SqliteFactStore, SqlEventId>, Error> {
        read::has_event_backlink_page(self.conn.conn(), self.bound, *event, after, limit).await
    }
}

impl<C: AsConn> ImageView<SqliteFactStore> for SqliteHandle<C> {
    /// One `RESOLVE_REPS` query resolves the whole batch in a single round trip.
    async fn image_representatives(
        &mut self,
        members: &[SqlImageId],
    ) -> Result<std::collections::HashMap<SqlImageId, SqlImageId>, Error> {
        let fq = &*self.queries;
        read::representatives(self.conn.conn(), self.bound, fq, members).await
    }

    async fn image_class(&mut self, member: &SqlImageId) -> Result<EquivClass<SqlImageId>, Error> {
        let fq = &*self.queries;
        read::equiv_class(self.conn.conn(), self.bound, fq, *member).await
    }

    /// The temporal streams answer empty pages for the same reason as
    /// `walk_entity_classes`'s.
    async fn walk_image_classes<'b>(
        &'b mut self,
        stream: &'b ImageStream<'b>,
        after: Option<(SqlImageId, FactId)>,
        limit: std::num::NonZeroUsize,
    ) -> Result<ClassWalkPage<SqliteFactStore, SqlImageId>, Error> {
        let bound = self.bound;
        let fq = &*self.queries;
        let conn = self.conn.conn();
        match stream {
            ImageStream::BySourceUrl { url } => {
                let key = FacetKey::SourceUrl(url.as_str());
                read::keyed_class_page(conn, bound, fq, key, sourced_image, after, limit).await
            }
            ImageStream::All => read::all_class_page(conn, bound, fq, after, limit).await,
            ImageStream::InViewport(viewport) => {
                read::spatial_image_page(conn, bound, fq, viewport, after, limit).await
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
    ) -> Result<WalkPage<SqliteFactStore, SqlImageId>, Error> {
        read::backlink_page(self.conn.conn(), self.bound, *image, after, limit).await
    }
}

// ============================================================================
// Temporal-conflict read — the witness-index read path
// ============================================================================

impl<C: AsConn> SqliteHandle<C> {
    /// The entity-level temporal contradictions of `entity`'s class at this
    /// view's snapshot, read via the per-subject witness indexes rather than a
    /// whole-entity projection: the composed read
    /// (`read::temporal_conflict_scan`) followed by
    /// [`conflicts_via_index`](chronoscope_core::solvers::conflicts_via_index).
    /// Equal (as a set) to
    /// [`temporal_conflicts`](chronoscope_core::solvers::temporal_conflicts) over
    /// the projected entity at the same snapshot.
    ///
    /// # Errors
    /// Returns [`SqliteFactStoreError`] on a backend failure.
    pub async fn temporal_conflicts_indexed(
        &mut self,
        entity: SqlEntityId,
    ) -> Result<Vec<chronoscope_core::solvers::TemporalConflict>, Error> {
        let fq = &*self.queries;
        let scan = read::temporal_conflict_scan(self.conn.conn(), self.bound, fq, entity).await?;
        Ok(chronoscope_core::solvers::conflicts_via_index(&scan))
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
        let queries = self.queries.clone();
        let scope_tx = self
            .conn
            .conn()
            .begin()
            .await
            .map_err(sql("opening submit scope"))?;
        let mut scope = SqliteHandle {
            conn: ScopeTx(scope_tx),
            bound: ReadBound::Union,
            queries,
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
        let fq = &*self.queries;
        let conn = self.conn.conn();
        let facets = facet_columns(&fact)?;
        let subjects = subject_rows(&fact);
        // A region location writes a facts_spatial envelope, though it carries no
        // clustering key.
        let spatial = fact
            .located_subject()
            .map(|(location, subject)| (location.bounding_rects(), kind_tag(subject.kind())));
        let witness = witness_row(&fact);
        let fact_json = fact_to_json(fact)?;
        // A RetractCommit facet stores the target's surrogate seq; an
        // unrecorded target resolves NULL, and the validator rejects the
        // bundle before anything reads it. fact_json keeps the raw hash.
        let retracts_commit_seq = match &facets.retracts_commit_id {
            Some(hash) => read::commit_seq(&mut *conn, hash).await?,
            None => None,
        };
        // SQL assigns the dense id (see insert_fact_sql); rows are never
        // deleted, so ids stay dense for the store's life.
        let staged = sqlx::query(&fq.insert_fact)
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
        // fact_id is the rowid alias, so the insert's rowid is the new id.
        let fid_raw = staged.last_insert_rowid();
        for (kind, subject) in subjects {
            sqlx::query(queries::INSERT_SUBJECT.sql)
                .bind(fid_raw)
                .bind(kind)
                .bind(subject)
                .execute(&mut *conn)
                .await
                .map_err(sql("inserting fact subject row"))?;
        }
        if let Some((rects, subject_kind)) = &spatial {
            for rect in rects {
                sqlx::query(queries::INSERT_SPATIAL.sql)
                    .bind(fid_raw)
                    .bind(subject_kind)
                    .bind(rect.min_lon)
                    .bind(rect.min_lat)
                    .bind(rect.max_lon)
                    .bind(rect.max_lat)
                    .execute(&mut *conn)
                    .await
                    .map_err(sql("inserting spatial envelope row"))?;
            }
        }
        // Witness-index maintenance runs on the same connection as the
        // staging, so a rejected submit's savepoint unwinds these rows too.
        if let Some(witness) = witness {
            maintain::record_witness(&mut *conn, fid_raw, witness).await?;
        }
        // Representative-log maintenance runs on the same connection as the
        // staging, so a rejected submit's savepoint unwinds its rep rows
        // with its fact rows.
        if let (Some(kind), Some(a), Some(b)) = (facets.edge_kind, facets.edge_a, facets.edge_b) {
            maintain::record_identity_edge(&mut *conn, fq, kind, a, b, fid_raw).await?;
        }
        if facets.retracts_fact_id.is_some() || retracts_commit_seq.is_some() {
            maintain::record_retraction(&mut *conn, fq, fid_raw).await?;
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
        let insert = &self.queries.insert_commit;
        let conn = self.conn.conn();
        let commit_json = commit_to_json(&commit, result)?;
        let result_json = result_to_json(result)?;
        let inserted = sqlx::query(insert)
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

fn singleton_class<S: Ord + Copy>(member: S) -> EquivClass<S> {
    EquivClass {
        representative: member,
        members: std::iter::once(member).collect(),
    }
}
