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
//!
//! Representatives and classes resolve through the `subject_reps` log
//! ([`resolve_rep_raw`] / [`class_members_raw`]): the write path
//! ([`super::maintain`]) keeps the log equal to the live-edge components at
//! every snapshot, so no read walks identity edges.

use std::collections::{BTreeMap, BTreeSet};

use sqlx::SqliteConnection;

use chronoscope_core::algebra::lattice::JoinSemilattice;
use chronoscope_core::date::UncertainDate;
use chronoscope_core::geo::{QuadLevel, QuadTileRange, TileId, Viewport};
use chronoscope_core::grammar::event;
use chronoscope_core::grammar::ids::FactId;
use chronoscope_core::solvers::WitnessScan;
use chronoscope_core::store::FactPlacement;
use chronoscope_core::store::pagination;
use chronoscope_core::store::retraction::{RetractionEdges, effective_retractor};
use chronoscope_core::store::schema::{
    CELL_DEPTH, CLUSTER_TILE_N, ClassPage, ClassRow, ClusterCell, DepictionPage, EquivClass,
    FactPage, PageItem, RankKey, cluster_tile_ranges,
};
use chronoscope_core::submit::{FactLookup, LocatedSubject, StoredFact, SubmitResult};

use super::error::{SqliteFactStoreError, sql};
use super::queries::{self, FactQueries};
use crate::common::cluster::{
    LocatedSubjects, OwnerCandidates, OwnerRow, place_candidates, resolve_entities,
};
use crate::common::convert::{i64_to_u64, seed_ids, u64_to_i64};
use crate::common::error::json;
use crate::common::ids::{SqlEntityId, SqlEventId, SqlIds, SqlImageId};
use crate::common::spatial::{dedup_by_fact, refine_in_viewport};
use crate::common::storage::{
    SubjectColumn, day_number, depiction_subjects, fact_from_json, kind_tag, result_from_json,
};

/// A read's visibility: a pool view's pinned exclusive upper bound, or the
/// whole of what the connection sees (a transaction's union view). The resolved
/// base-aware SQL rides on the store's [`FactQueries`] cache, passed alongside.
#[derive(Clone, Copy)]
pub(super) enum ReadBound {
    Pinned(FactId),
    Union,
}

impl ReadBound {
    /// The pinned snapshot, or `None` for the union view.
    pub(super) fn snapshot(self) -> Option<FactId> {
        match self {
            ReadBound::Pinned(snapshot) => Some(snapshot),
            ReadBound::Union => None,
        }
    }

    /// SQL bind form. Stored fact ids all fit `i64`, so the union view (and
    /// any larger pin) admits the same rows as the maximum.
    pub(super) fn bind(self) -> i64 {
        match self {
            ReadBound::Pinned(snapshot) => i64::try_from(snapshot.get()).unwrap_or(i64::MAX),
            ReadBound::Union => i64::MAX,
        }
    }

    /// The bound as a [`FactId`] for the shared retraction fixpoint.
    pub(super) fn fact_id(self) -> FactId {
        match self {
            ReadBound::Pinned(snapshot) => snapshot,
            ReadBound::Union => FactId::new(u64::MAX),
        }
    }
}

