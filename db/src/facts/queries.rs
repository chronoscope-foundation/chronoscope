//! Fact-store query definitions.
//!
//! Same [`QueryDef`] + startup `EXPLAIN QUERY PLAN` discipline as
//! [`crate::queries`]; kept beside the fact-store code because these queries
//! and the row codecs in [`super::storage`] change together.
//! [`verify_query_plans`] runs when a [`SqliteFactStore`](super::SqliteFactStore)
//! opens (its `attach` step), against the pool that has the `ovl` overlay
//! attached — the only place the fact tables these queries name resolve.
//!
//! Two-file layout: the fact tables live in a database attached as schema
//! `ovl` (the app tables stay in `main`). Reads name the tables unqualified
//! and resolve to `ovl` through the attach search order — `main` has no fact
//! tables, so there is no ambiguity, and a temp union view over base+overlay
//! could later slot in front of these same reads without touching them.
//! Writes and write-layer state (the mint counters, the pre-commit staging
//! audit) qualify `ovl.` explicitly, so they bypass any such view and land on
//! the real writable overlay. The one principle: statements reading fact
//! *data* (which a union view spans across layers) stay unqualified;
//! statements writing, or reading overlay-only *state*, qualify `ovl.`.
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

use crate::queries::{QueryDef, QueryPlanError, verify_query_defs, verify_query_plan_sql};

macro_rules! define_fact_queries {
    ($($name:ident: $sql:expr),* $(,)?) => {
        $(pub(super) const $name: QueryDef = QueryDef { name: stringify!($name), sql: $sql };)*

        /// Every fact-store query, for plan verification.
        pub(crate) const ALL: &[&QueryDef] = &[$(&$name),*];
    };
}

