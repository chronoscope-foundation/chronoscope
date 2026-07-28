//! Postgres fact-store query definitions.
//!
//! Postgres is one writable database — no base/overlay union, so every query is
//! fixed text (the SQLite backend's `has_base`-parameterized builders have no
//! analogue here); the one non-`const` is [`CLUSTER_TILE`], which splices the
//! shared per-range cut into its SQL. The json columns are `JSONB`: writes bind
//! the codec's JSON string with a `$N::jsonb` cast (sqlx sends the parameter as
//! `text`, and the explicit cast turns it into `jsonb`), and reads project
//! `col::text` so the [`crate::common::storage`] string codecs decode unchanged.
//!
//! Id-set-driven reads use `= ANY($n::bigint[])` / `unnest($n::bigint[])` over a
//! bound `Vec<i64>` where the SQLite backend drove a `json_each` virtual table.
//!
//! The recursive retraction CTEs (retractor closure, equivalence component,
//! identity targets) are rewritten to a single self-reference each, since
//! Postgres forbids SQLite's doubled recursive references (see their constants
//! below). The `PostGIS` spatial reads live in a later unit; the query-plan
//! validator (seed + ANALYZE + bound-constant EXPLAIN) is a later unit too, so
//! these strings are not plan-gated yet.

use chronoscope_core::store::schema::CLUSTER_TILE_N;

// ---- Mints ----
//
// Bump-and-return against the single counters row. RETURNING evaluates
// post-update, so `- 1` hands back the id just consumed. The counters row lock
// taken at `with_tx` start (SELECT ... FOR UPDATE, held to commit) serializes
// every mint, so no separate advisory lock is needed.
pub(super) const MINT_ENTITY: &str = "UPDATE fact_counters SET next_entity_id = next_entity_id + 1 WHERE id = 0 RETURNING next_entity_id - 1";
pub(super) const MINT_EVENT: &str = "UPDATE fact_counters SET next_event_id = next_event_id + 1 WHERE id = 0 RETURNING next_event_id - 1";
pub(super) const MINT_IMAGE: &str = "UPDATE fact_counters SET next_image_id = next_image_id + 1 WHERE id = 0 RETURNING next_image_id - 1";
pub(super) const MINT_FACT_ID: &str = "UPDATE fact_counters SET next_fact_id = next_fact_id + 1 WHERE id = 0 RETURNING next_fact_id - 1";
pub(super) const MINT_COMMIT_SEQ: &str = "UPDATE fact_counters SET next_commit_seq = next_commit_seq + 1 WHERE id = 0 RETURNING next_commit_seq - 1";

/// Pin the write transaction to READ COMMITTED before it runs any statement.
/// The counters-row `FOR UPDATE` serializer needs exactly RC: under a stricter
/// isolation (REPEATABLE READ / SERIALIZABLE) the held lock's contention surfaces
/// as spurious serialization-failure aborts instead of the clean block-then-
/// proceed RC gives. `SET TRANSACTION` must precede the first query in the tx.
pub(super) const SET_ISOLATION: &str = "SET TRANSACTION ISOLATION LEVEL READ COMMITTED";

/// The counters-row lock: taken immediately after `BEGIN`, held through COMMIT,
/// so match -> mint -> stage runs under one serialization point (decision #2).
pub(super) const LOCK_COUNTERS: &str = "SELECT 1 FROM fact_counters WHERE id = 0 FOR UPDATE";

/// The `(entity, event, image)` mint counters, read fresh per known-id check so
/// the row stays the one source of what the store has minted.
pub(super) const MINT_COUNTERS: &str =
    "SELECT next_entity_id, next_event_id, next_image_id FROM fact_counters WHERE id = 0";

/// The clock: one past the highest minted fact id. The counter is incremented
/// per staged fact and rolled back with a failed submit scope, so on a
/// committed reader it equals `max committed fact_id + 1`, and on the write tx
/// it equals `max staged fact_id + 1`.
pub(super) const NEXT_FACT_ID: &str = "SELECT next_fact_id FROM fact_counters WHERE id = 0";

// ---- Inserts ----

pub(super) const INSERT_SUBJECT: &str =
    "INSERT INTO fact_subjects (fact_id, kind, subject_id) VALUES ($1, $2, $3)";

// The pre-minted fact id binds as $1; fact_json binds as $2 (a JSON string cast
// to jsonb). $3..$19 are the facet columns.
pub(super) const INSERT_FACT: &str = "\
    INSERT INTO facts (
        fact_id, fact_json,
        name_norm, name_language, external_ref, source_url,
        date_earliest, date_latest, lat, lon, radius_m,
        quadkey, subject_kind,
        edge_kind, edge_a, edge_b, event_owner,
        retracts_fact_id, retracts_commit_seq
    ) VALUES (
        $1, $2::jsonb, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17,
        $18, $19
    )";