/// One past the highest stored fact id over the mounted layers; 0 on an empty
/// store.
pub(super) async fn next_fact_id(
    conn: &mut SqliteConnection,
    fq: &FactQueries,
) -> Result<u64, SqliteFactStoreError> {
    let (next,): (i64,) = sqlx::query_as(&fq.next_fact_id)
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
) -> Result<FactLookup<SqlIds>, SqliteFactStoreError> {
    if let Some(snapshot) = bound.snapshot()
        && fact_id.get() >= snapshot.get()
    {
        return Ok(FactLookup::Future);
    }
    let missing = match bound.snapshot() {
        Some(_) => FactLookup::Unknown,
        None => FactLookup::Future,
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
) -> Result<Option<SubmitResult<SqlIds>>, SqliteFactStoreError> {
    let row: Option<(String,)> = sqlx::query_as(queries::CACHED_RESULT.sql)
        .bind(id.as_str())
        .fetch_optional(&mut *conn)
        .await
        .map_err(sql("fetching cached submit result"))?;
    Ok(row
        .map(|(result_json,)| result_from_json(&result_json))
        .transpose()?)
}

/// Where a fact id sits relative to the snapshot. A row whose commit hasn't
/// recorded reads `InFlight`; a pool-side view never sees one, so it reports
/// only `Committed` / `Absent`.
pub(super) async fn placement(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    id: FactId,
) -> Result<FactPlacement, SqliteFactStoreError> {
    if let Some(snapshot) = bound.snapshot()
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

/// The representative of a raw subject id at the bound: the log's last row
/// strictly below it, one descending covering seek; no row means the member
/// has always been its own representative.
pub(super) async fn resolve_rep_raw(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    fq: &FactQueries,
    kind: &str,
    member: i64,
) -> Result<i64, SqliteFactStoreError> {
    let row: Option<(i64,)> = sqlx::query_as(&fq.resolve_rep)
        .bind(kind)
        .bind(member)
        .bind(bound.bind())
        .fetch_optional(&mut *conn)
        .await
        .map_err(sql("resolving subject representative"))?;
    Ok(row.map_or(member, |(rep,)| rep))
}

/// The raw members whose latest log row at the bound names `rep` as their
/// representative. The representative itself appears only when it carries a
/// self row (after a split); callers add it.
pub(super) async fn class_members_raw(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    kind: &str,
    rep: i64,
) -> Result<Vec<i64>, SqliteFactStoreError> {
    let rows: Vec<(i64,)> = sqlx::query_as(queries::CLASS_MEMBERS.sql)
        .bind(kind)
        .bind(rep)
        .bind(bound.bind())
        .fetch_all(&mut *conn)
        .await
        .map_err(sql("gathering class members"))?;
    Ok(rows.into_iter().map(|(member,)| member).collect())
}

/// [`resolve_rep_raw`] behind a per-walk cache: each distinct member costs
/// one log seek, repeats are free.
async fn resolve_rep_cached(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    fq: &FactQueries,
    kind: &str,
    member: i64,
    cache: &mut std::collections::HashMap<i64, i64>,
) -> Result<i64, SqliteFactStoreError> {
    if let Some(rep) = cache.get(&member) {
        return Ok(*rep);
    }
    let rep = resolve_rep_raw(conn, bound, fq, kind, member).await?;
    cache.insert(member, rep);
    Ok(rep)
}

/// The class representative of `member` at the snapshot — one log seek.
pub(super) async fn representative<S: SubjectColumn>(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    fq: &FactQueries,
    member: S,
) -> Result<S, SqliteFactStoreError> {
    let rep = resolve_rep_raw(conn, bound, fq, kind_tag(S::KIND), member.raw()).await?;
    Ok(S::from_raw(rep))
}

/// The class representatives of a batch of members at the snapshot, resolved
/// in one query: each member's last log row below the bound, or the member
/// itself where it has none — the set-based analogue of [`representative`],
/// applying the same shared rule per member. Every input member appears in
/// the returned map.
pub(super) async fn representatives<S: SubjectColumn + std::hash::Hash>(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    fq: &FactQueries,
    members: &[S],
) -> Result<std::collections::HashMap<S, S>, SqliteFactStoreError> {
    if members.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let raw_ids: Vec<i64> = members.iter().map(|m| m.raw()).collect();
    let members_json =
        serde_json::to_string(&raw_ids).map_err(json("encoding representative member list"))?;
    let rows: Vec<(i64, i64)> = sqlx::query_as(&fq.resolve_reps)
        .bind(kind_tag(S::KIND))
        .bind(&members_json)
        .bind(bound.bind())
        .fetch_all(&mut *conn)
        .await
        .map_err(sql("resolving subject representatives"))?;
    Ok(rows
        .into_iter()
        .map(|(member, rep)| (S::from_raw(member), S::from_raw(rep)))
        .collect())
}

/// The equivalence class of `member` at the snapshot, read off the
/// representative log: one seek to the representative, then the reverse
/// gather, plus the representative itself.
pub(super) async fn equiv_class<S: SubjectColumn>(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    fq: &FactQueries,
    member: S,
) -> Result<EquivClass<S>, SqliteFactStoreError> {
    let rep = resolve_rep_raw(conn, bound, fq, kind_tag(S::KIND), member.raw()).await?;
    let mut members: std::collections::BTreeSet<S> =
        class_members_raw(conn, bound, kind_tag(S::KIND), rep)
            .await?
            .into_iter()
            .map(S::from_raw)
            .collect();
    members.insert(S::from_raw(rep));
    Ok(EquivClass {
        representative: S::from_raw(rep),
        members,
    })
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
) -> Result<FactPage<StoredFact<SqlIds>, S, FactId>, SqliteFactStoreError> {
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

    let seeds = seed_ids(rows.iter().map(|(fid, _)| fid), "backlink fact id")?;
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

/// One page of `event`'s `HasEvent` facts, active *or retracted*, resuming
/// strictly past `after`. The retraction-inclusive twin of [`backlink_page`]
/// for the ownership rule: it drops the retraction filter and keeps only
/// `HasEvent` facts, decoding each candidate's fact to test its variant. A
/// non-`HasEvent` candidate is dropped without consuming a slot, so a page can
/// come short yet still carry a resume cursor (the last candidate consumed,
/// whatever its variant).
pub(super) async fn has_event_backlink_page(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    event: SqlEventId,
    after: Option<FactId>,
    limit: std::num::NonZeroUsize,
) -> Result<FactPage<StoredFact<SqlIds>, SqlEventId, FactId>, SqliteFactStoreError> {
    // -1 sits below every stored id, so it opens the strictly-past scan.
    let cursor_bind = match after {
        None => -1,
        Some(cursor) => u64_to_i64(cursor.get(), "has-event backlink resume cursor")?,
    };
    // Fetch one past the page to learn whether candidates remain.
    let fetch = i64::try_from(limit.get())
        .unwrap_or(i64::MAX)
        .saturating_add(1);
    let mut rows: Vec<(i64, String)> = sqlx::query_as(queries::BACKLINK_PAGE.sql)
        .bind(kind_tag(SqlEventId::KIND))
        .bind(event.raw())
        .bind(cursor_bind)
        .bind(bound.bind())
        .bind(fetch)
        .fetch_all(&mut *conn)
        .await
        .map_err(sql("fetching has-event backlink page"))?;
    let more = rows.len() > limit.get();
    rows.truncate(limit.get());

    let seeds = seed_ids(
        rows.iter().map(|(fid, _)| fid),
        "has-event backlink fact id",
    )?;
    let mut items = Vec::with_capacity(rows.len());
    for ((_, fact_json), fid) in rows.iter().zip(&seeds) {
        let fact = fact_from_json(fact_json)?;
        if matches!(fact.event_fact(), Some(event::Fact::HasEvent { .. })) {
            items.push(PageItem {
                fact_id: *fid,
                fact,
                representative: event,
            });
        }
    }
    let next_cursor = if more { seeds.last().copied() } else { None };
    Ok(FactPage { items, next_cursor })
}

/// The facet key a keyed class walk fetches candidates under. Each variant
/// carries exactly the columns of its partial index; the key values arrive
/// pre-encoded by the same functions the write path uses ([`normalize_name`]
/// for names, [`external_ref_key`](crate::common::storage::external_ref_key) for
/// references), so the walk and the stored facets cannot drift.
///
/// [`normalize_name`]: chronoscope_core::store::schema::normalize_name
pub(super) enum FacetKey<'a> {
    Name { norm: String, language: &'a str },
    ExternalRef(String),
    SourceUrl(&'a str),
}

impl FacetKey<'_> {
    /// Fetch this key's `(fact_id, fact_json)` candidates below the bound.
    async fn candidates(
        &self,
        conn: &mut SqliteConnection,
        bound: ReadBound,
    ) -> Result<Vec<(i64, String)>, SqliteFactStoreError> {
        match self {
            FacetKey::Name { norm, language } => {
                sqlx::query_as(queries::CLASS_CANDIDATES_BY_NAME.sql)
                    .bind(norm)
                    .bind(language)
                    .bind(bound.bind())
                    .fetch_all(&mut *conn)
                    .await
                    .map_err(sql("fetching name-keyed class candidates"))
            }
            FacetKey::ExternalRef(key) => sqlx::query_as(queries::CLASS_CANDIDATES_BY_EXTREF.sql)
                .bind(key)
                .bind(bound.bind())
                .fetch_all(&mut *conn)
                .await
                .map_err(sql("fetching reference-keyed class candidates")),
            FacetKey::SourceUrl(url) => sqlx::query_as(queries::CLASS_CANDIDATES_BY_SRCURL.sql)
                .bind(url)
                .bind(bound.bind())
                .fetch_all(&mut *conn)
                .await
                .map_err(sql("fetching url-keyed class candidates")),
        }
    }
}

/// The tail every flat class walk shares: resolve each `(subject, fact_id)`
/// pair to its class representative — one cached log seek per distinct
/// subject — and cut the `(representative, fact_id)` rows with the
/// backend-shared cursor semantics. The set fold also dedups pairs that
/// arrive twice.
async fn rep_class_page<S: SubjectColumn>(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    fq: &FactQueries,
    subjects: Vec<(S, FactId)>,
    after: Option<(S, FactId)>,
    limit: std::num::NonZeroUsize,
) -> Result<ClassPage<S, (S, FactId)>, SqliteFactStoreError> {
    let mut reps: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
    let mut rows: std::collections::BTreeSet<(S, FactId)> = std::collections::BTreeSet::new();
    for (subject, fid) in subjects {
        let rep = resolve_rep_cached(conn, bound, fq, kind_tag(S::KIND), subject.raw(), &mut reps)
            .await?;
        rows.insert((S::from_raw(rep), fid));
    }
    Ok(pagination::class_page(&rows, after, limit))
}

/// One page of a keyed class walk (`ByName` / `ByExternalReference` /
/// `BySourceUrl`). The candidate set is key-sized, so the walk fetches it
/// whole: candidates off the facet index, one batched retractor closure,
/// `subject_of` on each survivor's decoded fact, then the shared
/// [`rep_class_page`] tail.
pub(super) async fn keyed_class_page<S: SubjectColumn>(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    fq: &FactQueries,
    key: FacetKey<'_>,
    subject_of: impl Fn(&StoredFact<SqlIds>) -> Option<S>,
    after: Option<(S, FactId)>,
    limit: std::num::NonZeroUsize,
) -> Result<ClassPage<S, (S, FactId)>, SqliteFactStoreError> {
    let candidates = key.candidates(conn, bound).await?;
    let seeds = seed_ids(
        candidates.iter().map(|(fid, _)| fid),
        "class candidate fact id",
    )?;
    let retraction = retraction_edges(conn, bound, &seeds).await?;

    let mut subjects: Vec<(S, FactId)> = Vec::new();
    for ((_, fact_json), fid) in candidates.iter().zip(&seeds) {
        if effective_retractor(*fid, bound.fact_id(), &retraction).is_some() {
            continue;
        }
        let Some(subject) = subject_of(&fact_from_json(fact_json)?) else {
            continue;
        };
        subjects.push((subject, *fid));
    }
    rep_class_page(conn, bound, fq, subjects, after, limit).await
}

/// One page of the depiction walk: the depiction facts of `entity`'s
/// `SameEntity` class, each row keeping its whole fact under the depicted
/// image's `SameArtifact` representative, ordered `(image_rep, fact_id)`.
/// Candidates are the class members' backlink sets filtered to depictions of
/// the class; one batched retractor closure gates them and one log seek per
/// distinct depicted image resolves the grouping representative. The
/// backend-shared [`pagination::grouped_class_page`] cuts the page by whole
/// images — `limit` counts distinct image representatives and every fact of
/// an included image enters the page.
pub(super) async fn depiction_page(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    fq: &FactQueries,
    entity: SqlEntityId,
    after: Option<(SqlImageId, FactId)>,
    limit: std::num::NonZeroUsize,
) -> Result<DepictionPage<StoredFact<SqlIds>, SqlImageId, (SqlImageId, FactId)>, SqliteFactStoreError>
{
    let members = equiv_class(conn, bound, fq, entity).await?.members;
    // A depiction names one entity, so each candidate appears under exactly
    // one member; the map keys by fact id regardless.
    let mut candidates: std::collections::BTreeMap<i64, String> = std::collections::BTreeMap::new();
    for member in &members {
        let rows: Vec<(i64, String)> = sqlx::query_as(queries::SUBJECT_FACTS.sql)
            .bind(kind_tag(SqlEntityId::KIND))
            .bind(member.raw())
            .bind(bound.bind())
            .fetch_all(&mut *conn)
            .await
            .map_err(sql("fetching depiction candidates"))?;
        candidates.extend(rows);
    }
    let seeds = seed_ids(candidates.keys(), "depiction candidate fact id")?;
    let retraction = retraction_edges(conn, bound, &seeds).await?;

    let mut reps: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
    let mut rows: std::collections::BTreeSet<(SqlImageId, FactId)> =
        std::collections::BTreeSet::new();
    let mut facts: std::collections::BTreeMap<FactId, StoredFact<SqlIds>> =
        std::collections::BTreeMap::new();
    for ((_, fact_json), fid) in candidates.iter().zip(&seeds) {
        if effective_retractor(*fid, bound.fact_id(), &retraction).is_some() {
            continue;
        }
        let fact = fact_from_json(fact_json)?;
        let Some((depicted, image)) = depiction_subjects(&fact) else {
            continue;
        };
        if !members.contains(&depicted) {
            continue;
        }
        let rep = resolve_rep_cached(
            conn,
            bound,
            fq,
            kind_tag(SqlImageId::KIND),
            image.raw(),
            &mut reps,
        )
        .await?;
        rows.insert((SqlImageId::from_raw(rep), *fid));
        facts.insert(*fid, fact);
    }

    let (pairs, next_class) = pagination::grouped_class_page(&rows, after, limit);
    let mut page: Vec<PageItem<StoredFact<SqlIds>, SqlImageId>> = Vec::new();
    for (representative, fact_id) in pairs {
        // Every row's fact was kept when the row was built.
        if let Some(fact) = facts.remove(&fact_id) {
            page.push(PageItem {
                fact_id,
                fact,
                representative,
            });
        }
    }
    Ok(DepictionPage {
        rows: page,
        next_class,
    })
}

/// The active location-bearing facts of the stream's subject kinds whose
/// region can meet `viewport`: rtree candidates per viewport half (the
/// core-owned seam split, [`halves`]) narrowed to `kinds` in SQL, one batched
/// retractor closure, then the shared [`refine_in_viewport`] membership
/// decision, which the Postgres backend runs on its own fetch, so the two
/// cannot disagree on who is in the box.
///
/// [`halves`]: chronoscope_core::geo::Viewport::halves
async fn located_in_viewport(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    fq: &FactQueries,
    viewport: &Viewport,
    kinds: (&str, &str),
) -> Result<Vec<(FactId, StoredFact<SqlIds>)>, SqliteFactStoreError> {
    // The spatial read can't span layers through a temp view (an rtree drives
    // its index only when named directly), so the candidate SQL unions the base
    // and overlay rtree branches explicitly when a base is mounted.
    let mut rows: Vec<(i64, String)> = Vec::new();
    for half in viewport.halves() {
        let fetched: Vec<(i64, String)> = sqlx::query_as(&fq.spatial_candidates)
            .bind(half.min_lat)
            .bind(half.max_lat)
            .bind(half.min_lon)
            .bind(half.max_lon)
            .bind(bound.bind())
            .bind(kinds.0)
            .bind(kinds.1)
            .fetch_all(&mut *conn)
            .await
            .map_err(sql("fetching spatial candidates"))?;
        rows.extend(fetched);
    }
    let candidates = dedup_by_fact(rows, "spatial candidate fact id")?;
    let seeds: Vec<FactId> = candidates.iter().map(|(fid, _)| *fid).collect();
    let retraction = retraction_edges(conn, bound, &seeds).await?;
    Ok(refine_in_viewport(
        &candidates,
        bound.fact_id(),
        &retraction,
        viewport,
    )?)
}

/// The owning entity of each of `events` at the snapshot. One batched
/// [`EVENT_OWNERS`](super::queries::EVENT_OWNERS) fetch (an indexed probe per
/// event) reads owners off the `event_owner` facet without decoding a fact,
/// then one batched retractor closure feeds the shared live-owner rule
/// ([`OwnerCandidates`]).
async fn event_owners(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    events: &BTreeSet<SqlEventId>,
) -> Result<BTreeMap<SqlEventId, SqlEntityId>, SqliteFactStoreError> {
    if events.is_empty() {
        return Ok(BTreeMap::new());
    }
    let event_ids: Vec<i64> = events.iter().map(|event| event.raw()).collect();
    let event_json = serde_json::to_string(&event_ids).map_err(json("encoding event id list"))?;
    // `SELECT fact_subjects.fact_id, fact_subjects.subject_id, facts.event_owner`
    // — named into `OwnerRow` right here, since the seam has no other tie to this
    // statement's column order.
    let rows: Vec<(i64, i64, i64)> = sqlx::query_as(queries::EVENT_OWNERS.sql)
        .bind(&event_json)
        .bind(kind_tag(SqlEventId::KIND))
        .bind(bound.bind())
        .fetch_all(&mut *conn)
        .await
        .map_err(sql("fetching event owners"))?;
    let candidates =
        OwnerCandidates::from_rows(rows.into_iter().map(|(fact_id, subject_id, owner)| {
            OwnerRow {
                fact_id,
                event: SqlEventId::from_raw(subject_id),
                owner: SqlEntityId::from_raw(owner),
            }
        }))?;
    let retraction = retraction_edges(conn, bound, &candidates.seeds()).await?;
    Ok(candidates.live(bound.fact_id(), &retraction))
}

/// One page of the entity `InViewport` class walk: [`located_in_viewport`] facts of
/// the entity and event kinds, construction bookends attributed to their own
/// entity and `MovedToLocation` facts to their [`event_owners`] entity (an
/// orphaned move attributes to nothing), then the shared [`rep_class_page`]
/// tail.
pub(super) async fn spatial_entity_page(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    fq: &FactQueries,
    viewport: &Viewport,
    after: Option<(SqlEntityId, FactId)>,
    limit: std::num::NonZeroUsize,
) -> Result<ClassPage<SqlEntityId, (SqlEntityId, FactId)>, SqliteFactStoreError> {
    let kinds = (kind_tag(SqlEntityId::KIND), kind_tag(SqlEventId::KIND));
    let located = located_in_viewport(conn, bound, fq, viewport, kinds).await?;
    let split = LocatedSubjects::partition(&located);
    let owners = event_owners(conn, bound, &split.events()).await?;
    rep_class_page(conn, bound, fq, split.attribute(&owners), after, limit).await
}

/// One page of the image `InViewport` class walk: [`located_in_viewport`] facts of
/// the image kind (`CapturedLocation`), then the shared [`rep_class_page`]
/// tail.
pub(super) async fn spatial_image_page(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    fq: &FactQueries,
    viewport: &Viewport,
    after: Option<(SqlImageId, FactId)>,
    limit: std::num::NonZeroUsize,
) -> Result<ClassPage<SqlImageId, (SqlImageId, FactId)>, SqliteFactStoreError> {
    let kind = kind_tag(SqlImageId::KIND);
    let located = located_in_viewport(conn, bound, fq, viewport, (kind, kind)).await?;
    let mut subjects: Vec<(SqlImageId, FactId)> = Vec::new();
    for (fid, fact) in &located {
        if let Some((_, LocatedSubject::Image(image))) = fact.located_subject() {
            subjects.push((*image, *fid));
        }
    }
    rep_class_page(conn, bound, fq, subjects, after, limit).await
}

/// Attribute a batch of located facts to their entity representatives through
/// the shared [`resolve_entities`] sequence, driving it with this backend's two
/// batched fetches: [`event_owners`] and [`representatives`].
async fn resolve_located_entities(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    fq: &FactQueries,
    located: LocatedSubjects,
) -> Result<Vec<(SqlEntityId, FactId)>, SqliteFactStoreError> {
    resolve_entities(
        located,
        conn,
        async |conn: &mut SqliteConnection, events: &BTreeSet<SqlEventId>| {
            event_owners(conn, bound, events).await
        },
        async |conn: &mut SqliteConnection, members: &[SqlEntityId]| {
            representatives(conn, bound, fq, members).await
        },
    )
    .await
}

/// The bounded core under both clustering reads: fetch each of `ranges` for its
/// `(quadkey, fact_id)`-lowest [`CLUSTER_TILE_N`] entity/event candidates
/// ([`cluster_tile`](super::queries::FactQueries::cluster_tile), whose `LIMIT`
/// caps each layer branch at N rows), then fold each range to at most one
/// [`ClusterCell`]. The range's index is the fold's bucket key, so ranges never
/// bleed together.
///
/// Over a mounted base the union yields up to 2N rows per range (a layer-local
/// top-N each), so each bucket is **re-truncated in Rust** to its `(quadkey,
/// fact_id)`-lowest [`CLUSTER_TILE_N`] before retraction — the same cut the
/// memory oracle applies and the same one the single-table `LIMIT` gives, since
/// the per-layer id spaces are disjoint (base-top-N ∪ overlay-top-N ⊇
/// global-top-N). Feeding the fold the un-truncated ≤2N rows would misread the
/// cell's kind, members, or split level, so the cut must precede the fold. The
/// overlay-only path holds ≤N per bucket, so the truncate is a no-op there.
///
/// The batch then goes through the shared tail — one retractor closure into
/// [`place_candidates`], [`resolve_located_entities`], and the placement's fold
/// — which the Postgres backend runs on its own fetch, so the two answer
/// identical cells. `cluster_entities_in_viewport` passes the viewport's tiles
/// and `cluster_tile_cells` the container's child tiles, so a stand-alone tile
/// and the same tile inside a viewport fold to byte-identical cells.
///
/// `ranges` is consumed by value as an [`IntoIterator`], so the per-tile read
/// streams its lazy [`TileId::child_ranges`] iterator without ever collecting it,
/// while the viewport read hands over its already-built `Vec`.
async fn cluster_ranges(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    fq: &FactQueries,
    ranges: impl IntoIterator<Item = QuadTileRange>,
) -> Result<Vec<ClusterCell<SqlEntityId>>, SqliteFactStoreError> {
    let limit = i64::try_from(CLUSTER_TILE_N).unwrap_or(i64::MAX);
    // One indexed top-N fetch per range (each layer branch's `LIMIT` caps it),
    // grouped by the range's index — the fold's bucket key.
    let mut bucket_rows: BTreeMap<usize, Vec<(i64, FactId, StoredFact<SqlIds>)>> = BTreeMap::new();
    for (bucket, range) in ranges.into_iter().enumerate() {
        let rows: Vec<(i64, String, i64)> = sqlx::query_as(&fq.cluster_tile)
            .bind(range.lo)
            .bind(range.hi)
            .bind(bound.bind())
            .bind(limit)
            .fetch_all(&mut *conn)
            .await
            .map_err(sql("fetching cluster tile candidates"))?;
        let entry = bucket_rows.entry(bucket).or_default();
        for (fid, fact_json, quadkey) in rows {
            entry.push((
                quadkey,
                FactId::new(i64_to_u64(fid, "cluster candidate fact id")?),
                fact_from_json(&fact_json)?,
            ));
        }
    }

    // Re-truncate each bucket to its (quadkey, fact_id)-lowest CLUSTER_TILE_N
    // before retraction (see the doc above): the union path holds up to 2N per
    // bucket, and the whole fold reads the survivor slice, so it must see the
    // single-table top-N. A no-op on the ≤N overlay-only path.
    let mut candidates: Vec<(usize, FactId, i64, StoredFact<SqlIds>)> = Vec::new();
    for (bucket, mut rows) in bucket_rows {
        rows.sort_by_key(|(quadkey, fid, _)| (*quadkey, *fid));
        rows.truncate(CLUSTER_TILE_N);
        for (quadkey, fid, fact) in rows {
            candidates.push((bucket, fid, quadkey, fact));
        }
    }

    let seeds: Vec<FactId> = candidates.iter().map(|(_, fid, _, _)| *fid).collect();
    let retraction = retraction_edges(conn, bound, &seeds).await?;
    let placed = place_candidates(candidates, bound.fact_id(), &retraction);
    let resolved = resolve_located_entities(conn, bound, fq, placed.subjects()).await?;
    Ok(placed.fold(resolved))
}

/// One [`ClusterCell`] per non-empty tile of `viewport` at `level`, ranked by
/// `rank` (`Unranked` = `(quadkey, fact_id)`).
///
/// [`cluster_tile_ranges`] gives the viewport's tiles — the coarse indexed
/// pre-filter ranges — and [`cluster_ranges`] fetches each tile's bounded top-N
/// and folds it. The fold is viewport-free, so a fringe tile the viewport only
/// partly covers still yields a cell; empty tiles yield nothing.
///
/// A viewport spanning too many tiles at the level is refused by
/// [`cluster_tile_ranges`] — a level too fine for the span.
pub(super) async fn cluster_entities_in_viewport(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    fq: &FactQueries,
    viewport: &Viewport,
    level: QuadLevel,
    rank: RankKey,
) -> Result<Vec<ClusterCell<SqlEntityId>>, SqliteFactStoreError> {
    // The sole rank; its `(quadkey, fact_id)` order is the fetch order and the
    // per-tile min in the fold.
    match rank {
        RankKey::Unranked => {}
    }
    let tiles = cluster_tile_ranges(viewport, level)?;
    cluster_ranges(conn, bound, fq, tiles).await
}

/// One [`ClusterCell`] per non-empty sub-tile of container `tile` at
/// `level + CELL_DEPTH` — the viewport-free per-tile fold whose cell geometry is
/// keyed by `(snapshot, level, x, y)`.
///
/// [`TileId::child_ranges`] enumerates the container's children `CELL_DEPTH`
/// levels finer — at most `4^CELL_DEPTH` ranges, since the depth folds against
/// the finest level. The children partition the container's Morton block, so each
/// is one sub-tile bucket; folding each through the shared [`cluster_ranges`]
/// applies the **per-sub-tile** top-[`CLUSTER_TILE_N`] cut, keeping a dense
/// low-Morton corner from starving the container's other sub-tiles. That shared
/// tail matches the viewport read, so a stand-alone tile and the same tile inside
/// a viewport fold to byte-identical cells. The child ranges stream lazily into
/// [`cluster_ranges`] — never collected.
pub(super) async fn cluster_tile_cells(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    fq: &FactQueries,
    tile: TileId,
    rank: RankKey,
) -> Result<Vec<ClusterCell<SqlEntityId>>, SqliteFactStoreError> {
    // The sole rank; its `(quadkey, fact_id)` order is the fetch order and the
    // per-sub-tile min in the fold.
    match rank {
        RankKey::Unranked => {}
    }
    cluster_ranges(conn, bound, fq, tile.child_ranges(CELL_DEPTH)).await
}

/// The `(rep, fact_id)` SQL binds for an All-walk cursor. `None` opens the
/// walk (below every real row). The row cursor's fact id always fits `i64`
/// (it names a stored row); the class cursor's `u64::MAX` sentinel clamps to
/// `i64::MAX`, which still sorts past every real row of its representative.
fn all_cursor_binds<S: SubjectColumn>(after: Option<(S, FactId)>) -> (i64, i64) {
    match after {
        None => (i64::MIN, i64::MAX),
        Some((rep, fid)) => (rep.raw(), i64::try_from(fid.get()).unwrap_or(i64::MAX)),
    }
}

/// One page of the All-stream class walk: the single-statement walk
/// ([`class_walk_all_sql`](super::queries::class_walk_all_sql)) fetches one row
/// past the page to learn whether candidates remain, then the batched retractor
/// closure drops retracted candidates in Rust. `next` resumes past the last
/// candidate consumed (active or not), so a page can come back short with a
/// live cursor. `next_class` follows the last emitted row's representative:
/// when the lookahead shares it, a one-row probe past `(rep, MAX)` answers
/// whether a later representative remains; a fully-retracted page carries
/// the row cursor instead, since it emitted no representative to skip.
pub(super) async fn all_class_page<S: SubjectColumn>(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    fq: &FactQueries,
    after: Option<(S, FactId)>,
    limit: std::num::NonZeroUsize,
) -> Result<ClassPage<S, (S, FactId)>, SqliteFactStoreError> {
    let (after_rep, after_fid) = all_cursor_binds(after);
    let fetch = i64::try_from(limit.get())
        .unwrap_or(i64::MAX)
        .saturating_add(1);
    let mut candidates: Vec<(i64, i64)> = sqlx::query_as(&fq.class_walk_all)
        .bind(kind_tag(S::KIND))
        .bind(bound.bind())
        .bind(after_rep)
        .bind(after_fid)
        .bind(fetch)
        .fetch_all(&mut *conn)
        .await
        .map_err(sql("fetching all-stream class page"))?;
    let lookahead = if candidates.len() > limit.get() {
        candidates.pop()
    } else {
        None
    };

    let seeds = seed_ids(
        candidates.iter().map(|(_, fid)| fid),
        "class candidate fact id",
    )?;
    let retraction = retraction_edges(conn, bound, &seeds).await?;
    let mut rows: Vec<ClassRow<S>> = Vec::new();
    for ((rep, _), fid) in candidates.iter().zip(&seeds) {
        if effective_retractor(*fid, bound.fact_id(), &retraction).is_some() {
            continue;
        }
        rows.push(ClassRow {
            representative: S::from_raw(*rep),
            fact_id: *fid,
        });
    }

    let next = match (&lookahead, candidates.last(), seeds.last()) {
        (Some(_), Some((rep, _)), Some(fid)) => Some((S::from_raw(*rep), *fid)),
        _ => None,
    };
    let next_class = match (rows.last(), &lookahead) {
        (_, None) => None,
        // Every candidate on the page was retracted: there is no emitted
        // representative to skip past, so the class cursor continues at the
        // row cursor — strictly forward, never truncating a live class
        // beyond the retracted stretch.
        (None, Some(_)) => next,
        (Some(last), Some((ahead_rep, _))) => {
            let last_raw = last.representative.raw();
            let beyond = if *ahead_rep > last_raw {
                true
            } else {
                // The probe counts candidates without retraction filtering,
                // so a trailing fully-retracted class still answers Some —
                // the consumer's next fetch comes back empty with an
                // exhausted cursor, one extra round trip and correct
                // termination. Deliberate: filtering the probe would cost a
                // retraction closure at every page end.
                let probe: Option<(i64, i64)> = sqlx::query_as(&fq.class_walk_all)
                    .bind(kind_tag(S::KIND))
                    .bind(bound.bind())
                    .bind(last_raw)
                    .bind(i64::MAX)
                    .bind(1_i64)
                    .fetch_optional(&mut *conn)
                    .await
                    .map_err(sql("probing for a later class"))?;
                probe.is_some()
            };
            beyond.then_some(pagination::class_cursor_past(last.representative))
        }
    };
    Ok(ClassPage {
        rows,
        next,
        next_class,
    })
}

// ============================================================================
// Temporal-conflict composed read
// ============================================================================

/// The extent (join) of a set of dated facts — the bookend hull the floor /
/// ceiling read an endpoint off. The same `join_all` `witness_floor` /
/// `witness_ceiling` rebuild core-side, so the scan threshold and the
/// enumerated bound can never disagree.
fn witness_join(facts: &[(FactId, UncertainDate)]) -> UncertainDate {
    UncertainDate::join_all(facts.iter().map(|(_, date)| date.clone()))
}

/// Which bound a value-range witness scan runs against, carrying the threshold
/// day the endpoint is compared to.
#[derive(Clone, Copy)]
enum Threshold {
    /// Below the construction floor: `date_latest < day`.
    Below(i64),
    /// Above the demolition ceiling: `date_earliest > day`.
    Above(i64),
}

/// The JSON array of raw subject ids a batched witness scan binds to its
/// `json_each` driver — one indexed probe per id, the `event_owners` shape.
fn subject_id_json<S: SubjectColumn>(
    ids: &BTreeSet<S>,
    context: &'static str,
) -> Result<String, SqliteFactStoreError> {
    let raw: Vec<i64> = ids.iter().map(|&id| id.raw()).collect();
    Ok(serde_json::to_string(&raw).map_err(json(context))?)
}

/// The events a class owns at the bound: every event some member holds a
/// live `HasEvent` to. Per-edge liveness (the retraction fixpoint over
/// `has_event`) — the ownership hop `project_entity`'s `event_reachers` takes,
/// **not** `event_owners`' latest-owner-wins, so re-owning an event (retract old
/// edge + add new) leaves ownership per-edge and a snapshot where both edges are
/// transiently live counts the event for both.
async fn owned_events(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    members: &BTreeSet<SqlEntityId>,
) -> Result<BTreeSet<SqlEventId>, SqliteFactStoreError> {
    let members_json = subject_id_json(members, "encoding has-event member id list")?;
    // Keyed by fact id so the retraction filter runs over deduped edge facts.
    let mut candidates: BTreeMap<i64, i64> = BTreeMap::new();
    let rows: Vec<(i64, i64)> = sqlx::query_as(queries::HAS_EVENT_EDGES.sql)
        .bind(&members_json)
        .bind(bound.bind())
        .fetch_all(&mut *conn)
        .await
        .map_err(sql("fetching has-event edges"))?;
    for (event, fid) in rows {
        candidates.insert(fid, event);
    }
    let seeds = seed_ids(candidates.keys(), "has-event edge fact id")?;
    let retraction = retraction_edges(conn, bound, &seeds).await?;
    let mut owned = BTreeSet::new();
    for ((_, event), fid) in candidates.iter().zip(&seeds) {
        if effective_retractor(*fid, bound.fact_id(), &retraction).is_some() {
            continue;
        }
        owned.insert(SqlEventId::from_raw(*event));
    }
    Ok(owned)
}

/// The class's live bookend facts under one query (construction starts or
/// demolition completions), fetched whole over the member set and
/// retraction-filtered. Ascending by fact id.
async fn bookend_facts(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    members: &BTreeSet<SqlEntityId>,
    query: &'static str,
) -> Result<Vec<(FactId, UncertainDate)>, SqliteFactStoreError> {
    let members_json = subject_id_json(members, "encoding bookend member id list")?;
    let mut candidates: BTreeMap<i64, UncertainDate> = BTreeMap::new();
    let rows: Vec<(sqlx::types::Json<UncertainDate>, i64)> = sqlx::query_as(query)
        .bind(&members_json)
        .bind(bound.bind())
        .fetch_all(&mut *conn)
        .await
        .map_err(sql("fetching bookend facts"))?;
    for (date, fid) in rows {
        candidates.insert(fid, date.0);
    }
    let seeds = seed_ids(candidates.keys(), "bookend fact id")?;
    let retraction = retraction_edges(conn, bound, &seeds).await?;
    let mut facts = Vec::new();
    for ((_, date), fid) in candidates.iter().zip(&seeds) {
        if effective_retractor(*fid, bound.fact_id(), &retraction).is_some() {
            continue;
        }
        facts.push((*fid, date.clone()));
    }
    Ok(facts)
}

/// The value-range witness violators against one bound, grouped as the
/// enumeration bundles them: existence facts by their exact date, an event's
/// date facts by event in projection slot order (`(role, fact_id)`). One scan
/// over the whole member set gathers the existence witnesses and one over the
/// owned-event set the event witnesses; one retraction closure gates the batch.
async fn witness_groups(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    members: &BTreeSet<SqlEntityId>,
    owned: &BTreeSet<SqlEventId>,
    threshold: Threshold,
) -> Result<Vec<Vec<(FactId, UncertainDate)>>, SqliteFactStoreError> {
    let (existence_sql, event_sql, day) = match threshold {
        Threshold::Below(day) => (
            queries::EXISTENCE_WITNESS_BELOW.sql,
            queries::EVENT_WITNESS_BELOW.sql,
            day,
        ),
        Threshold::Above(day) => (
            queries::EXISTENCE_WITNESS_ABOVE.sql,
            queries::EVENT_WITNESS_ABOVE.sql,
            day,
        ),
    };

    // Existence candidates: fact id → stored date.
    let members_json = subject_id_json(members, "encoding existence witness member id list")?;
    let mut existence: BTreeMap<i64, UncertainDate> = BTreeMap::new();
    let existence_rows: Vec<(sqlx::types::Json<UncertainDate>, i64)> =
        sqlx::query_as(existence_sql)
            .bind(&members_json)
            .bind(day)
            .bind(bound.bind())
            .fetch_all(&mut *conn)
            .await
            .map_err(sql("fetching existence witnesses"))?;
    for (date, fid) in existence_rows {
        existence.insert(fid, date.0);
    }
    // Event candidates: fact id → (event, slot-order role, stored date). The
    // scan carries `event` back so the batched rows regroup by event.
    let owned_json = subject_id_json(owned, "encoding event witness event id list")?;
    let mut events: BTreeMap<i64, (i64, i64, UncertainDate)> = BTreeMap::new();
    let event_rows: Vec<(sqlx::types::Json<UncertainDate>, i64, i64, i64)> =
        sqlx::query_as(event_sql)
            .bind(&owned_json)
            .bind(day)
            .bind(bound.bind())
            .fetch_all(&mut *conn)
            .await
            .map_err(sql("fetching event witnesses"))?;
    for (date, fid, role, event) in event_rows {
        events.insert(fid, (event, role, date.0));
    }

    // One retraction closure over every candidate fact id.
    let mut seed_ints: Vec<i64> = existence.keys().copied().collect();
    seed_ints.extend(events.keys().copied());
    let seeds = seed_ids(seed_ints.iter(), "witness fact id")?;
    let retraction = retraction_edges(conn, bound, &seeds).await?;
    let live = |fid: FactId| effective_retractor(fid, bound.fact_id(), &retraction).is_none();

    // Existence groups: bundle by exact date, matching the projection's
    // FactMap<UncertainDate> keying.
    let mut existence_groups: BTreeMap<UncertainDate, Vec<FactId>> = BTreeMap::new();
    for (fid_raw, date) in &existence {
        let fid = FactId::new(i64_to_u64(*fid_raw, "existence witness fact id")?);
        if !live(fid) {
            continue;
        }
        existence_groups.entry(date.clone()).or_default().push(fid);
    }
    // Event groups: bundle by event, ordered into the projection's slot order so
    // a tie at the bound picks the same witness the whole-entity oracle does.
    let mut event_groups: BTreeMap<i64, Vec<(i64, FactId, UncertainDate)>> = BTreeMap::new();
    for (fid_raw, (event, role, date)) in &events {
        let fid = FactId::new(i64_to_u64(*fid_raw, "event witness fact id")?);
        if !live(fid) {
            continue;
        }
        event_groups
            .entry(*event)
            .or_default()
            .push((*role, fid, date.clone()));
    }

    let mut groups: Vec<Vec<(FactId, UncertainDate)>> = Vec::new();
    for (date, fids) in existence_groups {
        groups.push(fids.into_iter().map(|fid| (fid, date.clone())).collect());
    }
    for (_event, mut rows) in event_groups {
        rows.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        groups.push(rows.into_iter().map(|(_, fid, date)| (fid, date)).collect());
    }
    Ok(groups)
}

/// The temporal-conflict composed read at the bound — the witness-index
/// counterpart of projecting the whole entity. Mirrors `project_entity`'s hops:
/// class members via `subject_reps`, owned events via the per-edge `HasEvent`
/// liveness hop. Reads the class's live bookend facts (joined core-side into the
/// floor / ceiling), then the two value-range scans for the witnesses that cross
/// them — so an in-bounds entity seeks to nothing and yields no conflict without
/// a projection. The result feeds
/// [`conflicts_via_index`](chronoscope_core::solvers::conflicts_via_index).
pub(super) async fn temporal_conflict_scan(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    fq: &FactQueries,
    entity: SqlEntityId,
) -> Result<WitnessScan, SqliteFactStoreError> {
    let members = equiv_class(conn, bound, fq, entity).await?.members;
    let owned = owned_events(conn, bound, &members).await?;

    let construction_starts =
        bookend_facts(conn, bound, &members, queries::CONSTRUCTION_STARTS.sql).await?;
    let demolition_completions =
        bookend_facts(conn, bound, &members, queries::DEMOLITION_COMPLETIONS.sql).await?;

    let floor_day = witness_join(&construction_starts)
        .earliest()
        .map(day_number);
    let ceiling_day = witness_join(&demolition_completions)
        .latest()
        .map(day_number);

    let below_floor = match floor_day {
        Some(day) => witness_groups(conn, bound, &members, &owned, Threshold::Below(day)).await?,
        None => Vec::new(),
    };
    let above_ceiling = match ceiling_day {
        Some(day) => witness_groups(conn, bound, &members, &owned, Threshold::Above(day)).await?,
        None => Vec::new(),
    };

    Ok(WitnessScan {
        construction_starts,
        demolition_completions,
        below_floor,
        above_ceiling,
    })
}