define_fact_queries! {
    // Mints: bump-and-return against the single counters row. RETURNING
    // evaluates post-update, so `- 1` hands back the id just consumed. The
    // counter is overlay-only write state (a singleton, never unioned across
    // layers), so it qualifies `ovl.` on both the write and the read below.
    MINT_ENTITY: "UPDATE ovl.fact_counters SET next_entity_id = next_entity_id + 1 WHERE id = 0 RETURNING next_entity_id - 1",
    MINT_EVENT: "UPDATE ovl.fact_counters SET next_event_id = next_event_id + 1 WHERE id = 0 RETURNING next_event_id - 1",
    MINT_IMAGE: "UPDATE ovl.fact_counters SET next_image_id = next_image_id + 1 WHERE id = 0 RETURNING next_image_id - 1",

    // The known-id predicates read the counters row per check: the
    // transaction's own connection sees its uncommitted bumps, so SQLite is
    // the one source of what this store has minted. Overlay-only state, so
    // `ovl.`-qualified like the mint UPDATEs.
    MINT_COUNTERS: "SELECT next_entity_id, next_event_id, next_image_id FROM ovl.fact_counters WHERE id = 0",

    INSERT_SUBJECT: "INSERT INTO ovl.fact_subjects (fact_id, kind, subject_id) VALUES (?1, ?2, ?3)",

    // One covering-rect envelope of a location-bearing fact (see the migration's
    // facts_spatial notes: seam-split halves, INSERT-only). The rect corners
    // become a SpatiaLite MBR polygon in the `region` geometry column, whose
    // managed spatial index SpatiaLite keeps in sync (the write connection sets
    // `trusted_schema=ON` so that maintenance trigger fires across the attach).
    // ?2 is the located subject's kind tag; ?3..?6 are min_lon, min_lat,
    // max_lon, max_lat (`BuildMbr`'s x/y order).
    INSERT_SPATIAL: "INSERT INTO ovl.facts_spatial (fact_id, subject_kind, region) VALUES (?1, ?2, BuildMbr(?3, ?4, ?5, ?6, 4326))",

    CLAIM_FACT: "UPDATE ovl.facts SET commit_seq = ?1 WHERE fact_id = ?2 AND commit_seq IS NULL",

    // The pre-commit audit: any visible row still unclaimed. Committed
    // state never holds one — every prior transaction passed this audit —
    // so a hit is the current transaction's own staging that no recorded
    // commit claimed. Overlay-only (the frozen base has no in-flight rows),
    // so `ovl.`-qualified rather than reading the union; probes the
    // idx_facts_unclaimed partial index, which holds only such rows.
    UNCLAIMED_STAGED_FACT: "SELECT fact_id FROM ovl.facts WHERE commit_seq IS NULL LIMIT 1",

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

    // The representative reverse gather: every member whose latest log row
    // below ?3 names ?2 as its representative — candidates off
    // idx_subject_reps_rep, each anti-joined against its own later rows by a
    // correlated primary-key probe (the representative itself, rowless when it
    // never moved, is the caller's to add). The point/batch/All-walk resolves
    // live in the base-aware builders below (resolve_rep_sql etc.); this reverse
    // gather rides the union view directly (the planner pushes the equality into
    // each branch).
    CLASS_MEMBERS: "
        SELECT s.member FROM subject_reps s
        WHERE s.kind = ?1 AND s.rep = ?2 AND s.as_of < ?3
        AND NOT EXISTS (
            SELECT 1 FROM subject_reps later
            WHERE later.kind = ?1 AND later.member = s.member
              AND later.as_of > s.as_of AND later.as_of < ?3
        )
    ",
    INSERT_REP: "INSERT INTO ovl.subject_reps (kind, member, as_of, rep) VALUES (?1, ?2, ?3, ?4)",

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

    // ---- Temporal-conflict witness indexes ----
    //
    // Append hooks: one row per relevant staged fact, under its immutable
    // subject. Written in the staging fact's submit savepoint (see
    // super::maintain), so a rejected submit unwinds them. Fact-table writes,
    // so `ovl.`-qualified like every other insert.
    INSERT_EXISTENCE_WITNESS: "INSERT INTO ovl.existence_witness (member, date_earliest, date_latest, date_json, fact_id) VALUES (?1, ?2, ?3, ?4, ?5)",
    INSERT_EVENT_WITNESS: "INSERT INTO ovl.event_witness (event, date_earliest, date_latest, date_json, fact_id, role) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    INSERT_HAS_EVENT: "INSERT INTO ovl.has_event (member, event, fact_id) VALUES (?1, ?2, ?3)",
    INSERT_CONSTRUCTION_START: "INSERT INTO ovl.construction_start (member, date_json, fact_id) VALUES (?1, ?2, ?3)",
    INSERT_DEMOLITION_COMPLETED: "INSERT INTO ovl.demolition_completed (member, date_json, fact_id) VALUES (?1, ?2, ?3)",

    // The composed read. Each drives its per-subject index probe from a
    // json_each id set — the batched `EVENT_OWNERS` shape — so a class's whole
    // member set (or owned-event set) reads in one round trip. Retraction
    // filtering runs in Rust over the batched closure, as everywhere else. The
    // bookend scans fetch whole; the existence / event witness scans carry the
    // value-range predicate the enumeration needs, riding the endpoint index.
    //
    // The `json_each(?1) CROSS JOIN <table> ON <table>.<key> = json_each.value`
    // form makes json_each the driver, so each id is one indexed probe of the
    // witness table; the CROSS JOIN pins that order against the stats-free
    // planner, which otherwise puts the witness table outermost and scans it.

    // The `HasEvent` edge candidates of a member set (?1 json array of member
    // ids, ?2 bound): (event, fact_id) per member. Retraction gates each edge, so
    // ownership is per-edge (matching project_entity's event_reachers), not
    // latest-owner-wins.
    HAS_EVENT_EDGES: "
        SELECT has_event.event, has_event.fact_id
        FROM json_each(?1)
        CROSS JOIN has_event ON has_event.member = json_each.value
        WHERE has_event.fact_id < ?2
    ",

    // The construction-start / demolition-completion bookend facts of a member
    // set (?1 json array of member ids, ?2 bound), whole. The read joins their
    // dates into the floor / ceiling.
    CONSTRUCTION_STARTS: "
        SELECT construction_start.date_json, construction_start.fact_id
        FROM json_each(?1)
        CROSS JOIN construction_start ON construction_start.member = json_each.value
        WHERE construction_start.fact_id < ?2
    ",
    DEMOLITION_COMPLETIONS: "
        SELECT demolition_completed.date_json, demolition_completed.fact_id
        FROM json_each(?1)
        CROSS JOIN demolition_completed ON demolition_completed.member = json_each.value
        WHERE demolition_completed.fact_id < ?2
    ",

    // The two per-bound value-range scans over an id set (?1 json array of ids,
    // ?2 threshold day, ?3 bound). Below the floor: witnesses whose latest
    // instant strictly precedes it (`date_latest < ?2`) — a NULL latest (open
    // above) never matches, correctly excluded. Above the ceiling: witnesses
    // whose earliest instant strictly follows it. Each id is one indexed PK
    // seek (member/event equality, fact_id bounded), the date threshold
    // filtering that id's rows; the endpoint indexes (idx_*_latest /
    // idx_*_earliest) stay available for a stats-driven planner. The event
    // scans carry `event` back so the batched rows regroup by event.
    EXISTENCE_WITNESS_BELOW: "
        SELECT existence_witness.date_json, existence_witness.fact_id
        FROM json_each(?1)
        CROSS JOIN existence_witness ON existence_witness.member = json_each.value
        WHERE existence_witness.date_latest < ?2 AND existence_witness.fact_id < ?3
    ",
    EXISTENCE_WITNESS_ABOVE: "
        SELECT existence_witness.date_json, existence_witness.fact_id
        FROM json_each(?1)
        CROSS JOIN existence_witness ON existence_witness.member = json_each.value
        WHERE existence_witness.date_earliest > ?2 AND existence_witness.fact_id < ?3
    ",
    EVENT_WITNESS_BELOW: "
        SELECT event_witness.date_json, event_witness.fact_id, event_witness.role, event_witness.event
        FROM json_each(?1)
        CROSS JOIN event_witness ON event_witness.event = json_each.value
        WHERE event_witness.date_latest < ?2 AND event_witness.fact_id < ?3
    ",
    EVENT_WITNESS_ABOVE: "
        SELECT event_witness.date_json, event_witness.fact_id, event_witness.role, event_witness.event
        FROM json_each(?1)
        CROSS JOIN event_witness ON event_witness.event = json_each.value
        WHERE event_witness.date_earliest > ?2 AND event_witness.fact_id < ?3
    ",
}

