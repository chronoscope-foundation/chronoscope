//! Write-path maintenance of the `subject_reps` log and the witness indexes.
//!
//! Both are INSERT-only and every insert happens inside the staging fact's
//! submit savepoint, so a rejected submit unwinds these rows with its staging.
//!
//! - **Staging an identity edge** ([`record_identity_edge`]): resolve both
//!   endpoints; when the classes differ, the lower representative wins and every
//!   member of the losing class gets one row pointing at it. A fresh mention
//!   loses alone — a new id never dethrones a class minimum. This is the
//!   non-recursive merge maintenance (`resolve_rep` + `class_members` +
//!   `INSERT_REP`); the retraction-driven split/revival recompute
//!   (`record_retraction`, which needs the recursive component walk) is a later
//!   unit.
//! - **Staging a witness/bookend fact** ([`record_witness`]): one INSERT under
//!   the fact's immutable subject. The witness *reads* land in a later unit
//!   alongside retraction, but the writes populate the tables now.
//!
//! Everything runs at the union bound: the staged fact itself is visible, so
//! liveness already accounts for it.

use std::collections::BTreeSet;

use sqlx::PgConnection;

use super::error::{PostgresFactStoreError as Error, sql};
use super::queries;
use super::read::{ReadBound, class_members_raw, resolve_rep_raw};
use crate::common::storage::{WitnessRow, witness_date_columns, witness_date_json};

async fn insert_rep(
    conn: &mut PgConnection,
    kind: &str,
    member: i64,
    as_of: i64,
    rep: i64,
) -> Result<(), Error> {
    sqlx::query(queries::INSERT_REP)
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
    conn: &mut PgConnection,
    fact_id: i64,
    row: WitnessRow,
) -> Result<(), Error> {
    match row {
        WitnessRow::Existence { member, date } => {
            let (earliest, latest, json) = witness_date_columns(&date)?;
            sqlx::query(queries::INSERT_EXISTENCE_WITNESS)
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
            sqlx::query(queries::INSERT_EVENT_WITNESS)
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
            sqlx::query(queries::INSERT_HAS_EVENT)
                .bind(member)
                .bind(event)
                .bind(fact_id)
                .execute(&mut *conn)
                .await
                .map_err(sql("inserting has-event edge"))?;
        }
        WitnessRow::ConstructionStart { member, date } => {
            let json = witness_date_json(&date)?;
            sqlx::query(queries::INSERT_CONSTRUCTION_START)
                .bind(member)
                .bind(&json)
                .bind(fact_id)
                .execute(&mut *conn)
                .await
                .map_err(sql("inserting construction-start bookend"))?;
        }
        WitnessRow::DemolitionCompleted { member, date } => {
            let json = witness_date_json(&date)?;
            sqlx::query(queries::INSERT_DEMOLITION_COMPLETED)
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

/// Log the merge a staged identity edge `(a, b)` performs, if any. `as_of` is
/// the staged fact's id — the log position later snapshots resolve against.
pub(super) async fn record_identity_edge(
    conn: &mut PgConnection,
    kind: &'static str,
    a: i64,
    b: i64,
    as_of: i64,
) -> Result<(), Error> {
    let bound = ReadBound::Union;
    let rep_a = resolve_rep_raw(conn, bound, kind, a).await?;
    let rep_b = resolve_rep_raw(conn, bound, kind, b).await?;
    if rep_a == rep_b {
        return Ok(());
    }
    let (winner, loser) = if rep_a < rep_b {
        (rep_a, rep_b)
    } else {
        (rep_b, rep_a)
    };
    // The set dedups the losing representative: after a split it carries a rep =
    // self log row, so the gather already returns it.
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
