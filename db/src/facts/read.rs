//! Snapshot-scoped read implementations over one SQLite connection.
//!
//! Both read paths come through here: a [`SqliteFactView`](super::SqliteFactView)
//! passes its owned read-transaction connection and sees committed rows, a
//! [`SqliteTx`](super::SqliteTx) passes the write transaction's and sees
//! committed ∪ staged — the union view is the connection's own uncommitted
//! visibility, no overlay bookkeeping.
//!
//! Retraction filtering is two-phase everywhere: one batched
//! [`RETRACTOR_CLOSURE`](super::queries::RETRACTOR_CLOSURE) fetch for the
//! rows in hand, then the shared fixpoint
//! ([`chronoscope_core::store::retraction`]) in Rust.

use sqlx::SqliteConnection;

use chronoscope_core::grammar::ids::FactId;
use chronoscope_core::store::FactPlacement;
use chronoscope_core::store::equiv::EquivAdjacency;
use chronoscope_core::store::retraction::{RetractionEdges, effective_retractor};
use chronoscope_core::store::schema::{EquivClass, FactPage, PageItem};
use chronoscope_core::submit::{FactLookup, StoredFact, SubmitResult};

use super::convert::{i64_to_u64, u64_to_i64};
use super::error::{SqliteFactStoreError, json, sql};
use super::ids::SqliteIds;
use super::queries;
use super::storage::{SubjectColumn, fact_from_json, kind_tag, result_from_json};

/// A read's visibility: a pool view's pinned exclusive upper bound, or the
/// whole of what the connection sees (a transaction's union view).
#[derive(Clone, Copy)]
pub(super) enum ReadBound {
    Pinned(FactId),
    Union,
}

impl ReadBound {
    /// SQL bind form. Stored fact ids all fit `i64`, so the union view (and
    /// any larger pin) admits the same rows as the maximum.
    fn bind(self) -> i64 {
        match self {
            ReadBound::Pinned(snapshot) => i64::try_from(snapshot.get()).unwrap_or(i64::MAX),
            ReadBound::Union => i64::MAX,
        }
    }

    /// The bound as a [`FactId`] for the shared retraction fixpoint.
    fn fact_id(self) -> FactId {
        match self {
            ReadBound::Pinned(snapshot) => snapshot,
            ReadBound::Union => FactId::new(u64::MAX),
        }
    }
}

/// One past the highest stored fact id; 0 on an empty store.
pub(super) async fn next_fact_id(conn: &mut SqliteConnection) -> Result<u64, SqliteFactStoreError> {
    let (next,): (i64,) = sqlx::query_as(queries::NEXT_FACT_ID.sql)
        .fetch_one(&mut *conn)
        .await
        .map_err(sql("reading next fact id"))?;
    Ok(i64_to_u64(next, "next fact id")?)
}

/// The retractor-closure edges for `seeds`, fetched once per batch of rows
/// under consideration and resolved in memory by the shared fixpoint
/// ([`effective_retractor`]).
pub(super) async fn retraction_edges(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    seeds: &[FactId],
) -> Result<RetractionEdges, SqliteFactStoreError> {
    if seeds.is_empty() {
        return Ok(RetractionEdges::from_edges(std::iter::empty()));
    }
    let seed_ids: Vec<i64> = seeds
        .iter()
        .map(|fid| u64_to_i64(fid.get(), "retraction seed fact id"))
        .collect::<Result<_, _>>()?;
    let seed_json =
        serde_json::to_string(&seed_ids).map_err(json("encoding retraction seed list"))?;
    let rows: Vec<(i64, i64)> = sqlx::query_as(queries::RETRACTOR_CLOSURE.sql)
        .bind(&seed_json)
        .bind(bound.bind())
        .fetch_all(&mut *conn)
        .await
        .map_err(sql("fetching retractor closure"))?;
    let edges: Vec<(FactId, FactId)> = rows
        .into_iter()
        .map(|(target, retractor)| {
            Ok((
                FactId::new(i64_to_u64(target, "retraction target id")?),
                FactId::new(i64_to_u64(retractor, "retractor fact id")?),
            ))
        })
        .collect::<Result<_, SqliteFactStoreError>>()?;
    Ok(RetractionEdges::from_edges(edges))
}