// ---- Mount-time id-counter seeding ----
//
// BASE_COUNTERS reads the frozen base's per-kind next-ids (one primary-key seek
// on its one-row table). It names `base.`, which resolves only on a mounted
// pool, so it stays out of [`ALL`] — the plan gate can't run it against the
// overlay-only pools the producer and most tests build. SEED_COUNTERS lifts the
// overlay's counters to the max of their own and the base's, so a fresh overlay
// over a populated base mints past it and a reused overlay keeps its own
// frontier; overlay-only write state, so `ovl.`-qualified.
pub(super) const BASE_COUNTERS: QueryDef = QueryDef {
    name: "BASE_COUNTERS",
    sql: "SELECT next_entity_id, next_event_id, next_image_id FROM base.fact_counters WHERE id = 0",
};
pub(super) const SEED_COUNTERS: QueryDef = QueryDef {
    name: "SEED_COUNTERS",
    sql: "UPDATE ovl.fact_counters SET \
          next_entity_id = MAX(next_entity_id, ?1), \
          next_event_id = MAX(next_event_id, ?2), \
          next_image_id = MAX(next_image_id, ?3) WHERE id = 0",
};

// The "one past the highest id/seq" expression over the mounted layers, for
// the clock read and the staging/commit inserts. The subquery sees the
// pre-statement tables, so it mints one id per statement — batching rows into a
// single INSERT would assign duplicates and trip the primary key. `MAX(col)`
// through the temp UNION *view* scans base.<table> (the planner won't push the
// aggregate into the branches), so a base mount takes the per-branch MAX of each
// layer — each an index seek on the indexed column — and combines them; the
// overlay-only form is the single-table seek. Reads `ovl.<table>` (and, mounted,
// `base.<table>`) explicitly rather than the union view; the insert targets are
// separately `ovl.`-qualified, and SQLite evaluates the subquery before the
// insert. Spliced into the three id producers so they can't drift.
fn next_id_expr(has_base: bool, table: &str, column: &str) -> String {
    if has_base {
        // COALESCE wraps `MAX + 1`, not `MAX`, so an empty store (both layers'
        // MAX NULL) reads 0, matching the overlay-only form.
        format!(
            "SELECT COALESCE(MAX(m) + 1, 0) FROM (\
             SELECT MAX({column}) AS m FROM base.{table} \
             UNION ALL SELECT MAX({column}) AS m FROM ovl.{table})"
        )
    } else {
        format!("SELECT COALESCE(MAX({column}) + 1, 0) FROM ovl.{table}")
    }
}

