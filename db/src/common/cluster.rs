//! The clustering read's decisions, over rows a backend already fetched.
//!
//! The SQL backends differ in how they reach the candidates — one batched
//! LATERAL against parallel arrays, one indexed fetch per range — but agree on
//! every rule that turns fetched rows into cells, and on the order those rules
//! run in; the conformance suite asserts that agreement across all backends.
//! Both the rules and their sequence ([`resolve_entities`]) live here, in
//! `sqlx`-free Rust over core's types, so a correction lands once. Each backend
//! keeps only its own SQL, handed over as fetches.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chronoscope_core::geo::GeoPoint;
use chronoscope_core::grammar::ids::FactId;
use chronoscope_core::store::retraction::{RetractionEdges, effective_retractor};
use chronoscope_core::store::schema::{ClusterCell, fold_cluster_cell};
use chronoscope_core::submit::{LocatedSubject, StoredFact};

use super::convert::{IdConvertError, i64_to_u64};
use super::ids::{SqlEntityId, SqlEventId, SqlIds};

/// One fetched `HasEvent` edge: the fact asserting it, the event it names, and
/// the entity owning it. Each backend's `EVENT_OWNERS` is its own SQL text, so
/// the seam names its columns and each backend's row decode says which is
/// which — a `SELECT` reordered on one side then reads wrong at that decode.
pub(crate) struct OwnerRow {
    pub fact_id: i64,
    pub event: SqlEventId,
    pub owner: SqlEntityId,
}

/// The `HasEvent` owner edges fetched for a batch of events, keyed by the fact
/// asserting each — the key the liveness gate and the winner scan both need.
pub(crate) struct OwnerCandidates {
    by_fact: BTreeMap<FactId, (SqlEventId, SqlEntityId)>,
}

impl OwnerCandidates {
    /// Key the fetched edges by fact id, so the winner scan runs ascending
    /// regardless of fetch order and each edge is gated on its own liveness.
    pub(crate) fn from_rows(
        rows: impl IntoIterator<Item = OwnerRow>,
    ) -> Result<Self, IdConvertError> {
        let by_fact = rows
            .into_iter()
            .map(|row| {
                Ok((
                    FactId::new(i64_to_u64(row.fact_id, "has-event fact id")?),
                    (row.event, row.owner),
                ))
            })
            .collect::<Result<_, IdConvertError>>()?;
        Ok(Self { by_fact })
    }

    /// The facts whose retractor closure gates these edges.
    pub(crate) fn seeds(&self) -> Vec<FactId> {
        self.by_fact.keys().copied().collect()
    }

    /// The owning entity of each event at `snapshot` — memory's
    /// `event_entity_map` scoped to one read's events. The latest active
    /// `HasEvent` wins (ascending fact id overwrites), and a retracted one is no
    /// owner edge at all, matching the memory map exactly.
    pub(crate) fn live(
        self,
        snapshot: FactId,
        retraction: &RetractionEdges,
    ) -> BTreeMap<SqlEventId, SqlEntityId> {
        let mut owners = BTreeMap::new();
        for (fid, (event, owner)) in self.by_fact {
            if effective_retractor(fid, snapshot, retraction).is_some() {
                continue;
            }
            owners.insert(event, owner);
        }
        owners
    }
}

/// Whose location a clusterable fact carries: a construction bookend names its
/// entity outright, a `MovedToLocation` names an event that has to be attributed
/// through its owner. One id per fact, so the fetched fact is free to drop the
/// moment it's decoded.
#[derive(Clone, Copy)]
pub(crate) enum ClusterSubject {
    Entity(SqlEntityId),
    Event(SqlEventId),
}

impl ClusterSubject {
    /// An image's capture point is the image walks' location, not an entity's,
    /// so it yields no cluster subject.
    fn of(subject: LocatedSubject<'_, SqlIds>) -> Option<Self> {
        match subject {
            LocatedSubject::Entity(entity) => Some(Self::Entity(*entity)),
            LocatedSubject::Event(event) => Some(Self::Event(*event)),
            LocatedSubject::Image(_) => None,
        }
    }
}

