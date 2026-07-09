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
            edge_kind, edge_a, edge_b,
            retracts_fact_id, retracts_commit_seq
        ) VALUES ((",
        next_fact_id_expr!(),
        "),
                  ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
    "
    ),
    INSERT_SUBJECT: "INSERT INTO fact_subjects (fact_id, kind, subject_id, current_rep) VALUES (?1, ?2, ?3, ?4)",

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

    // The identity-edge facts of ?1's connected component under edge kind
    // ?2, below snapshot ?3, retracted edges included: traversal
    // over-approximates, and the Rust side re-walks from the member over
    // active edges only, so an edge reachable only through a retracted link
    // costs a fetched row, never a wrong class. Filtering the final select
    // on edge_a alone is complete because membership propagates both ways.
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
}

/// Verify every fact-store query's plan — no full table scans.
pub(crate) async fn verify_query_plans(pool: &SqlitePool) -> Result<(), QueryPlanError> {
    verify_query_defs(pool, ALL).await
}