// The clock: one past the highest stored fact id over the mounted layers, 0 on
// an empty store.
pub(super) fn next_fact_id_sql(has_base: bool) -> String {
    next_id_expr(has_base, "facts", "fact_id")
}

// Staging: SQL assigns the dense fact_id — one past the highest over the mounted
// layers (see next_id_expr) — so a base mount continues past the frozen base's
// ids and an aborted savepoint's overlay rows vanish, the next id
// self-correcting. commit_seq stays NULL until the fact's commit records. The
// new id comes back as the insert's rowid (fact_id is the rowid alias); a
// RETURNING on a foreign-key parent would drag a scan of the child table into
// the plan. ?1..?16 bind the row columns.
pub(super) fn insert_fact_sql(has_base: bool) -> String {
    format!(
        "INSERT INTO ovl.facts (
            fact_id, fact_json,
            name_norm, name_language, external_ref, source_url,
            date_earliest, date_latest, lat, lon, radius_m,
            edge_kind, edge_a, edge_b, event_owner,
            retracts_fact_id, retracts_commit_seq
        ) VALUES (({}),
                  ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        next_id_expr(has_base, "facts", "fact_id")
    )
}

// Commit recording: SQL assigns commit_seq — one past the highest over the
// mounted layers — so the overlay's seqs continue past a frozen base's rather
// than restarting and colliding (commit_seq values must stay globally unique
// across the union, since a retractor's retracts_commit_seq matches a target
// fact's owning commit_seq). The seq comes back as the insert's rowid; a
// RETURNING clause would drag the foreign-key child check of unindexed
// facts.commit_seq into the plan as a full scan. ?1..?3 bind commit_id,
// commit_json, result_json.
pub(super) fn insert_commit_sql(has_base: bool) -> String {
    format!(
        "INSERT INTO ovl.fact_commits (commit_seq, commit_id, commit_json, result_json) \
         VALUES (({}), ?1, ?2, ?3)",
        next_id_expr(has_base, "fact_commits", "commit_seq")
    )
}

// The representative-resolution rule — the member's last subject_reps row
// strictly below the exclusive bound wins — as a SELECT yielding `rep`. `member`
// and `bound` are the SQL expressions (a bind param or a correlated column); the
// kind is always ?1. Over a base mount the top-level ORDER BY as_of DESC LIMIT 1
// through the temp UNION view scans subject_reps (the planner won't push the
// LIMIT into the branches), so the mounted form seeks each layer's last row — a
// reverse index seek per branch — and combines them; the overlay-only form is
// the single reverse seek. Spliced into the point resolve, the batch resolve,
// and the All-walk's correlated subquery so the rule can't drift.
fn resolve_rep_expr(has_base: bool, member: &str, bound: &str) -> String {
    if has_base {
        // Each layer's last row is a reverse index seek; a per-branch ORDER BY
        // LIMIT must sit inside a subquery (SQLite forbids it directly on a
        // UNION ALL branch), then the outer ORDER BY LIMIT picks the later of
        // the two.
        format!(
            "SELECT rep FROM (\
               SELECT rep, as_of FROM (\
                 SELECT rep, as_of FROM base.subject_reps \
                   WHERE kind = ?1 AND member = {member} AND as_of < {bound} \
                   ORDER BY as_of DESC LIMIT 1) \
               UNION ALL \
               SELECT rep, as_of FROM (\
                 SELECT rep, as_of FROM ovl.subject_reps \
                   WHERE kind = ?1 AND member = {member} AND as_of < {bound} \
                   ORDER BY as_of DESC LIMIT 1)\
             ) ORDER BY as_of DESC LIMIT 1"
        )
    } else {
        format!(
            "SELECT rep FROM subject_reps \
               WHERE kind = ?1 AND member = {member} AND as_of < {bound} \
               ORDER BY as_of DESC LIMIT 1"
        )
    }
}

// The point representative resolve (member ?2, bound ?3): the member's last log
// row, or none, the caller COALESCEing to self.
pub(super) fn resolve_rep_sql(has_base: bool) -> String {
    resolve_rep_expr(has_base, "?2", "?3")
}

