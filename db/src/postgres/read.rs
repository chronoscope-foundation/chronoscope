//! Snapshot-scoped read implementations over one Postgres connection.
//!
//! Both read paths come through here: a [`PostgresFactView`](super::PostgresFactView)
//! passes its owned read-transaction connection and sees committed rows, a
//! [`PostgresTx`](super::PostgresTx) passes the write transaction's and sees
//! committed union staged. Consistency comes from the `WHERE fact_id < N` bound,
//! not the transaction isolation: the counters-row lock held through commit
//! gives commit-order == id-order, so every fact with id < N is already
//! committed at view time.
//!
//! Retraction filtering is two-phase everywhere: one batched
//! [`RETRACTOR_CLOSURE`](super::queries::RETRACTOR_CLOSURE) fetch for the rows
//! in hand, then the shared fixpoint
//! ([`chronoscope_core::store::retraction`]) in Rust.
//!
//! Representatives and classes resolve through the `subject_reps` log, kept
//! equal to the live-edge components at every snapshot by the write path
//! ([`super::maintain`]), so no read walks identity edges.

use std::collections::{BTreeMap, BTreeSet};

use sqlx::PgConnection;

use chronoscope_core::geo::{QuadLevel, QuadTileRange, TileId, Viewport};
use chronoscope_core::grammar::event;
use chronoscope_core::grammar::ids::FactId;
use chronoscope_core::store::FactPlacement;
use chronoscope_core::store::pagination;
use chronoscope_core::store::retraction::{RetractionEdges, effective_retractor};
use chronoscope_core::store::schema::{
    CELL_DEPTH, ClassPage, ClassRow, ClusterCell, DepictionPage, EquivClass, FactPage, PageItem,
    RankKey, cluster_tile_ranges,
};
use chronoscope_core::submit::{FactLookup, LocatedSubject, StoredFact, SubmitResult};

use super::error::{PostgresFactStoreError as Error, sql};
use super::queries;
use crate::common::cluster::{
    LocatedSubjects, OwnerCandidates, OwnerRow, place_candidates, resolve_entities,
};
use crate::common::convert::{i64_to_u64, seed_ids, u64_to_i64};
use crate::common::ids::{SqlEntityId, SqlEventId, SqlIds, SqlImageId};
use crate::common::spatial::{dedup_by_fact, refine_in_viewport};
use crate::common::storage::{
    SubjectColumn, depiction_subjects, fact_from_json, kind_tag, result_from_json,
};

/// A read's visibility: a pool view's pinned exclusive upper bound, or the
/// whole of what the connection sees (a transaction's committed union staged).
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

    /// SQL bind form. Stored fact ids all fit `i64`, so the union view (and any
    /// larger pin) admits the same rows as the maximum.
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

/// One past the highest minted fact id; 0 on an empty store.
pub(super) async fn next_fact_id(conn: &mut PgConnection) -> Result<u64, Error> {
    let (next,): (i64,) = sqlx::query_as(queries::NEXT_FACT_ID)
        .fetch_one(&mut *conn)
        .await
        .map_err(sql("reading next fact id"))?;
    Ok(i64_to_u64(next, "next fact id")?)
}

/// The retractor-closure edges for `seeds`, fetched once per batch of rows under
/// consideration and resolved in memory by the shared fixpoint
/// ([`effective_retractor`]). The seed ids bind directly as a `bigint[]` (the
/// SQLite backend drives a `json_each` string instead).
pub(super) async fn retraction_edges(
    conn: &mut PgConnection,
    bound: ReadBound,
    seeds: &[FactId],
) -> Result<RetractionEdges, Error> {
    if seeds.is_empty() {
        return Ok(RetractionEdges::from_edges(std::iter::empty()));
    }
    let seed_ids: Vec<i64> = seeds
        .iter()
        .map(|fid| u64_to_i64(fid.get(), "retraction seed fact id"))
        .collect::<Result<_, _>>()?;
    let rows: Vec<(i64, i64)> = sqlx::query_as(queries::RETRACTOR_CLOSURE)
        .bind(seed_ids)
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
        .collect::<Result<_, Error>>()?;
    Ok(RetractionEdges::from_edges(edges))
}

