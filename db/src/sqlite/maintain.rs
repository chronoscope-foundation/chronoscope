//! Write-path maintenance of the `subject_reps` log.
//!
//! The log is INSERT-only and every insert happens inside the staging
//! fact's submit savepoint, so a rejected submit unwinds its rep rows with
//! its staging. Two events move representatives:
//!
//! - **Staging an identity edge** ([`record_identity_edge`]): resolve both
//!   endpoints; when the classes differ, the lower representative wins and
//!   every member of the losing class gets one row pointing at it. A fresh
//!   mention loses alone — a new id never dethrones a class minimum.
//! - **Staging a retraction** ([`record_retraction`]): the staged fact's
//!   transitive targets may include identity edges (directly, through a
//!   retracted retractor, or through a retracted commit), flipping their
//!   liveness either way — a split, or a revival re-merging classes. The
//!   affected components are recomputed from scratch over live edges via
//!   the [`EQUIV_COMPONENT`](super::queries::EQUIV_COMPONENT) traversal
//!   plus the shared retraction fixpoint, and every member whose
//!   representative moved gets one row — `rep = member` included, for
//!   members returning to self.
//!
//! Everything runs at the union bound: the staged fact itself is visible,
//! so liveness already accounts for it.

use std::collections::{BTreeMap, BTreeSet};

use sqlx::SqliteConnection;

use chronoscope_core::store::equiv::EquivAdjacency;
use chronoscope_core::store::retraction::effective_retractor;
use chronoscope_core::store::schema::EquivClass;

use super::error::{SqliteFactStoreError, sql};
use super::queries::{self, FactQueries};
use super::read::{ReadBound, class_members_raw, resolve_rep_raw, retraction_edges};
use crate::common::convert::seed_ids;
use crate::common::storage::{WitnessRow, witness_date_columns, witness_date_json};

async fn insert_rep(
    conn: &mut SqliteConnection,
    kind: &str,
    member: i64,
    as_of: i64,
    rep: i64,
) -> Result<(), SqliteFactStoreError> {
    sqlx::query(queries::INSERT_REP.sql)
        .bind(kind)
        .bind(member)
        .bind(as_of)
        .bind(rep)
        .execute(&mut *conn)
        .await
        .map_err(sql("inserting representative log row"))?;
    Ok(())
}

/// Append the temporal-index row a staged fact seeds. `fact_id` is the staged
/// fact's id. Each variant is one INSERT under the fact's immutable subject; a
/// rejected submit's savepoint unwinds it with the staging.
pub(super) async fn record_witness(
    conn: &mut SqliteConnection,
    fact_id: i64,
    row: WitnessRow,
) -> Result<(), SqliteFactStoreError> {
    match row {
        WitnessRow::Existence { member, date } => {
            let (earliest, latest, json) = witness_date_columns(&date)?;
            sqlx::query(queries::INSERT_EXISTENCE_WITNESS.sql)
                .bind(member)
                .bind(earliest)
                .bind(latest)
                .bind(&json)
                .bind(fact_id)
                .execute(&mut *conn)
                .await
                .map_err(sql("inserting existence witness"))?;
        }
        WitnessRow::EventDate { event, date, role } => {
            let (earliest, latest, json) = witness_date_columns(&date)?;
            sqlx::query(queries::INSERT_EVENT_WITNESS.sql)
                .bind(event)
                .bind(earliest)
                .bind(latest)
                .bind(&json)
                .bind(fact_id)
                .bind(role)
                .execute(&mut *conn)
                .await
                .map_err(sql("inserting event witness"))?;
        }
        WitnessRow::HasEvent { member, event } => {
            sqlx::query(queries::INSERT_HAS_EVENT.sql)
                .bind(member)
                .bind(event)
                .bind(fact_id)
                .execute(&mut *conn)
                .await
                .map_err(sql("inserting has-event edge"))?;
        }
        WitnessRow::ConstructionStart { member, date } => {
            let json = witness_date_json(&date)?;
            sqlx::query(queries::INSERT_CONSTRUCTION_START.sql)
                .bind(member)
                .bind(&json)
                .bind(fact_id)
                .execute(&mut *conn)
                .await
                .map_err(sql("inserting construction-start bookend"))?;
        }
        WitnessRow::DemolitionCompleted { member, date } => {
            let json = witness_date_json(&date)?;
            sqlx::query(queries::INSERT_DEMOLITION_COMPLETED.sql)
                .bind(member)
                .bind(&json)
                .bind(fact_id)
                .execute(&mut *conn)
                .await
                .map_err(sql("inserting demolition-completion bookend"))?;
        }
    }
    Ok(())
}