// The pre-minted commit seq binds as $1; commit_json / result_json as jsonb.
pub(super) const INSERT_COMMIT: &str = "\
    INSERT INTO fact_commits (commit_seq, commit_id, commit_json, result_json) \
     VALUES ($1, $2, $3::jsonb, $4::jsonb)";

pub(super) const INSERT_REP: &str =
    "INSERT INTO subject_reps (kind, member, as_of, rep) VALUES ($1, $2, $3, $4)";

// ---- Claim / audit ----

pub(super) const CLAIM_FACT: &str =
    "UPDATE facts SET commit_seq = $1 WHERE fact_id = $2 AND commit_seq IS NULL";

// Any visible row still unclaimed. Committed state never holds one, so a hit is
// the current transaction's own staging that no recorded commit claimed; probes
// the idx_facts_unclaimed partial index.
pub(super) const UNCLAIMED_STAGED_FACT: &str =
    "SELECT fact_id FROM facts WHERE commit_seq IS NULL LIMIT 1";

// ---- Point reads ----

pub(super) const CACHED_RESULT: &str =
    "SELECT result_json::text FROM fact_commits WHERE commit_id = $1";
pub(super) const COMMIT_KNOWN: &str = "SELECT 1 FROM fact_commits WHERE commit_id = $1";
pub(super) const COMMIT_SEQ: &str = "SELECT commit_seq FROM fact_commits WHERE commit_id = $1";
pub(super) const FACT_ROW: &str = "SELECT fact_json::text FROM facts WHERE fact_id = $1";
pub(super) const PLACEMENT_ROW: &str =
    "SELECT commit_seq IS NOT NULL FROM facts WHERE fact_id = $1";

// ---- Representative log ----

// The member's last log row strictly below the exclusive bound wins; no row
// means the member has always been its own representative.
pub(super) const RESOLVE_REP: &str = "\
    SELECT rep FROM subject_reps \
     WHERE kind = $1 AND member = $2 AND as_of < $3 \
     ORDER BY as_of DESC LIMIT 1";

// Batch representative resolution: the resolve rule applied to every member of
// a bigint[] ($2) in one query. Each member resolves through the same rule
// (COALESCE to the member where it has no log row), so batch and point paths
// can't disagree.
pub(super) const RESOLVE_REPS: &str = "\
    SELECT m AS member, \
           COALESCE(( \
             SELECT rep FROM subject_reps \
              WHERE kind = $1 AND member = m AND as_of < $3 \
              ORDER BY as_of DESC LIMIT 1 \
           ), m) AS rep \
    FROM unnest($2::bigint[]) AS m";

// Every member whose latest log row below $3 names $2 as its representative,
// each anti-joined against its own later rows. The representative itself
// (rowless when it never moved) is the caller's to add.
pub(super) const CLASS_MEMBERS: &str = "\
    SELECT s.member FROM subject_reps s \
     WHERE s.kind = $1 AND s.rep = $2 AND s.as_of < $3 \
       AND NOT EXISTS ( \
         SELECT 1 FROM subject_reps later \
          WHERE later.kind = $1 AND later.member = s.member \
            AND later.as_of > s.as_of AND later.as_of < $3 \
       )";

// The All-stream class walk: every subject of kind $1 below snapshot $2
// resolved to its representative by the correlated one-seek log rule, deduped,
// ordered by the computed (rep, fact_id), resuming strictly past cursor
// ($3, $4), at most $5 rows. Retraction filtering happens in Rust.
//
// DELIBERATE FULL WALK: enumerating every class IS this stream's semantics, so
// the kind-prefixed index search visits the whole subject population and every
// page re-sorts the walk — conformance-scale only.
pub(super) const CLASS_WALK_ALL: &str = "\
    SELECT rep, fact_id FROM ( \
        SELECT DISTINCT \
            COALESCE(( \
              SELECT r.rep FROM subject_reps r \
               WHERE r.kind = $1 AND r.member = fs.subject_id AND r.as_of < $2 \
               ORDER BY r.as_of DESC LIMIT 1 \
            ), fs.subject_id) AS rep, \
            fs.fact_id AS fact_id \
        FROM fact_subjects fs \
        WHERE fs.kind = $1 AND fs.fact_id < $2 \
    ) u \
    WHERE (u.rep, u.fact_id) > ($3, $4) \
    ORDER BY u.rep, u.fact_id \
    LIMIT $5";

// ---- Backlinks / keyed candidates ----

// One backlink-walk page of candidates: facts mentioning subject ($1 kind, $2
// id), ascending, strictly past cursor $3 (-1 opens the walk), below snapshot
// $4, at most $5 rows.
pub(super) const BACKLINK_PAGE: &str = "\
    SELECT s.fact_id, f.fact_json::text \
     FROM fact_subjects s JOIN facts f ON f.fact_id = s.fact_id \
     WHERE s.kind = $1 AND s.subject_id = $2 AND s.fact_id > $3 AND s.fact_id < $4 \
     ORDER BY s.fact_id \
     LIMIT $5";