/// Look up a fact under the bound, preserving the four outcomes. Fact rows are
/// dense, so on the union view a missing row means the id is past the staged
/// maximum — `Future`; a pinned view classifies missing rows below its bound as
/// `Unknown`.
pub(super) async fn fact_lookup(
    conn: &mut PgConnection,
    bound: ReadBound,
    fact_id: FactId,
) -> Result<FactLookup<SqlIds>, Error> {
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
    let row: Option<(String,)> = sqlx::query_as(queries::FACT_ROW)
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
    conn: &mut PgConnection,
    id: &chronoscope_core::grammar::ids::CommitId,
) -> Result<bool, Error> {
    let row: Option<(i32,)> = sqlx::query_as(queries::COMMIT_KNOWN)
        .bind(id.as_str())
        .fetch_optional(&mut *conn)
        .await
        .map_err(sql("checking commit existence"))?;
    Ok(row.is_some())
}

/// The surrogate `commit_seq` recorded under a commit hash; `None` while the
/// hash is unrecorded.
pub(super) async fn commit_seq(
    conn: &mut PgConnection,
    commit_id: &str,
) -> Result<Option<i64>, Error> {
    let row: Option<(i64,)> = sqlx::query_as(queries::COMMIT_SEQ)
        .bind(commit_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(sql("resolving commit seq"))?;
    Ok(row.map(|(seq,)| seq))
}

/// The cached submit result under a commit id.
pub(super) async fn cached_result(
    conn: &mut PgConnection,
    id: &chronoscope_core::grammar::ids::CommitId,
) -> Result<Option<SubmitResult<SqlIds>>, Error> {
    let row: Option<(String,)> = sqlx::query_as(queries::CACHED_RESULT)
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
    conn: &mut PgConnection,
    bound: ReadBound,
    id: FactId,
) -> Result<FactPlacement, Error> {
    if let Some(snapshot) = bound.snapshot()
        && id.get() >= snapshot.get()
    {
        return Ok(FactPlacement::Absent);
    }
    let Ok(fid) = u64_to_i64(id.get(), "placement fact id") else {
        return Ok(FactPlacement::Absent);
    };
    let row: Option<(bool,)> = sqlx::query_as(queries::PLACEMENT_ROW)
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
/// strictly below it, one descending covering seek; no row means the member has
/// always been its own representative.
pub(super) async fn resolve_rep_raw(
    conn: &mut PgConnection,
    bound: ReadBound,
    kind: &str,
    member: i64,
) -> Result<i64, Error> {
    let row: Option<(i64,)> = sqlx::query_as(queries::RESOLVE_REP)
        .bind(kind)
        .bind(member)
        .bind(bound.bind())
        .fetch_optional(&mut *conn)
        .await
        .map_err(sql("resolving subject representative"))?;
    Ok(row.map_or(member, |(rep,)| rep))
}

/// The raw members whose latest log row at the bound names `rep` as their
/// representative. The representative itself appears only when it carries a self
/// row (after a split); callers add it.
pub(super) async fn class_members_raw(
    conn: &mut PgConnection,
    bound: ReadBound,
    kind: &str,
    rep: i64,
) -> Result<Vec<i64>, Error> {
    let rows: Vec<(i64,)> = sqlx::query_as(queries::CLASS_MEMBERS)
        .bind(kind)
        .bind(rep)
        .bind(bound.bind())
        .fetch_all(&mut *conn)
        .await
        .map_err(sql("gathering class members"))?;
    Ok(rows.into_iter().map(|(member,)| member).collect())
}

/// [`resolve_rep_raw`] behind a per-walk cache: each distinct member costs one
/// log seek, repeats are free.
async fn resolve_rep_cached(
    conn: &mut PgConnection,
    bound: ReadBound,
    kind: &str,
    member: i64,
    cache: &mut std::collections::HashMap<i64, i64>,
) -> Result<i64, Error> {
    if let Some(rep) = cache.get(&member) {
        return Ok(*rep);
    }
    let rep = resolve_rep_raw(conn, bound, kind, member).await?;
    cache.insert(member, rep);
    Ok(rep)
}

/// The class representative of `member` at the snapshot — one log seek.
pub(super) async fn representative<S: SubjectColumn>(
    conn: &mut PgConnection,
    bound: ReadBound,
    member: S,
) -> Result<S, Error> {
    let rep = resolve_rep_raw(conn, bound, kind_tag(S::KIND), member.raw()).await?;
    Ok(S::from_raw(rep))
}

/// The class representatives of a batch of members at the snapshot, resolved in
/// one query: each member's last log row below the bound, or the member itself
/// where it has none. Every input member appears in the returned map.
pub(super) async fn representatives<S: SubjectColumn + std::hash::Hash>(
    conn: &mut PgConnection,
    bound: ReadBound,
    members: &[S],
) -> Result<std::collections::HashMap<S, S>, Error> {
    if members.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let raw_ids: Vec<i64> = members.iter().map(|m| m.raw()).collect();
    let rows: Vec<(i64, i64)> = sqlx::query_as(queries::RESOLVE_REPS)
        .bind(kind_tag(S::KIND))
        .bind(raw_ids)
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
/// representative log: one seek to the representative, then the reverse gather,
/// plus the representative itself.
pub(super) async fn equiv_class<S: SubjectColumn>(
    conn: &mut PgConnection,
    bound: ReadBound,
    member: S,
) -> Result<EquivClass<S>, Error> {
    let rep = resolve_rep_raw(conn, bound, kind_tag(S::KIND), member.raw()).await?;
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

/// One page of the facts mentioning `subject`, active at the snapshot, resuming
/// strictly past `after`. Candidates come off the subject index in id order; a
/// retracted candidate is dropped after the batched closure resolves, so a page
/// can come back short yet still carry a resume cursor.
pub(super) async fn backlink_page<S: SubjectColumn>(
    conn: &mut PgConnection,
    bound: ReadBound,
    subject: S,
    after: Option<FactId>,
    limit: std::num::NonZeroUsize,
) -> Result<FactPage<StoredFact<SqlIds>, S, FactId>, Error> {
    // -1 sits below every stored id, so it opens the strictly-past scan.
    let cursor_bind = match after {
        None => -1,
        Some(cursor) => u64_to_i64(cursor.get(), "backlink resume cursor")?,
    };
    // Fetch one past the page to learn whether candidates remain.
    let fetch = i64::try_from(limit.get())
        .unwrap_or(i64::MAX)
        .saturating_add(1);
    let mut rows: Vec<(i64, String)> = sqlx::query_as(queries::BACKLINK_PAGE)
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
/// strictly past `after`. The retraction-inclusive twin of [`backlink_page`]:
/// it drops the retraction filter and keeps only `HasEvent` facts.
pub(super) async fn has_event_backlink_page(
    conn: &mut PgConnection,
    bound: ReadBound,
    event: SqlEventId,
    after: Option<FactId>,
    limit: std::num::NonZeroUsize,
) -> Result<FactPage<StoredFact<SqlIds>, SqlEventId, FactId>, Error> {
    let cursor_bind = match after {
        None => -1,
        Some(cursor) => u64_to_i64(cursor.get(), "has-event backlink resume cursor")?,
    };
    let fetch = i64::try_from(limit.get())
        .unwrap_or(i64::MAX)
        .saturating_add(1);
    let mut rows: Vec<(i64, String)> = sqlx::query_as(queries::BACKLINK_PAGE)
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

/// The facet key a keyed class walk fetches candidates under. The key values
/// arrive pre-encoded by the same functions the write path uses, so the walk and
/// the stored facets cannot drift.
pub(super) enum FacetKey<'a> {
    Name { norm: String, language: &'a str },
    ExternalRef(String),
    SourceUrl(&'a str),
}

impl FacetKey<'_> {
    /// Fetch this key's `(fact_id, fact_json)` candidates below the bound.
    async fn candidates(
        &self,
        conn: &mut PgConnection,
        bound: ReadBound,
    ) -> Result<Vec<(i64, String)>, Error> {
        match self {
            FacetKey::Name { norm, language } => sqlx::query_as(queries::CLASS_CANDIDATES_BY_NAME)
                .bind(norm)
                .bind(language)
                .bind(bound.bind())
                .fetch_all(&mut *conn)
                .await
                .map_err(sql("fetching name-keyed class candidates")),
            FacetKey::ExternalRef(key) => sqlx::query_as(queries::CLASS_CANDIDATES_BY_EXTREF)
                .bind(key)
                .bind(bound.bind())
                .fetch_all(&mut *conn)
                .await
                .map_err(sql("fetching reference-keyed class candidates")),
            FacetKey::SourceUrl(url) => sqlx::query_as(queries::CLASS_CANDIDATES_BY_SRCURL)
                .bind(url)
                .bind(bound.bind())
                .fetch_all(&mut *conn)
                .await
                .map_err(sql("fetching url-keyed class candidates")),
        }
    }
}

/// The tail every flat class walk shares: resolve each `(subject, fact_id)` pair
/// to its class representative — one cached log seek per distinct subject — and
/// cut the `(representative, fact_id)` rows with the backend-shared cursor
/// semantics. The set fold also dedups pairs that arrive twice.
async fn rep_class_page<S: SubjectColumn>(
    conn: &mut PgConnection,
    bound: ReadBound,
    subjects: Vec<(S, FactId)>,
    after: Option<(S, FactId)>,
    limit: std::num::NonZeroUsize,
) -> Result<ClassPage<S, (S, FactId)>, Error> {
    let mut reps: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
    let mut rows: std::collections::BTreeSet<(S, FactId)> = std::collections::BTreeSet::new();
    for (subject, fid) in subjects {
        let rep =
            resolve_rep_cached(conn, bound, kind_tag(S::KIND), subject.raw(), &mut reps).await?;
        rows.insert((S::from_raw(rep), fid));
    }
    Ok(pagination::class_page(&rows, after, limit))
}

/// One page of a keyed class walk (`ByName` / `ByExternalReference` /
/// `BySourceUrl`). The candidate set is key-sized, so the walk fetches it whole:
/// candidates off the facet index, one batched retractor closure, `subject_of`
/// on each survivor's decoded fact, then the shared [`rep_class_page`] tail.
pub(super) async fn keyed_class_page<S: SubjectColumn>(
    conn: &mut PgConnection,
    bound: ReadBound,
    key: FacetKey<'_>,
    subject_of: impl Fn(&StoredFact<SqlIds>) -> Option<S>,
    after: Option<(S, FactId)>,
    limit: std::num::NonZeroUsize,
) -> Result<ClassPage<S, (S, FactId)>, Error> {
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
    rep_class_page(conn, bound, subjects, after, limit).await
}

/// One page of the depiction walk: the depiction facts of `entity`'s
/// `SameEntity` class, each row keeping its whole fact under the depicted image's
/// `SameArtifact` representative, ordered `(image_rep, fact_id)`.
pub(super) async fn depiction_page(
    conn: &mut PgConnection,
    bound: ReadBound,
    entity: SqlEntityId,
    after: Option<(SqlImageId, FactId)>,
    limit: std::num::NonZeroUsize,
) -> Result<DepictionPage<StoredFact<SqlIds>, SqlImageId, (SqlImageId, FactId)>, Error> {
    let members = equiv_class(conn, bound, entity).await?.members;
    // A depiction names one entity, so each candidate appears under exactly one
    // member; the map keys by fact id regardless.
    let mut candidates: std::collections::BTreeMap<i64, String> = std::collections::BTreeMap::new();
    for member in &members {
        let rows: Vec<(i64, String)> = sqlx::query_as(queries::SUBJECT_FACTS)
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

/// The owning entity of each of `events` at the snapshot. One batched
/// [`EVENT_OWNERS`](super::queries::EVENT_OWNERS) fetch (an indexed probe per
/// event) reads owners off the `event_owner` facet without decoding a fact, then
/// one batched retractor closure feeds the shared live-owner rule
/// ([`OwnerCandidates`]).
async fn event_owners(
    conn: &mut PgConnection,
    bound: ReadBound,
    events: &BTreeSet<SqlEventId>,
) -> Result<BTreeMap<SqlEventId, SqlEntityId>, Error> {
    if events.is_empty() {
        return Ok(BTreeMap::new());
    }
    let event_ids: Vec<i64> = events.iter().map(|event| event.raw()).collect();
    // `SELECT s.fact_id, s.subject_id, f.event_owner` — named into `OwnerRow` right
    // here, since the seam has no other tie to this statement's column order.
    let rows: Vec<(i64, i64, i64)> = sqlx::query_as(queries::EVENT_OWNERS)
        .bind(event_ids)
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

/// The active location-bearing facts of the stream's subject kinds whose region
/// can meet `viewport`: one `GiST` `&&` fetch per viewport half (the core-owned
/// seam split, [`halves`]) narrowed to `kinds` in SQL, one batched retractor
/// closure, then the shared [`refine_in_viewport`] membership decision, which
/// the SQLite backend runs on its own fetch, so the two cannot disagree on who
/// is in the box.
///
/// [`halves`]: chronoscope_core::geo::Viewport::halves
async fn located_in_viewport(
    conn: &mut PgConnection,
    bound: ReadBound,
    viewport: &Viewport,
    kinds: (&str, &str),
) -> Result<Vec<(FactId, StoredFact<SqlIds>)>, Error> {
    let mut rows: Vec<(i64, String)> = Vec::new();
    for half in viewport.halves() {
        // ST_MakeEnvelope reads (xmin, ymin, xmax, ymax): longitude on x.
        let fetched: Vec<(i64, String)> = sqlx::query_as(queries::SPATIAL_CANDIDATES)
            .bind(half.min_lon)
            .bind(half.min_lat)
            .bind(half.max_lon)
            .bind(half.max_lat)
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

/// One page of the entity `InViewport` class walk: [`located_in_viewport`] facts
/// of the entity and event kinds, construction bookends attributed to their own
/// entity and `MovedToLocation` facts to their [`event_owners`] entity (an
/// orphaned move attributes to nothing), then the shared [`rep_class_page`]
/// tail, which resolves representatives itself.
pub(super) async fn spatial_entity_page(
    conn: &mut PgConnection,
    bound: ReadBound,
    viewport: &Viewport,
    after: Option<(SqlEntityId, FactId)>,
    limit: std::num::NonZeroUsize,
) -> Result<ClassPage<SqlEntityId, (SqlEntityId, FactId)>, Error> {
    let kinds = (kind_tag(SqlEntityId::KIND), kind_tag(SqlEventId::KIND));
    let located = located_in_viewport(conn, bound, viewport, kinds).await?;
    let split = LocatedSubjects::partition(&located);
    let owners = event_owners(conn, bound, &split.events()).await?;
    rep_class_page(conn, bound, split.attribute(&owners), after, limit).await
}

/// One page of the image `InViewport` class walk: [`located_in_viewport`] facts
/// of the image kind (`CapturedLocation`), then the shared [`rep_class_page`]
/// tail.
pub(super) async fn spatial_image_page(
    conn: &mut PgConnection,
    bound: ReadBound,
    viewport: &Viewport,
    after: Option<(SqlImageId, FactId)>,
    limit: std::num::NonZeroUsize,
) -> Result<ClassPage<SqlImageId, (SqlImageId, FactId)>, Error> {
    let kind = kind_tag(SqlImageId::KIND);
    let located = located_in_viewport(conn, bound, viewport, (kind, kind)).await?;
    let mut subjects: Vec<(SqlImageId, FactId)> = Vec::new();
    for (fid, fact) in &located {
        if let Some((_, LocatedSubject::Image(image))) = fact.located_subject() {
            subjects.push((*image, *fid));
        }
    }
    rep_class_page(conn, bound, subjects, after, limit).await
}

/// Attribute a batch of located facts to their entity representatives through
/// the shared [`resolve_entities`] sequence, driving it with this backend's two
/// batched fetches: [`event_owners`] and [`representatives`].
async fn resolve_located_entities(
    conn: &mut PgConnection,
    bound: ReadBound,
    located: LocatedSubjects,
) -> Result<Vec<(SqlEntityId, FactId)>, Error> {
    resolve_entities(
        located,
        conn,
        async |conn: &mut PgConnection, events: &BTreeSet<SqlEventId>| {
            event_owners(conn, bound, events).await
        },
        async |conn: &mut PgConnection, members: &[SqlEntityId]| {
            representatives(conn, bound, members).await
        },
    )
    .await
}

/// Which range a cluster candidate folds under: its position in the bound arrays,
/// as `WITH ORDINALITY` numbers it. Distinct from the quadkey and fact id it
/// travels beside, all three of which are `bigint` on the wire — a transposition
/// would merge two cells into one or explode one into many, so it costs a type
/// rather than a runtime check.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct Bucket(i64);

/// The bounded core under both clustering reads: fetch each of `ranges` for its
/// `(quadkey, fact_id)`-lowest
/// [`CLUSTER_TILE_N`](chronoscope_core::store::schema::CLUSTER_TILE_N)
/// entity/event candidates, then fold each range to at most one [`ClusterCell`].
/// The range's position in the bound arrays is the fold's [`Bucket`], so ranges
/// never bleed together.
///
/// The whole fan-out is one statement
/// ([`CLUSTER_TILE`](super::queries::CLUSTER_TILE)): the ranges bind as parallel
/// arrays and a LATERAL applies the top-N scan per range, so 64 sub-tiles or 256
/// viewport tiles cost one round trip while each range keeps its own budget. The
/// `WITH ORDINALITY` bucket rides back on every row, since the fold is
/// per-bucket and order-independent — a lost bucket would silently merge two
/// cells into one rather than fail. Binding arrays means `ranges` is collected
/// here, which the caps keep small.
///
/// The batch then goes through the shared tail — one retractor closure into
/// [`place_candidates`], [`resolve_located_entities`], and the placement's fold
/// — which the SQLite backend runs on its own fetch, so the two answer identical
/// cells. `cluster_entities_in_viewport` passes the viewport's tiles and
/// `cluster_tile_cells` the container's child tiles, so a stand-alone tile and
/// the same tile inside a viewport fold to byte-identical cells.
async fn cluster_ranges(
    conn: &mut PgConnection,
    bound: ReadBound,
    ranges: impl IntoIterator<Item = QuadTileRange>,
) -> Result<Vec<ClusterCell<SqlEntityId>>, Error> {
    let (los, his): (Vec<i64>, Vec<i64>) =
        ranges.into_iter().map(|range| (range.lo, range.hi)).unzip();
    if los.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<(i64, i64, String, i64)> = sqlx::query_as(queries::CLUSTER_TILE.as_str())
        .bind(los)
        .bind(his)
        .bind(bound.bind())
        .fetch_all(&mut *conn)
        .await
        .map_err(sql("fetching cluster tile candidates"))?;

    let mut candidates: Vec<(Bucket, FactId, i64, StoredFact<SqlIds>)> =
        Vec::with_capacity(rows.len());
    for (bucket, fid, fact_json, quadkey) in rows {
        candidates.push((
            Bucket(bucket),
            FactId::new(i64_to_u64(fid, "cluster candidate fact id")?),
            quadkey,
            fact_from_json(&fact_json)?,
        ));
    }

    let seeds: Vec<FactId> = candidates.iter().map(|(_, fid, _, _)| *fid).collect();
    let retraction = retraction_edges(conn, bound, &seeds).await?;
    let placed = place_candidates(candidates, bound.fact_id(), &retraction);
    let resolved = resolve_located_entities(conn, bound, placed.subjects()).await?;
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
    conn: &mut PgConnection,
    bound: ReadBound,
    viewport: &Viewport,
    level: QuadLevel,
    rank: RankKey,
) -> Result<Vec<ClusterCell<SqlEntityId>>, Error> {
    // The sole rank; its `(quadkey, fact_id)` order is the fetch order and the
    // per-tile min in the fold.
    match rank {
        RankKey::Unranked => {}
    }
    let tiles = cluster_tile_ranges(viewport, level)?;
    cluster_ranges(conn, bound, tiles).await
}

/// One [`ClusterCell`] per non-empty sub-tile of container `tile` at
/// `level + CELL_DEPTH` — the viewport-free per-tile fold whose cell geometry is
/// keyed by `(snapshot, level, x, y)`.
///
/// [`TileId::child_ranges`] enumerates the container's children `CELL_DEPTH`
/// levels finer — at most `4^CELL_DEPTH` ranges, since the depth folds against
/// the finest level. The children partition the container's Morton block, so each
/// is one sub-tile bucket; folding each through the shared [`cluster_ranges`]
/// applies the **per-sub-tile**
/// top-[`CLUSTER_TILE_N`](chronoscope_core::store::schema::CLUSTER_TILE_N) cut,
/// keeping a dense low-Morton corner from starving the container's other
/// sub-tiles. That shared tail matches the viewport read, so a stand-alone tile
/// and the same tile inside a viewport fold to byte-identical cells.
pub(super) async fn cluster_tile_cells(
    conn: &mut PgConnection,
    bound: ReadBound,
    tile: TileId,
    rank: RankKey,
) -> Result<Vec<ClusterCell<SqlEntityId>>, Error> {
    // The sole rank; its `(quadkey, fact_id)` order is the fetch order and the
    // per-sub-tile min in the fold.
    match rank {
        RankKey::Unranked => {}
    }
    cluster_ranges(conn, bound, tile.child_ranges(CELL_DEPTH)).await
}

/// The `(rep, fact_id)` SQL binds for an All-walk cursor. `None` opens the walk
/// (below every real row). The class cursor's `u64::MAX` sentinel clamps to
/// `i64::MAX`, which still sorts past every real row of its representative.
fn all_cursor_binds<S: SubjectColumn>(after: Option<(S, FactId)>) -> (i64, i64) {
    match after {
        None => (i64::MIN, i64::MAX),
        Some((rep, fid)) => (rep.raw(), i64::try_from(fid.get()).unwrap_or(i64::MAX)),
    }
}

/// One page of the All-stream class walk: the single-statement walk fetches one
/// row past the page to learn whether candidates remain, then the batched
/// retractor closure drops retracted candidates in Rust. `next` resumes past the
/// last candidate consumed; `next_class` follows the last emitted row's
/// representative.
pub(super) async fn all_class_page<S: SubjectColumn>(
    conn: &mut PgConnection,
    bound: ReadBound,
    after: Option<(S, FactId)>,
    limit: std::num::NonZeroUsize,
) -> Result<ClassPage<S, (S, FactId)>, Error> {
    let (after_rep, after_fid) = all_cursor_binds(after);
    let fetch = i64::try_from(limit.get())
        .unwrap_or(i64::MAX)
        .saturating_add(1);
    let mut candidates: Vec<(i64, i64)> = sqlx::query_as(queries::CLASS_WALK_ALL)
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
        // representative to skip past, so the class cursor continues at the row
        // cursor — strictly forward, never truncating a live class beyond the
        // retracted stretch.
        (None, Some(_)) => next,
        (Some(last), Some((ahead_rep, _))) => {
            let last_raw = last.representative.raw();
            let beyond = if *ahead_rep > last_raw {
                true
            } else {
                // The probe counts candidates without retraction filtering, so a
                // trailing fully-retracted class still answers Some — the
                // consumer's next fetch comes back empty with an exhausted
                // cursor, one extra round trip and correct termination.
                let probe: Option<(i64, i64)> = sqlx::query_as(queries::CLASS_WALK_ALL)
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