// Batch representative resolution: the resolve rule applied to every member of a
// JSON array (?2) in one query — kind ?1, bound ?3. The json_each virtual table
// drives the outer rows; each member resolves through the same shared rule
// (COALESCE to the member where it has no log row), so the batch and point paths
// can't disagree.
pub(super) fn resolve_reps_sql(has_base: bool) -> String {
    format!(
        "SELECT je.value AS member, COALESCE(({}), je.value) AS rep FROM json_each(?2) je",
        resolve_rep_expr(has_base, "je.value", "?3")
    )
}

// The subject_id/fact_id source of the All-walk over the mounted layers: every
// fact_subjects row of kind ?1 below snapshot ?2. Over a base mount, kind = ?1
// through the union view scans (the walk IS a full walk, but the union
// materializes), so the mounted form unions each layer's kind-prefixed index
// search explicitly; the overlay-only form is the single search.
fn class_walk_subjects(has_base: bool) -> String {
    if has_base {
        "SELECT subject_id AS sub, fact_id AS fid FROM base.fact_subjects WHERE kind = ?1 AND fact_id < ?2 \
         UNION ALL \
         SELECT subject_id AS sub, fact_id AS fid FROM ovl.fact_subjects WHERE kind = ?1 AND fact_id < ?2"
            .to_owned()
    } else {
        "SELECT subject_id AS sub, fact_id AS fid FROM ovl.fact_subjects WHERE kind = ?1 AND fact_id < ?2"
            .to_owned()
    }
}

// The All-stream class walk: every subject of kind ?1 below snapshot ?2 resolved
// to its representative by the correlated one-seek log rule, deduped (two
// same-class subjects of one fact fold to one row), ordered by the computed
// (rep, fact_id), resuming strictly past cursor (?3, ?4), at most ?5 rows.
// Retraction filtering happens in Rust over the batched closure.
//
// DELIBERATE FULL WALK: enumerating every class IS this stream's semantics, so
// the kind-prefixed index search visits the whole subject population and every
// page re-sorts the walk — conformance-scale only; a production consumer
// triggers reconsidering the stream itself.
pub(super) fn class_walk_all_sql(has_base: bool) -> String {
    format!(
        "SELECT rep, fact_id FROM (
            SELECT DISTINCT COALESCE(({resolve}), sub) AS rep, fid AS fact_id
            FROM ({subjects})
        )
        WHERE (rep, fact_id) > (?3, ?4)
        ORDER BY rep, fact_id
        LIMIT ?5",
        resolve = resolve_rep_expr(has_base, "sub", "?2"),
        subjects = class_walk_subjects(has_base)
    )
}