/// A batch of located facts split by whose location each carries: the entity
/// ones are attributed already, the moves still need their event's owner.
pub(crate) struct LocatedSubjects {
    entities: Vec<(SqlEntityId, FactId)>,
    moved: Vec<(SqlEventId, FactId)>,
}

impl LocatedSubjects {
    /// Split already-decoded subjects by kind.
    pub(crate) fn new(subjects: impl IntoIterator<Item = (FactId, ClusterSubject)>) -> Self {
        let mut entities: Vec<(SqlEntityId, FactId)> = Vec::new();
        let mut moved: Vec<(SqlEventId, FactId)> = Vec::new();
        for (fid, subject) in subjects {
            match subject {
                ClusterSubject::Entity(entity) => entities.push((entity, fid)),
                ClusterSubject::Event(event) => moved.push((event, fid)),
            }
        }
        Self { entities, moved }
    }

    /// Split `located` by subject kind, decoding each fact's subject here. Image
    /// locations belong to the image walks, so they fall out.
    pub(crate) fn partition<'a>(
        located: impl IntoIterator<Item = &'a (FactId, StoredFact<SqlIds>)>,
    ) -> Self {
        Self::new(located.into_iter().filter_map(|(fid, fact)| {
            let (_, subject) = fact.located_subject()?;
            Some((*fid, ClusterSubject::of(subject)?))
        }))
    }

    /// The events whose ownership the attribution needs.
    pub(crate) fn events(&self) -> BTreeSet<SqlEventId> {
        self.moved.iter().map(|(event, _)| *event).collect()
    }

    /// Attribute every located fact to an entity: a bookend to its own, a move
    /// to its event's owner. An orphaned move — no live owner — attributes to
    /// nothing, so it places no entity anywhere.
    pub(crate) fn attribute(
        self,
        owners: &BTreeMap<SqlEventId, SqlEntityId>,
    ) -> Vec<(SqlEntityId, FactId)> {
        let mut subjects = self.entities;
        for (event, fid) in self.moved {
            if let Some(entity) = owners.get(&event) {
                subjects.push((*entity, fid));
            }
        }
        subjects
    }
}