/// Look up a fact under the bound, preserving the four outcomes. Fact rows
/// are dense, so on the union view a missing row means the id is past the
/// staged maximum — `Future`; a pinned view classifies missing rows below
/// its bound as `Unknown`.
pub(super) async fn fact_lookup(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    fact_id: FactId,
) -> Result<FactLookup<SqliteIds>, SqliteFactStoreError> {
    if let ReadBound::Pinned(snapshot) = bound
        && fact_id.get() >= snapshot.get()
    {
        return Ok(FactLookup::Future);
    }
    let missing = match bound {
        ReadBound::Pinned(_) => FactLookup::Unknown,
        ReadBound::Union => FactLookup::Future,
    };
    // An id past the storable range has no row.
    let Ok(fid) = u64_to_i64(fact_id.get(), "fact lookup id") else {
        return Ok(missing);
    };
    let row: Option<(String,)> = sqlx::query_as(queries::FACT_ROW.sql)
        .bind(fid)
        .fetch_optional(&mut *conn)
        .await
        .map_err(sql("fetching fact row"))?;
    let Some((fact_json,)) = row else {
        return Ok(missing);
    };
    let edges = retraction_edges(conn, bound, &[fact_id]).await?;
    match effective_retractor(fact_id, bound.fact_id(), &edges) {
        Some(by) => Ok(FactLookup::Retracted { by }),
        None => Ok(FactLookup::Active(Box::new(fact_from_json(&fact_json)?))),
    }
}

/// Whether a commit id is recorded — committed, or recorded earlier in the
/// calling transaction.
pub(super) async fn commit_known(
    conn: &mut SqliteConnection,
    id: &chronoscope_core::grammar::ids::CommitId,
) -> Result<bool, SqliteFactStoreError> {
    let row: Option<(i64,)> = sqlx::query_as(queries::COMMIT_KNOWN.sql)
        .bind(id.as_str())
        .fetch_optional(&mut *conn)
        .await
        .map_err(sql("checking commit existence"))?;
    Ok(row.is_some())
}