// Every fact mentioning subject ($1 kind, $2 id) below snapshot $3 —
// BACKLINK_PAGE without the page cut. The depiction walk fetches each entity
// class member's whole backlink set and filters to depictions in Rust.
pub(super) const SUBJECT_FACTS: &str = "\
    SELECT s.fact_id, f.fact_json::text \
     FROM fact_subjects s JOIN facts f ON f.fact_id = s.fact_id \
     WHERE s.kind = $1 AND s.subject_id = $2 AND s.fact_id < $3";

// Keyed class-walk candidates: every fact under one facet key, below the
// snapshot bound (the trailing parameter). The subject comes out of fact_json.
pub(super) const CLASS_CANDIDATES_BY_NAME: &str = "\
    SELECT fact_id, fact_json::text FROM facts \
     WHERE name_norm = $1 AND name_language = $2 AND fact_id < $3";
pub(super) const CLASS_CANDIDATES_BY_EXTREF: &str = "\
    SELECT fact_id, fact_json::text FROM facts \
     WHERE external_ref = $1 AND fact_id < $2";
pub(super) const CLASS_CANDIDATES_BY_SRCURL: &str = "\
    SELECT fact_id, fact_json::text FROM facts \
     WHERE source_url = $1 AND fact_id < $2";

// ---- Event ownership ----

// The active-or-retracted HasEvent rows of a batch of events: $1 a bigint[] of
// event ids, $2 the event kind tag, $3 the exclusive snapshot. Each event costs
// one indexed fact_subjects probe; the owner comes off the event_owner facet, so
// nothing decodes fact_json. Retraction filtering happens in Rust over the
// batched closure.
pub(super) const EVENT_OWNERS: &str = "\
    SELECT s.fact_id, s.subject_id, f.event_owner \
     FROM fact_subjects s JOIN facts f ON f.fact_id = s.fact_id \
     WHERE s.kind = $2 AND s.subject_id = ANY($1::bigint[]) \
       AND s.fact_id < $3 AND f.event_owner IS NOT NULL";

// ---- Viewport clustering ----

// The clustering candidates of a batch of Morton ranges, one statement for the
// whole fan-out: $1/$2 are the ranges' parallel lo/hi bigint[]s and $3 the
// exclusive snapshot. `WITH ORDINALITY` names each range's position, and the
// LATERAL runs the top-N scan once per range — so the bound is per range,
// exactly as a range-at-a-time loop would give, and each row comes back carrying
// the bucket its cell folds under.
//
// Two values sit in the text rather than in binds, both to keep
// idx_facts_quadkey's ordered early-stopping scan. `subject_kind IN ('entity',
// 'event')` matches the index's partial predicate, which the planner proves by
// clause implication — a bound `= ANY($n::text[])` is unprovable at plan time.
// And a generic plan cannot see through a bound LIMIT, so it loses the
// early-stop preference that makes the ordered index scan cheaper than a sort;
// the per-range cut is spliced from `CLUSTER_TILE_N` instead. Retraction and the
// tile fold run in Rust.
pub(super) static CLUSTER_TILE: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        "
    SELECT r.bucket, t.fact_id, t.fact_json::text, t.quadkey
    FROM unnest($1::bigint[], $2::bigint[]) WITH ORDINALITY AS r(lo, hi, bucket)
    CROSS JOIN LATERAL (
        SELECT fact_id, fact_json, quadkey FROM facts
         WHERE quadkey BETWEEN r.lo AND r.hi
           AND fact_id < $3
           AND subject_kind IN ('entity', 'event')
         ORDER BY quadkey, fact_id
         LIMIT {CLUSTER_TILE_N}
    ) t
"
    )
});

// ---- Temporal-conflict witness inserts ----
//
// One row per relevant staged fact, under its immutable subject. Written in the
// staging fact's submit savepoint, so a rejected submit unwinds them. The
// witness *reads* (temporal_conflicts_indexed) land in a later unit alongside
// retraction, but the writes populate the tables now.
pub(super) const INSERT_EXISTENCE_WITNESS: &str = "INSERT INTO existence_witness (member, date_earliest, date_latest, date_json, fact_id) \
     VALUES ($1, $2, $3, $4::jsonb, $5)";
pub(super) const INSERT_EVENT_WITNESS: &str = "INSERT INTO event_witness (event, date_earliest, date_latest, date_json, fact_id, role) \
     VALUES ($1, $2, $3, $4::jsonb, $5, $6)";
pub(super) const INSERT_HAS_EVENT: &str =
    "INSERT INTO has_event (member, event, fact_id) VALUES ($1, $2, $3)";
