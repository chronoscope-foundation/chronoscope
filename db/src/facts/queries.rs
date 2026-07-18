//! Fact-store query definitions.
//!
//! Same [`QueryDef`] + startup `EXPLAIN QUERY PLAN` discipline as
//! [`crate::queries`]; kept beside the fact-store code because these queries
//! and the row codecs in [`super::storage`] change together.
//! [`verify_query_plans`] is folded into `Database`'s startup verification.
//!
//! Recursive-CTE conventions: CTE tables are referenced unaliased so a plan's
//! `SCAN <name>` lines match the `MATERIALIZE`/`CO-ROUTINE` declarations the
//! verifier collects, and every CTE step bounds itself with the snapshot
//! predicate plus an indexed join, so a "scan" of a CTE is a bounded
//! subquery scan, never a table scan. The CTE→facts joins are CROSS JOINs —
//! SQLite's join-order pin — because the stats-free planner otherwise puts
//! `facts` outermost on a kind-prefixed edge index and rescans it per CTE
//! row; Postgres rejects these CTEs' doubled recursive references outright,
//! so its port rewrites them and the SQLite-only pin costs no portability.

use sqlx::SqlitePool;

use crate::queries::{QueryDef, QueryPlanError, verify_query_defs};

/// The dense-id expression, spliced into both the clock read and the
/// staging insert so the two can never drift. The subquery sees the
/// pre-statement table, so it mints one id per statement: batching rows
/// into a single INSERT would assign duplicates and trip the primary key.
macro_rules! next_fact_id_expr {
    () => {
        "SELECT COALESCE(MAX(fact_id) + 1, 0) FROM facts"
    };
}

/// The representative-resolution rule — the member's last `subject_reps` row
/// strictly below the exclusive snapshot bound wins — spliced into the point
/// resolve and the All-walk's correlated subquery so the rule cannot drift.
/// `$member` / `$bound` are the SQL expressions for the member and the
/// bound; the kind is always `?1`.
macro_rules! resolve_rep_expr {
    ($member:expr, $bound:expr) => {
        concat!(
            "SELECT r.rep FROM subject_reps r
             WHERE r.kind = ?1 AND r.member = ",
            $member,
            " AND r.as_of < ",
            $bound,
            " ORDER BY r.as_of DESC LIMIT 1"
        )
    };
}

macro_rules! define_fact_queries {
    ($($name:ident: $sql:expr),* $(,)?) => {
        $(pub(super) const $name: QueryDef = QueryDef { name: stringify!($name), sql: $sql };)*

        /// Every fact-store query, for plan verification.
        pub(crate) const ALL: &[&QueryDef] = &[$(&$name),*];
    };
}