/// The surrogate `commit_seq` recorded under a commit hash; `None` while
/// the hash is unrecorded.
pub(super) async fn commit_seq(
    conn: &mut SqliteConnection,
    commit_id: &str,
) -> Result<Option<i64>, SqliteFactStoreError> {
    let row: Option<(i64,)> = sqlx::query_as(queries::COMMIT_SEQ.sql)
        .bind(commit_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(sql("resolving commit seq"))?;
    Ok(row.map(|(seq,)| seq))
}

/// The cached submit result under a commit id.
pub(super) async fn cached_result(
    conn: &mut SqliteConnection,
    id: &chronoscope_core::grammar::ids::CommitId,
) -> Result<Option<SubmitResult<SqliteIds>>, SqliteFactStoreError> {
    let row: Option<(String,)> = sqlx::query_as(queries::CACHED_RESULT.sql)
        .bind(id.as_str())
        .fetch_optional(&mut *conn)
        .await
        .map_err(sql("fetching cached submit result"))?;
    row.map(|(result_json,)| result_from_json(&result_json))
        .transpose()
}

/// Where a fact id sits relative to the snapshot. A row whose commit hasn't
/// recorded reads `InFlight`; a pool-side view never sees one, so it reports
/// only `Committed` / `Absent`.
pub(super) async fn placement(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    id: FactId,
) -> Result<FactPlacement, SqliteFactStoreError> {
    if let ReadBound::Pinned(snapshot) = bound
        && id.get() >= snapshot.get()
    {
        return Ok(FactPlacement::Absent);
    }
    // Rows are dense, so a missing row (any unstorable id included) is past
    // the staged maximum.
    let Ok(fid) = u64_to_i64(id.get(), "placement fact id") else {
        return Ok(FactPlacement::Absent);
    };
    let row: Option<(bool,)> = sqlx::query_as(queries::PLACEMENT_ROW.sql)
        .bind(fid)
        .fetch_optional(&mut *conn)
        .await
        .map_err(sql("fetching fact placement"))?;
    Ok(match row {
        None => FactPlacement::Absent,
        Some((true,)) => FactPlacement::Committed,
        Some((false,)) => FactPlacement::InFlight,
    })
}

/// The equivalence class of `member` under its kind's canonical identity
/// edges at the snapshot: component fetch (retracted edges included), one
/// batched retractor closure over the edge facts, then the shared adjacency
/// walk from `member` over active edges only.
pub(super) async fn equiv_class<S: SubjectColumn>(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    member: S,
) -> Result<EquivClass<S>, SqliteFactStoreError> {
    let rows: Vec<(i64, i64, i64)> = sqlx::query_as(queries::EQUIV_COMPONENT.sql)
        .bind(member.raw())
        .bind(kind_tag(S::KIND))
        .bind(bound.bind())
        .fetch_all(&mut *conn)
        .await
        .map_err(sql("fetching identity-edge component"))?;
    let seeds: Vec<FactId> = rows
        .iter()
        .map(|(fid, _, _)| Ok(FactId::new(i64_to_u64(*fid, "component edge fact id")?)))
        .collect::<Result<_, SqliteFactStoreError>>()?;
    let retraction = retraction_edges(conn, bound, &seeds).await?;
    let mut edges = Vec::with_capacity(rows.len());
    for ((_, a, b), fid) in rows.iter().zip(&seeds) {
        if effective_retractor(*fid, bound.fact_id(), &retraction).is_some() {
            continue;
        }
        edges.push((S::from_raw(*a), S::from_raw(*b)));
    }
    Ok(EquivAdjacency::from_edges(edges).class_of(member))
}

/// One page of the facts mentioning `subject`, active at the snapshot,
/// resuming strictly past `after`. Candidates come off the subject index in
/// id order; a retracted candidate is dropped after the batched closure
/// resolves, so a page can come back short yet still carry a resume cursor
/// (the last candidate consumed, active or not — resumption never re-reads
/// or skips a candidate).
pub(super) async fn backlink_page<S: SubjectColumn>(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    subject: S,
    after: Option<FactId>,
    limit: std::num::NonZeroUsize,
) -> Result<FactPage<StoredFact<SqliteIds>, S, FactId>, SqliteFactStoreError> {
    // -1 sits below every stored id, so it opens the strictly-past scan.
    let cursor_bind = match after {
        None => -1,
        Some(cursor) => u64_to_i64(cursor.get(), "backlink resume cursor")?,
    };
    // Fetch one past the page to learn whether candidates remain.
    let fetch = i64::try_from(limit.get())
        .unwrap_or(i64::MAX)
        .saturating_add(1);
    let mut rows: Vec<(i64, String)> = sqlx::query_as(queries::BACKLINK_PAGE.sql)
        .bind(kind_tag(S::KIND))
        .bind(subject.raw())
        .bind(cursor_bind)
        .bind(bound.bind())
        .bind(fetch)
        .fetch_all(&mut *conn)
        .await
        .map_err(sql("fetching backlink page"))?;
    let more = rows.len() > limit.get();
    rows.truncate(limit.get());

    let seeds: Vec<FactId> = rows
        .iter()
        .map(|(fid, _)| Ok(FactId::new(i64_to_u64(*fid, "backlink fact id")?)))
        .collect::<Result<_, SqliteFactStoreError>>()?;
    let retraction = retraction_edges(conn, bound, &seeds).await?;

    let mut items = Vec::with_capacity(rows.len());
    for ((_, fact_json), fid) in rows.iter().zip(&seeds) {
        if effective_retractor(*fid, bound.fact_id(), &retraction).is_some() {
            continue;
        }
        items.push(PageItem {
            fact_id: *fid,
            fact: fact_from_json(fact_json)?,
            representative: subject,
        });
    }
    let next_cursor = if more { seeds.last().copied() } else { None };
    Ok(FactPage { items, next_cursor })
}