pub(super) const INSERT_CONSTRUCTION_START: &str =
    "INSERT INTO construction_start (member, date_json, fact_id) VALUES ($1, $2::jsonb, $3)";
pub(super) const INSERT_DEMOLITION_COMPLETED: &str =
    "INSERT INTO demolition_completed (member, date_json, fact_id) VALUES ($1, $2::jsonb, $3)";

// ---- Retraction — recursive CTEs ----
//
// Each SQLite counterpart references its recursive working table in more than
// one branch of the recursive term, which Postgres forbids. The rewrites below
// keep a single top-level self-reference and return the same set (conformance,
// with SQLite as oracle, is the equality check); the effective-retraction
// fixpoint over the fetched edges runs in Rust (chronoscope_core::store::retraction).

// The retractor closure of a seed set: every retraction edge reachable upward
// from the seeds below the snapshot bound. $1 is a bigint[] of seed fact ids,
// $2 the exclusive snapshot. The recursion climbs the reachable *node* set with
// one self-reference (the OR unifies the per-fact and per-commit retractor
// kinds), then the final SELECT emits every (target, retractor) edge off it —
// the same edge set SQLite's four-branch `edge` CTE yields. A staged seed's NULL
// commit_seq never matches `NULL = NULL`, so it behaves as under SQLite's
// json_each seeds.
pub(super) const RETRACTOR_CLOSURE: &str = "
    WITH RECURSIVE reach(fact_id, commit_seq) AS (
        SELECT f.fact_id, f.commit_seq FROM facts f WHERE f.fact_id = ANY($1::bigint[])
        UNION
        SELECT r.fact_id, r.commit_seq FROM reach
          JOIN facts r ON (r.retracts_fact_id = reach.fact_id
                           OR r.retracts_commit_seq = reach.commit_seq)
         WHERE r.fact_id < $2
    )
    SELECT reach.fact_id AS target_id, r.fact_id AS retractor_id
    FROM reach JOIN facts r
      ON (r.retracts_fact_id = reach.fact_id OR r.retracts_commit_seq = reach.commit_seq)
     WHERE r.fact_id < $2
";

// The identity-edge facts of $1's connected component under edge kind $2, below
// snapshot $3, retracted edges included. One self-reference (SQLite grows the
// component in two directional branches): a single join over either endpoint,
// picking the far endpoint via CASE. Over-approximation is safe — the caller
// re-walks in Rust over live edges only, so an edge reached through a retracted
// link costs a fetched row, never a wrong class. Filtering the final select on
// edge_a alone is complete because membership propagates both ways.
pub(super) const EQUIV_COMPONENT: &str = "
    WITH RECURSIVE member(id) AS (
        SELECT $1
        UNION
        SELECT CASE WHEN f.edge_a = member.id THEN f.edge_b ELSE f.edge_a END
        FROM member JOIN facts f
          ON f.edge_kind = $2 AND (f.edge_a = member.id OR f.edge_b = member.id)
         WHERE f.fact_id < $3
    )
    SELECT f.fact_id, f.edge_a, f.edge_b FROM member JOIN facts f
      ON f.edge_kind = $2 AND f.edge_a = member.id WHERE f.fact_id < $3
";

// The identity edges a staged meta-fact ($1) can change the liveness of: its
// transitive targets, descending through retracts_fact_id and through a
// retracted commit's fact_ids (a jsonb array on commit_json). The downward
// mirror of RETRACTOR_CLOSURE; targets sit strictly below their retractors, so
// the descent terminates. `target` stays the single top-level self-reference —
// the two descent kinds live in a CROSS JOIN LATERAL over the joined `facts t`
// (Postgres allows the LATERAL to reference `t`, not the recursive `target`).
// `je.value` is the jsonb_array_elements column; ::text::bigint parses the JSON
// number into a fact id. Identity facts retract nothing, so they are the leaves
// the final select keeps.
pub(super) const IDENTITY_TARGETS: &str = "
    WITH RECURSIVE target(fact_id) AS (
        SELECT $1
        UNION
        SELECT nxt.fact_id FROM target
          JOIN facts t ON t.fact_id = target.fact_id
          CROSS JOIN LATERAL (
            SELECT t.retracts_fact_id AS fact_id WHERE t.retracts_fact_id IS NOT NULL
            UNION ALL
            SELECT je.value::text::bigint AS fact_id
            FROM fact_commits c
            CROSS JOIN jsonb_array_elements(c.commit_json->'fact_ids') je
            WHERE c.commit_seq = t.retracts_commit_seq
          ) nxt
    )
    SELECT f.edge_kind, f.edge_a, f.edge_b FROM target
      JOIN facts f ON f.fact_id = target.fact_id WHERE f.edge_kind IS NOT NULL
";
