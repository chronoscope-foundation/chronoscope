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

use sqlx::SqliteConnection;

use chronoscope_core::grammar::ids::FactId;
use chronoscope_core::store::FactPlacement;
use chronoscope_core::store::pagination;
use chronoscope_core::store::retraction::{RetractionEdges, effective_retractor};
use chronoscope_core::store::schema::{
    ClassPage, ClassRow, DepictionPage, EquivClass, FactPage, PageItem,
};
use chronoscope_core::submit::{FactLookup, StoredFact, SubmitResult};

use super::convert::{i64_to_u64, seed_ids, u64_to_i64};
use super::error::{SqliteFactStoreError, json, sql};
use super::ids::{SqliteEntityId, SqliteIds, SqliteImageId};
use super::queries;
use super::storage::{
    SubjectColumn, depiction_subjects, fact_from_json, kind_tag, result_from_json,
};

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

/// The representative of a raw subject id at the bound: the log's last row
/// strictly below it, one descending covering seek; no row means the member
/// has always been its own representative.
pub(super) async fn resolve_rep_raw(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    kind: &str,
    member: i64,
) -> Result<i64, SqliteFactStoreError> {
    let row: Option<(i64,)> = sqlx::query_as(queries::RESOLVE_REP.sql)
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

/// The class representative of `member` at the snapshot — one log seek.
pub(super) async fn representative<S: SubjectColumn>(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    member: S,
) -> Result<S, SqliteFactStoreError> {
    let rep = resolve_rep_raw(conn, bound, kind_tag(S::KIND), member.raw()).await?;
    Ok(S::from_raw(rep))
}

/// The equivalence class of `member` at the snapshot, read off the
/// representative log: one seek to the representative, then the reverse
/// gather, plus the representative itself.
pub(super) async fn equiv_class<S: SubjectColumn>(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    member: S,
) -> Result<EquivClass<S>, SqliteFactStoreError> {
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

/// The facet key a keyed class walk fetches candidates under. Each variant
/// carries exactly the columns of its partial index; the key values arrive
/// pre-encoded by the same functions the write path uses ([`normalize_name`]
/// for names, [`external_ref_key`](super::storage::external_ref_key) for
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

/// One page of a keyed class walk (`ByName` / `ByExternalReference` /
/// `BySourceUrl`). The candidate set is key-sized, so the walk fetches it
/// whole: candidates off the facet index, one batched retractor closure,
/// `subject_of` on each survivor's decoded fact, one log seek per distinct
/// subject, and the `(representative, fact_id)` rows page in Rust with the
/// memory backend's exact cursor semantics ([`page_rows`]).
pub(super) async fn keyed_class_page<S: SubjectColumn>(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    key: FacetKey<'_>,
    subject_of: impl Fn(&StoredFact<SqliteIds>) -> Option<S>,
    after: Option<(S, FactId)>,
    limit: std::num::NonZeroUsize,
) -> Result<ClassPage<S, (S, FactId)>, SqliteFactStoreError> {
    let candidates = key.candidates(conn, bound).await?;
    let seeds = seed_ids(
        candidates.iter().map(|(fid, _)| fid),
        "class candidate fact id",
    )?;
    let retraction = retraction_edges(conn, bound, &seeds).await?;

    let mut reps: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
    let mut rows: std::collections::BTreeSet<(S, FactId)> = std::collections::BTreeSet::new();
    for ((_, fact_json), fid) in candidates.iter().zip(&seeds) {
        if effective_retractor(*fid, bound.fact_id(), &retraction).is_some() {
            continue;
        }
        let Some(subject) = subject_of(&fact_from_json(fact_json)?) else {
            continue;
        };
        let rep = match reps.get(&subject.raw()) {
            Some(rep) => *rep,
            None => {
                let rep = resolve_rep_raw(conn, bound, kind_tag(S::KIND), subject.raw()).await?;
                reps.insert(subject.raw(), rep);
                rep
            }
        };
        rows.insert((S::from_raw(rep), *fid));
    }
    Ok(pagination::class_page(&rows, after, limit))
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
    entity: SqliteEntityId,
    after: Option<(SqliteImageId, FactId)>,
    limit: std::num::NonZeroUsize,
) -> Result<
    DepictionPage<StoredFact<SqliteIds>, SqliteImageId, (SqliteImageId, FactId)>,
    SqliteFactStoreError,
> {
    let members = equiv_class(conn, bound, entity).await?.members;
    // A depiction names one entity, so each candidate appears under exactly
    // one member; the map keys by fact id regardless.
    let mut candidates: std::collections::BTreeMap<i64, String> = std::collections::BTreeMap::new();
    for member in &members {
        let rows: Vec<(i64, String)> = sqlx::query_as(queries::SUBJECT_FACTS.sql)
            .bind(kind_tag(SqliteEntityId::KIND))
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
    let mut rows: std::collections::BTreeSet<(SqliteImageId, FactId)> =
        std::collections::BTreeSet::new();
    let mut facts: std::collections::BTreeMap<FactId, StoredFact<SqliteIds>> =
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
        let rep = match reps.get(&image.raw()) {
            Some(rep) => *rep,
            None => {
                let rep = resolve_rep_raw(conn, bound, kind_tag(SqliteImageId::KIND), image.raw())
                    .await?;
                reps.insert(image.raw(), rep);
                rep
            }
        };
        rows.insert((SqliteImageId::from_raw(rep), *fid));
        facts.insert(*fid, fact);
    }

    let (pairs, next_class) = pagination::grouped_class_page(&rows, after, limit);
    let mut page: Vec<PageItem<StoredFact<SqliteIds>, SqliteImageId>> = Vec::new();
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
/// ([`CLASS_WALK_ALL`](super::queries::CLASS_WALK_ALL)) fetches one row past
/// the page to learn whether candidates remain, then the batched retractor
/// closure drops retracted candidates in Rust. `next` resumes past the last
/// candidate consumed (active or not), so a page can come back short with a
/// live cursor. `next_class` follows the last emitted row's representative:
/// when the lookahead shares it, a one-row probe past `(rep, MAX)` answers
/// whether a later representative remains; a fully-retracted page carries
/// the row cursor instead, since it emitted no representative to skip.
pub(super) async fn all_class_page<S: SubjectColumn>(
    conn: &mut SqliteConnection,
    bound: ReadBound,
    after: Option<(S, FactId)>,
    limit: std::num::NonZeroUsize,
) -> Result<ClassPage<S, (S, FactId)>, SqliteFactStoreError> {
    let (after_rep, after_fid) = all_cursor_binds(after);
    let fetch = i64::try_from(limit.get())
        .unwrap_or(i64::MAX)
        .saturating_add(1);
    let mut candidates: Vec<(i64, i64)> = sqlx::query_as(queries::CLASS_WALK_ALL.sql)
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
                let probe: Option<(i64, i64)> = sqlx::query_as(queries::CLASS_WALK_ALL.sql)
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