// The identity edges a staged meta-fact ?1 can change the liveness of: its
// transitive targets, descending through retracts_fact_id (a primary-key probe)
// and retracts_commit_seq (the target commit's recorded fact ids, via json_each
// over its commit_json). The downward mirror of RETRACTOR_CLOSURE; targets' ids
// sit strictly below their retractors', so the descent terminates. Identity
// facts retract nothing, so they are the leaves the final select keeps.
//
// Over a base mount the CTE's fact_id / commit_seq joins on the temp union views
// scan both layers (the planner won't push the equality into the branches for a
// recursive join), so the mounted form splits each fact/commit reference into a
// per-layer branch — each a primary-key seek. A fact and a commit-seq each live
// in exactly one layer (ids are disjoint), so one branch matches. The
// retracts_fact descent has a base and an overlay fact branch. The RetractCommit
// descent emits three fact×commit branches — (base fact, base commit), (overlay
// fact, base commit), (overlay fact, overlay commit) — and omits (base fact,
// overlay commit): a base fact was written before the overlay existed, so it can
// only reference a base commit. The (overlay fact, base commit) branch is the
// load-bearing cross-layer case — an overlay RetractCommit targeting a commit
// that lives in the frozen base.
pub(super) fn identity_targets_sql(has_base: bool) -> String {
    if has_base {
        "WITH RECURSIVE target(fact_id) AS (
            SELECT ?1
            UNION
            SELECT t.retracts_fact_id FROM target CROSS JOIN base.facts t ON t.fact_id = target.fact_id
            WHERE t.retracts_fact_id IS NOT NULL
            UNION
            SELECT t.retracts_fact_id FROM target CROSS JOIN ovl.facts t ON t.fact_id = target.fact_id
            WHERE t.retracts_fact_id IS NOT NULL
            UNION
            SELECT je.value FROM target CROSS JOIN base.facts t ON t.fact_id = target.fact_id
            CROSS JOIN base.fact_commits c ON c.commit_seq = t.retracts_commit_seq
            CROSS JOIN json_each(c.commit_json, '$.fact_ids') je
            UNION
            SELECT je.value FROM target CROSS JOIN ovl.facts t ON t.fact_id = target.fact_id
            CROSS JOIN base.fact_commits c ON c.commit_seq = t.retracts_commit_seq
            CROSS JOIN json_each(c.commit_json, '$.fact_ids') je
            UNION
            SELECT je.value FROM target CROSS JOIN ovl.facts t ON t.fact_id = target.fact_id
            CROSS JOIN ovl.fact_commits c ON c.commit_seq = t.retracts_commit_seq
            CROSS JOIN json_each(c.commit_json, '$.fact_ids') je
        )
        SELECT f.edge_kind, f.edge_a, f.edge_b
        FROM target CROSS JOIN base.facts f ON f.fact_id = target.fact_id
        WHERE f.edge_kind IS NOT NULL
        UNION ALL
        SELECT f.edge_kind, f.edge_a, f.edge_b
        FROM target CROSS JOIN ovl.facts f ON f.fact_id = target.fact_id
        WHERE f.edge_kind IS NOT NULL"
            .to_owned()
    } else {
        "WITH RECURSIVE target(fact_id) AS (
            SELECT ?1
            UNION
            SELECT t.retracts_fact_id FROM target CROSS JOIN ovl.facts t ON t.fact_id = target.fact_id
            WHERE t.retracts_fact_id IS NOT NULL
            UNION
            SELECT je.value FROM target CROSS JOIN ovl.facts t ON t.fact_id = target.fact_id
            CROSS JOIN ovl.fact_commits c ON c.commit_seq = t.retracts_commit_seq
            CROSS JOIN json_each(c.commit_json, '$.fact_ids') je
        )
        SELECT f.edge_kind, f.edge_a, f.edge_b
        FROM target CROSS JOIN ovl.facts f ON f.fact_id = target.fact_id
        WHERE f.edge_kind IS NOT NULL"
            .to_owned()
    }
}

// One spatial-candidate branch over `schema`'s SpatiaLite shadow rtree,
// geometry table, and facts. ?1..?4 bind the viewport window (min_lat, max_lat,
// min_lon, max_lon — a non-wrapping half; an antimeridian-crossing viewport runs
// this twice), ?5 the exclusive snapshot, ?6/?7 the two subject-kind tags. The
// numbered params are shared across both branches, so the caller binds one set
// of seven.
//
// The pre-filter joins the *shadow* rtree directly —
// idx_facts_spatial_region(pkid, xmin, xmax, ymin, ymax), the real vtab
// CreateSpatialIndex builds beside the geometry column — rather than the managed
// SpatialIndex virtual table, which only ever consults `main` and returns zero
// rows (silently) for a table in an attached schema. The rtree, its geometry
// table, and its facts are all schema-qualified per layer (an rtree can't drive
// its index through a union view), so the base branch is a verbatim copy over
// `base.`. The rtree scan drives the plan (SCAN … VIRTUAL TABLE INDEX), then the
// pkid → facts_spatial → facts rowid/PK probes — no full table scan.
//
// Single circles (lat/lon/radius set on `facts`) get their exact ellipsoidal
// test here: ST_Distance(center, viewport-box, 1) — WGS84 meters, the same
// geodesic core measures — within radius_m plus a 1 m margin. The margin keeps
// this a conservative superset of core's known_geometry_intersects (whose
// nearest-point distance over-estimates by sub-meter and admits a mm rim
// tolerance), so the Rust refine that runs next stays the final arbiter and the
// backends can't disagree. Compound/unresolved locations have NULL radius and
// pass straight through to that refine. A seam-split location matches through
// both envelope rows; the caller dedups by fact id.
fn spatial_branch(schema: &str) -> String {
    format!(
        "SELECT {schema}.facts.fact_id, {schema}.facts.fact_json
         FROM {schema}.idx_facts_spatial_region r
         JOIN {schema}.facts_spatial ON {schema}.facts_spatial.rowid = r.pkid
         JOIN {schema}.facts ON {schema}.facts.fact_id = {schema}.facts_spatial.fact_id
         WHERE r.xmin <= ?4 AND r.xmax >= ?3
           AND r.ymin <= ?2 AND r.ymax >= ?1
           AND {schema}.facts.fact_id < ?5
           AND {schema}.facts_spatial.subject_kind IN (?6, ?7)
           AND ({schema}.facts.radius_m IS NULL
                OR ST_Distance(MakePoint({schema}.facts.lon, {schema}.facts.lat, 4326),
                               BuildMbr(?3, ?1, ?4, ?2, 4326), 1) <= {schema}.facts.radius_m + 1.0)"
    )
}