define_fact_queries! {
    // Clock: one past the highest stored fact; 0 on an empty store. MAX on
    // the INTEGER PRIMARY KEY is an index seek.
    NEXT_FACT_ID: next_fact_id_expr!(),

    // Mints: bump-and-return against the single counters row. RETURNING
    // evaluates post-update, so `- 1` hands back the id just consumed.
    MINT_ENTITY: "UPDATE fact_counters SET next_entity_id = next_entity_id + 1 WHERE id = 0 RETURNING next_entity_id - 1",
    MINT_EVENT: "UPDATE fact_counters SET next_event_id = next_event_id + 1 WHERE id = 0 RETURNING next_event_id - 1",
    MINT_IMAGE: "UPDATE fact_counters SET next_image_id = next_image_id + 1 WHERE id = 0 RETURNING next_image_id - 1",

    // The known-id predicates read the counters row per check: the
    // transaction's own connection sees its uncommitted bumps, so SQLite is
    // the one source of what this store has minted.
    MINT_COUNTERS: "SELECT next_entity_id, next_event_id, next_image_id FROM fact_counters WHERE id = 0",

    // Staging. SQL assigns the dense fact_id — MAX + 1 over everything this
    // connection sees, so an aborted savepoint's rows vanish and the next id
    // self-corrects. The id space starts at 0 where SQLite's own rowid
    // allocator starts at 1. commit_seq stays NULL until the fact's commit
    // records. The new id comes back as the insert's rowid (fact_id is the
    // rowid alias) — like INSERT_COMMIT's, since RETURNING on a foreign-key
    // parent drags a scan of the child table into the plan.
    INSERT_FACT: concat!(
        "
        INSERT INTO facts (
            fact_id, fact_json,
            name_norm, name_language, external_ref, source_url,
            date_earliest, date_latest, lat, lon, radius_m,
            edge_kind, edge_a, edge_b, event_owner,
            retracts_fact_id, retracts_commit_seq
        ) VALUES ((",
        next_fact_id_expr!(),
        "),
                  ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
    "
    ),
    INSERT_SUBJECT: "INSERT INTO fact_subjects (fact_id, kind, subject_id) VALUES (?1, ?2, ?3)",

    // One covering-rect envelope of a location-bearing fact (see the migration's
    // facts_spatial notes: seam-split halves, INSERT-only). The rect corners
    // become a SpatiaLite MBR polygon in the `region` geometry column, whose
    // managed spatial index SpatiaLite keeps in sync. ?2 is the located
    // subject's kind tag; ?3..?6 are min_lon, min_lat, max_lon, max_lat
    // (`BuildMbr`'s x/y order).
    INSERT_SPATIAL: "INSERT INTO facts_spatial (fact_id, subject_kind, region) VALUES (?1, ?2, BuildMbr(?3, ?4, ?5, ?6, 4326))",

    // Commit recording: the metadata/result row, then a claim per fact
    // under the new surrogate seq. The seq comes back as the insert's rowid
    // (commit_seq is the rowid alias); a RETURNING clause here would drag
    // the foreign-key child check of unindexed `facts.commit_seq` into the
    // plan as a full scan. The claim's `IS NULL` guard updates nothing for
    // an unknown or already-owned row; `record_commit` refuses on the spot.
    INSERT_COMMIT: "INSERT INTO fact_commits (commit_id, commit_json, result_json) VALUES (?1, ?2, ?3)",
    CLAIM_FACT: "UPDATE facts SET commit_seq = ?1 WHERE fact_id = ?2 AND commit_seq IS NULL",

    // The pre-commit audit: any visible row still unclaimed. Committed
    // state never holds one — every prior transaction passed this audit —
    // so a hit is the current transaction's own staging that no recorded
    // commit claimed. Probes the idx_facts_unclaimed partial index, which
    // holds only such rows.
    UNCLAIMED_STAGED_FACT: "SELECT fact_id FROM facts WHERE commit_seq IS NULL LIMIT 1",

    // Point reads. The hash-keyed lookups ride the UNIQUE commit_id index;
    // COMMIT_SEQ resolves a RetractCommit target's hash to the surrogate
    // seq its facet column stores.
    CACHED_RESULT: "SELECT result_json FROM fact_commits WHERE commit_id = ?1",
    COMMIT_KNOWN: "SELECT 1 FROM fact_commits WHERE commit_id = ?1",
    COMMIT_SEQ: "SELECT commit_seq FROM fact_commits WHERE commit_id = ?1",
    FACT_ROW: "SELECT fact_json FROM facts WHERE fact_id = ?1",
    PLACEMENT_ROW: "SELECT commit_seq IS NOT NULL FROM facts WHERE fact_id = ?1",

    // The retractor closure of a seed set: every retraction edge reachable
    // upward from the seeds below the snapshot bound. ?1 is a JSON array of
    // seed fact ids; ?2 the exclusive snapshot. Retractor-of edges are either
    // per-fact (retracts_fact_id) or per-commit (retracts_commit_seq matched
    // against the target's owning commit seq), so each recursion level is
    // two indexed integer probes. Retractor ids strictly exceed their
    // targets', so the recursion climbs and terminates; UNION dedups shared
    // retractors. The effective-retraction fixpoint over the fetched edges
    // runs in Rust (chronoscope_core::store::retraction).
    RETRACTOR_CLOSURE: "
        WITH RECURSIVE
        seed(fact_id, commit_seq) AS (
            SELECT f.fact_id, f.commit_seq
            FROM json_each(?1) JOIN facts f ON f.fact_id = json_each.value
        ),
        edge(target_id, retractor_id, retractor_commit_seq) AS (
            SELECT seed.fact_id, r.fact_id, r.commit_seq
            FROM seed CROSS JOIN facts r ON r.retracts_fact_id = seed.fact_id
            WHERE r.fact_id < ?2
            UNION
            SELECT seed.fact_id, r.fact_id, r.commit_seq
            FROM seed CROSS JOIN facts r ON r.retracts_commit_seq = seed.commit_seq
            WHERE r.fact_id < ?2
            UNION
            SELECT edge.retractor_id, r.fact_id, r.commit_seq
            FROM edge CROSS JOIN facts r ON r.retracts_fact_id = edge.retractor_id
            WHERE r.fact_id < ?2
            UNION
            SELECT edge.retractor_id, r.fact_id, r.commit_seq
            FROM edge CROSS JOIN facts r ON r.retracts_commit_seq = edge.retractor_commit_seq
            WHERE r.fact_id < ?2
        )
        SELECT target_id, retractor_id FROM edge
    ",

    // Representative log. RESOLVE_REP is the one resolution path — the
    // shared resolve_rep_expr! rule (member ?2, bound ?3), a single
    // descending covering seek on the primary key; no row means the member
    // is its own representative (the caller COALESCEs). CLASS_MEMBERS is the
    // reverse gather: every member whose latest log row below ?3 names ?2 as
    // its representative — candidates off idx_subject_reps_rep, each
    // anti-joined against its own later rows by a correlated primary-key
    // probe (the representative itself, rowless when it never moved, is the
    // caller's to add).
    RESOLVE_REP: resolve_rep_expr!("?2", "?3"),

    // Batch representative resolution: the RESOLVE_REP rule applied to every
    // member of a JSON array (?2) in one query — kind ?1, bound ?3. The
    // json_each virtual table drives the outer rows; each member resolves
    // through the same shared one-seek log rule (COALESCE to the member where
    // it has no log row), so the batch and point paths can't disagree. Each
    // member's inner resolve is one descending covering seek on the primary
    // key, exactly as RESOLVE_REP's.
    RESOLVE_REPS: concat!(
        "
        SELECT je.value AS member,
               COALESCE((",
        resolve_rep_expr!("je.value", "?3"),
        "), je.value) AS rep
        FROM json_each(?2) je
    "
    ),
    CLASS_MEMBERS: "
        SELECT s.member FROM subject_reps s
        WHERE s.kind = ?1 AND s.rep = ?2 AND s.as_of < ?3
        AND NOT EXISTS (
            SELECT 1 FROM subject_reps later
            WHERE later.kind = ?1 AND later.member = s.member
              AND later.as_of > s.as_of AND later.as_of < ?3
        )
    ",
    INSERT_REP: "INSERT INTO subject_reps (kind, member, as_of, rep) VALUES (?1, ?2, ?3, ?4)",

    // The identity edges a staged meta-fact ?1 can change the liveness of:
    // its transitive targets, descending through retracts_fact_id (a
    // primary-key probe) and retracts_commit_seq (the target commit's
    // recorded fact ids, via json_each over its commit_json). The downward
    // mirror of RETRACTOR_CLOSURE; targets' ids sit strictly below their
    // retractors', so the descent terminates. Identity facts retract
    // nothing, so they are the leaves the final select keeps.
    IDENTITY_TARGETS: "
        WITH RECURSIVE target(fact_id) AS (
            SELECT ?1
            UNION
            SELECT t.retracts_fact_id
            FROM target CROSS JOIN facts t ON t.fact_id = target.fact_id
            WHERE t.retracts_fact_id IS NOT NULL
            UNION
            SELECT json_each.value
            FROM target CROSS JOIN facts t ON t.fact_id = target.fact_id
            CROSS JOIN fact_commits c ON c.commit_seq = t.retracts_commit_seq
            CROSS JOIN json_each(c.commit_json, '$.fact_ids')
        )
        SELECT f.edge_kind, f.edge_a, f.edge_b
        FROM target CROSS JOIN facts f ON f.fact_id = target.fact_id
        WHERE f.edge_kind IS NOT NULL
    ",

    // The identity-edge facts of ?1's connected component under edge kind
    // ?2, below snapshot ?3, retracted edges included: traversal
    // over-approximates, and the Rust side re-walks from the member over
    // active edges only, so an edge reachable only through a retracted link
    // costs a fetched row, never a wrong class. Filtering the final select
    // on edge_a alone is complete because membership propagates both ways.
    // Write-path machinery only: the read-side class queries resolve through
    // the subject_reps log, and this CTE recomputes components when a staged
    // retraction changes identity-edge liveness.
    EQUIV_COMPONENT: "
        WITH RECURSIVE member(id) AS (
            SELECT ?1
            UNION
            SELECT f.edge_b FROM member CROSS JOIN facts f ON f.edge_kind = ?2 AND f.edge_a = member.id
            WHERE f.fact_id < ?3
            UNION
            SELECT f.edge_a FROM member CROSS JOIN facts f ON f.edge_kind = ?2 AND f.edge_b = member.id
            WHERE f.fact_id < ?3
        )
        SELECT f.fact_id, f.edge_a, f.edge_b
        FROM member CROSS JOIN facts f ON f.edge_kind = ?2 AND f.edge_a = member.id
        WHERE f.fact_id < ?3
    ",

    // One backlink-walk page of candidates: facts mentioning subject
    // (?1 kind, ?2 id), ascending, strictly past cursor ?3 (-1 opens the
    // walk), below snapshot ?4, at most ?5 rows. Retraction filtering
    // happens in Rust over the batched retractor closure.
    BACKLINK_PAGE: "
        SELECT s.fact_id, f.fact_json
        FROM fact_subjects s JOIN facts f ON f.fact_id = s.fact_id
        WHERE s.kind = ?1 AND s.subject_id = ?2 AND s.fact_id > ?3 AND s.fact_id < ?4
        ORDER BY s.fact_id
        LIMIT ?5
    ",

    // Every fact mentioning subject (?1 kind, ?2 id) below snapshot ?3 —
    // BACKLINK_PAGE without the page cut. The depiction walk fetches each
    // entity-class member's whole backlink set and filters to depictions in
    // Rust, so its candidate population is one entity class's mentions.
    SUBJECT_FACTS: "
        SELECT s.fact_id, f.fact_json
        FROM fact_subjects s JOIN facts f ON f.fact_id = s.fact_id
        WHERE s.kind = ?1 AND s.subject_id = ?2 AND s.fact_id < ?3
    ",

    // Keyed class-walk candidates: every fact under one facet key, below
    // snapshot bound (the trailing parameter). Each rides its partial facet
    // index; the whole candidate set is key-sized, so the walk fetches it
    // and pages in Rust with memory-identical cursor semantics. The subject
    // comes out of fact_json (the facet columns don't carry it).
    CLASS_CANDIDATES_BY_NAME: "
        SELECT fact_id, fact_json FROM facts
        WHERE name_norm = ?1 AND name_language = ?2 AND fact_id < ?3
    ",
    CLASS_CANDIDATES_BY_EXTREF: "
        SELECT fact_id, fact_json FROM facts
        WHERE external_ref = ?1 AND fact_id < ?2
    ",
    CLASS_CANDIDATES_BY_SRCURL: "
        SELECT fact_id, fact_json FROM facts
        WHERE source_url = ?1 AND fact_id < ?2
    ",

    // Spatial-walk candidates: every location-bearing fact whose covering-rect
    // envelope meets the query window (?1..?4 = the viewport's min_lat, max_lat,
    // min_lon, max_lon — a non-wrapping window; an antimeridian-crossing
    // viewport runs this twice, once per half), below snapshot ?5, placing a
    // subject of kind ?6 or ?7. The `rowid IN (SELECT rowid FROM SpatialIndex
    // ...)` form is SpatiaLite's idiom for its rtree over the `region` MBRs —
    // the pre-filter — which the query plan drives before probing facts_spatial
    // and facts by rowid.
    //
    // Single circles (lat/lon/radius set on `facts`) get their exact ellipsoidal
    // test here: ST_Distance(center, viewport-box, 1) — WGS84 meters, the same
    // geodesic core measures — within `radius_m` plus a 1 m margin. The margin
    // keeps this a conservative superset of core's `known_geometry_intersects`
    // (whose nearest-point distance over-estimates by sub-meter and admits a mm
    // rim tolerance), so the Rust refine that runs next stays the final arbiter
    // and the backends can't disagree. Compound/unresolved locations have NULL
    // radius and pass straight through to that refine. A seam-split location
    // matches through both envelope rows; the caller dedups by fact id.
    SPATIAL_CANDIDATES: "
        SELECT facts.fact_id, facts.fact_json
        FROM facts_spatial
        JOIN facts ON facts.fact_id = facts_spatial.fact_id
        WHERE facts_spatial.rowid IN (
                SELECT rowid FROM SpatialIndex
                WHERE f_table_name = 'facts_spatial' AND f_geometry_column = 'region'
                  AND search_frame = BuildMbr(?3, ?1, ?4, ?2, 4326))
          AND facts.fact_id < ?5
          AND facts_spatial.subject_kind IN (?6, ?7)
          AND (facts.radius_m IS NULL
               OR ST_Distance(MakePoint(facts.lon, facts.lat, 4326),
                              BuildMbr(?3, ?1, ?4, ?2, 4326), 1) <= facts.radius_m + 1.0)
    ",

    // The active-or-retracted HasEvent rows of a batch of events: ?1 is a
    // JSON array of event ids, ?2 the event kind tag, ?3 the exclusive
    // snapshot. Each event costs one indexed fact_subjects probe; the owner
    // comes off the event_owner facet, so nothing decodes fact_json.
    // Retraction filtering happens in Rust over the batched closure.
    EVENT_OWNERS: "
        SELECT fact_subjects.fact_id, fact_subjects.subject_id, facts.event_owner
        FROM json_each(?1)
        CROSS JOIN fact_subjects ON fact_subjects.kind = ?2
            AND fact_subjects.subject_id = json_each.value
        CROSS JOIN facts ON facts.fact_id = fact_subjects.fact_id
        WHERE fact_subjects.fact_id < ?3 AND facts.event_owner IS NOT NULL
    ",

    // The All-stream class walk: every fact_subjects row of kind ?1 below
    // snapshot ?2, its subject resolved to a representative by the
    // correlated one-seek log resolution, deduped (two same-class subjects
    // of one fact fold to one row), ordered by the computed
    // (rep, fact_id), resuming strictly past cursor (?3, ?4), at most ?5
    // rows. Retraction filtering happens in Rust over the batched closure.
    //
    // DELIBERATE FULL WALK: enumerating every class IS this stream's
    // semantics, so the kind-prefixed primary-key search visits the whole
    // subject population and every page re-sorts the walk. It passes the
    // plan verifier because the kind equality plans as a SEARCH, but no
    // index bounds the rows behind it — conformance-scale only; a
    // production consumer triggers reconsidering the stream itself.
    CLASS_WALK_ALL: concat!(
        "
        SELECT rep, fact_id FROM (
            SELECT DISTINCT
                COALESCE((",
        resolve_rep_expr!("s.subject_id", "?2"),
        "),
                         s.subject_id) AS rep,
                s.fact_id AS fact_id
            FROM fact_subjects s
            WHERE s.kind = ?1 AND s.fact_id < ?2
        )
        WHERE (rep, fact_id) > (?3, ?4)
        ORDER BY rep, fact_id
        LIMIT ?5
    "
    ),

    // ---- Temporal-conflict witness indexes ----
    //
    // Append hooks: one row per relevant staged fact, under its immutable
    // subject. Written in the staging fact's submit savepoint (see
    // super::maintain), so a rejected submit unwinds them.
    INSERT_EXISTENCE_WITNESS: "INSERT INTO existence_witness (member, date_earliest, date_latest, date_json, fact_id) VALUES (?1, ?2, ?3, ?4, ?5)",
    INSERT_EVENT_WITNESS: "INSERT INTO event_witness (event, date_earliest, date_latest, date_json, fact_id, role) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    INSERT_HAS_EVENT: "INSERT INTO has_event (member, event, fact_id) VALUES (?1, ?2, ?3)",
    INSERT_CONSTRUCTION_START: "INSERT INTO construction_start (member, date_json, fact_id) VALUES (?1, ?2, ?3)",
    INSERT_DEMOLITION_COMPLETED: "INSERT INTO demolition_completed (member, date_json, fact_id) VALUES (?1, ?2, ?3)",

    // The composed read. Each scans one subject below the exclusive snapshot
    // bound; retraction filtering runs in Rust over the batched closure, as
    // everywhere else. The bookend scans (few rows per entity) fetch whole; the
    // existence / event witness scans take the value-range predicate the
    // enumeration needs, riding the endpoint index.

    // A member's `HasEvent` edge candidates: (event, fact_id). Retraction gates
    // each edge, so ownership is per-edge (matching project_entity's
    // event_reachers), not latest-owner-wins.
    HAS_EVENT_EDGES: "SELECT event, fact_id FROM has_event WHERE member = ?1 AND fact_id < ?2",

    // A member's construction-start / demolition-completion bookend facts,
    // whole. The read joins their dates into the floor / ceiling.
    CONSTRUCTION_STARTS: "SELECT date_json, fact_id FROM construction_start WHERE member = ?1 AND fact_id < ?2",
    DEMOLITION_COMPLETIONS: "SELECT date_json, fact_id FROM demolition_completed WHERE member = ?1 AND fact_id < ?2",

    // The two per-bound value-range scans. Below the floor: witnesses whose
    // latest instant strictly precedes it (`date_latest < floor_days`) — a
    // NULL latest (open above) never matches, correctly excluded. Above the
    // ceiling: witnesses whose earliest instant strictly follows it. Each rides
    // the matching endpoint index (idx_*_latest / idx_*_earliest), so an entity
    // with no violator seeks to nothing.
    EXISTENCE_WITNESS_BELOW: "SELECT date_json, fact_id FROM existence_witness WHERE member = ?1 AND date_latest < ?2 AND fact_id < ?3",
    EXISTENCE_WITNESS_ABOVE: "SELECT date_json, fact_id FROM existence_witness WHERE member = ?1 AND date_earliest > ?2 AND fact_id < ?3",
    EVENT_WITNESS_BELOW: "SELECT date_json, fact_id, role FROM event_witness WHERE event = ?1 AND date_latest < ?2 AND fact_id < ?3",
    EVENT_WITNESS_ABOVE: "SELECT date_json, fact_id, role FROM event_witness WHERE event = ?1 AND date_earliest > ?2 AND fact_id < ?3",
}

/// Verify every fact-store query's plan — no full table scans.
pub(crate) async fn verify_query_plans(pool: &SqlitePool) -> Result<(), QueryPlanError> {
    verify_query_defs(pool, ALL).await
}