/// The members one batched representative resolve takes: each attributed
/// subject once, however many facts placed it.
fn distinct_subjects(subjects: &[(SqlEntityId, FactId)]) -> Vec<SqlEntityId> {
    subjects
        .iter()
        .map(|(subject, _)| *subject)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Replace each attributed subject with its `SameEntity` representative, so
/// facts placing two merged entities fold into one cell member. A member with no
/// log row is its own representative — the rule the batch query already applies,
/// restated for the map lookup.
fn to_representatives(
    subjects: Vec<(SqlEntityId, FactId)>,
    reps: &HashMap<SqlEntityId, SqlEntityId>,
) -> Vec<(SqlEntityId, FactId)> {
    subjects
        .into_iter()
        .map(|(subject, fid)| (reps.get(&subject).copied().unwrap_or(subject), fid))
        .collect()
}

/// Attribute a batch of located facts to their entity representatives: the moves
/// name the events `fetch_owners` attributes, and every attributed member
/// resolves through one `fetch_reps` batch, so the read's cost stays flat in the
/// viewport's density.
///
/// The step order is a rule like the steps themselves, so it lives here and both
/// backends drive it. `ctx` — a connection — is threaded through both fetches so
/// each borrows it in turn; each backend keeps its own error type.
pub(crate) async fn resolve_entities<Ctx, Err>(
    located: LocatedSubjects,
    ctx: &mut Ctx,
    fetch_owners: impl AsyncFnOnce(
        &mut Ctx,
        &BTreeSet<SqlEventId>,
    ) -> Result<BTreeMap<SqlEventId, SqlEntityId>, Err>,
    fetch_reps: impl AsyncFnOnce(
        &mut Ctx,
        &[SqlEntityId],
    ) -> Result<HashMap<SqlEntityId, SqlEntityId>, Err>,
) -> Result<Vec<(SqlEntityId, FactId)>, Err> {
    let owners = fetch_owners(ctx, &located.events()).await?;
    let subjects = located.attribute(&owners);
    let reps = fetch_reps(ctx, &distinct_subjects(&subjects)).await?;
    Ok(to_representatives(subjects, &reps))
}

/// Where a surviving candidate folds: its Morton key and the point it pins. The
/// range it was fetched under keys it, beside the fact id.
struct Placement {
    quadkey: i64,
    center: GeoPoint,
}

/// The clusterable remainder of a fetched candidate batch: each survivor's
/// placement under the `(range, fact)` pair it was fetched as, beside the
/// subject each surviving fact places.
///
/// A fact reachable from two ranges places one subject and folds into each range
/// once, since the pair keys the placement. The entity resolve reorders its rows
/// and answers per fact, so the fold rejoins the two by fact id.
pub(crate) struct PlacedCandidates<B> {
    placements: BTreeMap<(B, FactId), Placement>,
    subjects: BTreeMap<FactId, ClusterSubject>,
}

/// Cut a fetched candidate batch to what can fold: a retracted fact is gone at
/// the snapshot, a location denoting a region pins no center to cluster on, and
/// an image's capture point places no entity. Each survivor keeps its subject
/// and its placement — one id and one point — so the batch's decoded facts drop
/// here, ahead of the resolve's round trips.
pub(crate) fn place_candidates<B: Ord>(
    candidates: Vec<(B, FactId, i64, StoredFact<SqlIds>)>,
    snapshot: FactId,
    retraction: &RetractionEdges,
) -> PlacedCandidates<B> {
    let mut placements = BTreeMap::new();
    let mut subjects = BTreeMap::new();
    for (bucket, fact_id, quadkey, fact) in candidates {
        if effective_retractor(fact_id, snapshot, retraction).is_some() {
            continue;
        }
        let Some((location, subject)) = fact.located_subject() else {
            continue;
        };
        let Some(subject) = ClusterSubject::of(subject) else {
            continue;
        };
        let Some(center) = location.point() else {
            continue;
        };
        placements.insert(
            (bucket, fact_id),
            Placement {
                quadkey,
                center: *center,
            },
        );
        subjects.insert(fact_id, subject);
    }
    PlacedCandidates {
        placements,
        subjects,
    }
}

impl<B: Ord> PlacedCandidates<B> {
    /// The survivors the entity resolve attributes, each fact once however many
    /// ranges fetched it.
    pub(crate) fn subjects(&self) -> LocatedSubjects {
        LocatedSubjects::new(self.subjects.iter().map(|(fid, subject)| (*fid, *subject)))
    }

    /// Fold each range's survivors to at most one cell, in bucket order. Ranges
    /// fold independently — a bucket is a whole cell's membership — so two
    /// ranges never bleed into one cell, and an empty one yields none.
    pub(crate) fn fold(
        self,
        resolved: impl IntoIterator<Item = (SqlEntityId, FactId)>,
    ) -> Vec<ClusterCell<SqlEntityId>> {
        let reps: BTreeMap<FactId, SqlEntityId> =
            resolved.into_iter().map(|(rep, fid)| (fid, rep)).collect();
        let mut folded: BTreeMap<B, Vec<(i64, FactId, SqlEntityId, GeoPoint)>> = BTreeMap::new();
        for ((bucket, fid), placement) in self.placements {
            if let Some(rep) = reps.get(&fid) {
                folded.entry(bucket).or_default().push((
                    placement.quadkey,
                    fid,
                    *rep,
                    placement.center,
                ));
            }
        }
        folded
            .into_values()
            .filter_map(|survivors| fold_cluster_cell(&survivors))
            .collect()
    }
}