/// The spatial-candidate SQL for a mount: the overlay branch, plus the base
/// branch UNION ALL'd when a base is mounted. The base and overlay id spaces
/// are disjoint, so a fact appears in at most one branch (bar a within-layer
/// seam split); the caller dedups by fact id regardless.
pub(super) fn spatial_candidates_sql(has_base: bool) -> String {
    let overlay = spatial_branch("ovl");
    if has_base {
        format!("{overlay}\nUNION ALL\n{}", spatial_branch("base"))
    } else {
        overlay
    }
}

/// The per-store resolved fact-store SQL: the `has_base`-parameterized builders
/// evaluated once when the store opens (`has_base` is fixed for its lifetime),
/// so the hot paths bind a cached `&str` instead of re-`format!`-ing ~400 chars
/// per fact / member / page. `ingest build-db` binds one cached `insert_fact` /
/// `insert_commit` for millions of entities; a retraction binds one cached
/// `resolve_rep` per class member. The SQL text is byte-identical to the
/// per-call builder output — only its lifetime moves onto the store.
#[derive(Debug)]
pub(super) struct FactQueries {
    pub next_fact_id: String,
    pub insert_fact: String,
    pub insert_commit: String,
    pub resolve_rep: String,
    pub resolve_reps: String,
    pub class_walk_all: String,
    pub identity_targets: String,
    pub spatial_candidates: String,
}

impl FactQueries {
    /// Resolve every base-aware builder once for a store's fixed `has_base`.
    pub(super) fn resolve(has_base: bool) -> Self {
        Self {
            next_fact_id: next_fact_id_sql(has_base),
            insert_fact: insert_fact_sql(has_base),
            insert_commit: insert_commit_sql(has_base),
            resolve_rep: resolve_rep_sql(has_base),
            resolve_reps: resolve_reps_sql(has_base),
            class_walk_all: class_walk_all_sql(has_base),
            identity_targets: identity_targets_sql(has_base),
            spatial_candidates: spatial_candidates_sql(has_base),
        }
    }

    /// Every resolved base-aware query as `(name, sql)` — the one set the plan
    /// gate iterates, so it verifies the exact strings the store binds rather
    /// than re-deriving them (which could drift). Every field appears here, so a
    /// query added to the struct is caught by the gate.
    fn plan_checked(&self) -> [(&'static str, &str); 8] {
        [
            ("NEXT_FACT_ID", self.next_fact_id.as_str()),
            ("INSERT_FACT", self.insert_fact.as_str()),
            ("INSERT_COMMIT", self.insert_commit.as_str()),
            ("RESOLVE_REP", self.resolve_rep.as_str()),
            ("RESOLVE_REPS", self.resolve_reps.as_str()),
            ("CLASS_WALK_ALL", self.class_walk_all.as_str()),
            ("IDENTITY_TARGETS", self.identity_targets.as_str()),
            ("SPATIAL_CANDIDATES", self.spatial_candidates.as_str()),
        ]
    }
}

/// Verify every fact-store query's plan — no full table scans — against the
/// layers this pool mounts. The static queries read through the temp union
/// views (or straight `ovl` when overlay-only); the base-aware queries verify
/// the resolved strings this store actually binds (see
/// [`FactQueries::plan_checked`]), so the gated set can't drift from the bound
/// set.
pub(crate) async fn verify_query_plans(
    pool: &SqlitePool,
    fq: &FactQueries,
) -> Result<(), QueryPlanError> {
    verify_query_defs(pool, ALL).await?;
    for (name, sql) in fq.plan_checked() {
        verify_query_plan_sql(pool, name, sql).await?;
    }
    Ok(())
}