/// Log the merge a staged identity edge `(a, b)` performs, if any. `as_of`
/// is the staged fact's id — the log position later snapshots resolve
/// against.
pub(super) async fn record_identity_edge(
    conn: &mut SqliteConnection,
    fq: &FactQueries,
    kind: &'static str,
    a: i64,
    b: i64,
    as_of: i64,
) -> Result<(), SqliteFactStoreError> {
    let bound = ReadBound::Union;
    let rep_a = resolve_rep_raw(conn, bound, fq, kind, a).await?;
    let rep_b = resolve_rep_raw(conn, bound, fq, kind, b).await?;
    if rep_a == rep_b {
        return Ok(());
    }
    let (winner, loser) = if rep_a < rep_b {
        (rep_a, rep_b)
    } else {
        (rep_b, rep_a)
    };
    // The set dedups the losing representative: after a split it carries a
    // rep = self log row, so the gather already returns it.
    let mut losing: BTreeSet<i64> = class_members_raw(conn, bound, kind, loser)
        .await?
        .into_iter()
        .collect();
    losing.insert(loser);
    for member in losing {
        insert_rep(conn, kind, member, as_of, winner).await?;
    }
    Ok(())
}

/// The live component of a raw subject under one edge kind at the union
/// bound: the component traversal over-approximates (retracted edges
/// included), then the batched retractor closure and the shared fixpoint
/// gate each edge, and the adjacency walk from `member` keeps only what
/// live edges reach.
async fn live_component(
    conn: &mut SqliteConnection,
    kind: &str,
    member: i64,
) -> Result<EquivClass<i64>, SqliteFactStoreError> {
    let bound = ReadBound::Union;
    let rows: Vec<(i64, i64, i64)> = sqlx::query_as(queries::EQUIV_COMPONENT.sql)
        .bind(member)
        .bind(kind)
        .bind(bound.bind())
        .fetch_all(&mut *conn)
        .await
        .map_err(sql("fetching identity-edge component"))?;
    let seeds = seed_ids(rows.iter().map(|(fid, _, _)| fid), "component edge fact id")?;
    let retraction = retraction_edges(conn, bound, &seeds).await?;
    let mut edges = Vec::with_capacity(rows.len());
    for ((_, a, b), fid) in rows.iter().zip(&seeds) {
        if effective_retractor(*fid, bound.fact_id(), &retraction).is_some() {
            continue;
        }
        edges.push((*a, *b));
    }
    Ok(EquivAdjacency::from_edges(edges).class_of(member))
}

/// Recompute representatives around the identity edges a staged retraction
/// touches. `staged` is the staged meta-fact's id, doubling as the log
/// position of every row this writes.
pub(super) async fn record_retraction(
    conn: &mut SqliteConnection,
    fq: &FactQueries,
    staged: i64,
) -> Result<(), SqliteFactStoreError> {
    let bound = ReadBound::Union;
    let edges: Vec<(String, i64, i64)> = sqlx::query_as(&fq.identity_targets)
        .bind(staged)
        .fetch_all(&mut *conn)
        .await
        .map_err(sql("collecting a retraction's identity targets"))?;
    let mut endpoints: BTreeMap<String, BTreeSet<i64>> = BTreeMap::new();
    for (kind, a, b) in edges {
        let seeds = endpoints.entry(kind).or_default();
        seeds.insert(a);
        seeds.insert(b);
    }

    for (kind, seeds) in &endpoints {
        // Post-retraction truth: the live component (and its minimum, the
        // new representative) of every subject reachable from a touched
        // edge. Walking each seed covers every affected member — a member's
        // new component always contains a touched edge's endpoint, since an
        // untouched component keeps its representative.
        let mut new_rep: BTreeMap<i64, i64> = BTreeMap::new();
        for &seed in seeds {
            if new_rep.contains_key(&seed) {
                continue;
            }
            let class = live_component(conn, kind, seed).await?;
            let rep = class.representative;
            for member in class.members {
                new_rep.insert(member, rep);
            }
        }

        // Pre-retraction truth: the log classes around the same seeds, each
        // member paired with the representative it currently resolves to.
        let mut current: BTreeMap<i64, i64> = BTreeMap::new();
        let mut gathered: BTreeSet<i64> = BTreeSet::new();
        for &seed in seeds {
            let rep = resolve_rep_raw(conn, bound, fq, kind, seed).await?;
            if !gathered.insert(rep) {
                continue;
            }
            current.insert(rep, rep);
            for member in class_members_raw(conn, bound, kind, rep).await? {
                current.insert(member, rep);
            }
        }
        // A revival can pull in members the gathered classes don't cover;
        // resolve their current reps individually.
        let missing: Vec<i64> = new_rep
            .keys()
            .filter(|member| !current.contains_key(member))
            .copied()
            .collect();
        for member in missing {
            let rep = resolve_rep_raw(conn, bound, fq, kind, member).await?;
            current.insert(member, rep);
        }

        let mut changes: Vec<(i64, i64)> = Vec::new();
        for (&member, &cur) in &current {
            let new = match new_rep.get(&member).copied() {
                Some(rep) => rep,
                // Not live-reachable from any seed: its component kept its
                // edges, so it is (or has become) its own singleton's truth
                // — recompute directly rather than assume.
                None => live_component(conn, kind, member).await?.representative,
            };
            if new != cur {
                changes.push((member, new));
            }
        }
        for (member, rep) in changes {
            insert_rep(conn, kind, member, staged, rep).await?;
        }
    }
    Ok(())
}
