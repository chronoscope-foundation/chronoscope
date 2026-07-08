//! Read-path enumeration: entity summaries in a map viewport, paginated
//! against a pinned snapshot.
//!
//! A viewport lists placeable entities — each class projected once, kept only
//! when it resolves to a point on the map. The walk side
//! ([`EntityView::walk_entity_classes`] over [`EntityStream::InBbox`]) does the
//! spatial index and equivalence grouping; this layer turns each representative
//! into an [`EntitySummary`] and threads a snapshot-pinned cursor so a scroll
//! resumes exactly where it stopped.
//!
//! The walk pages representatives, so the page size is `limit` itself: a page
//! carries up to `limit` representatives, the cursor resumes past them, and the
//! result overshoots `limit` by at most one page (a representative dropped for
//! being unplaceable never consumes a slot).

use std::num::NonZeroUsize;

use chrono::NaiveDate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::claimed::Claimed;
use crate::facts::depiction::Perspective;
use crate::facts::ids::FactId;
use crate::facts::projection::{member_lineage, project_entity};
use crate::facts::schema::EntityStream;
use crate::facts::store::{EntityIdOf, EntityView, EventView, FactStore, ImageIdOf};
use crate::facts::typed;
use crate::geo::{Bbox, GeoPoint};

/// The one point an entity pins down on the map, or `None` when its location is
/// a symbolic reference, a disjunction, or otherwise unresolved. Reads the
/// entity-level location — the destination of its latest move, else its
/// construction site — so a moved entity lists at its current marker.
pub fn extract_point<EntId: Ord, EvtId, ImgId>(
    entity: &typed::Entity<EntId, EvtId, ImgId>,
) -> Option<GeoPoint> {
    entity.location.possible.point().cloned()
}

/// The image whose thumbnail represents this entity on the map: the first
/// depiction classified `Exterior`, else the first depiction, `None` when the
/// entity has none. Exterior is read off the depiction's own perspective —
/// cheap — so the marker listing settles on a representative without
/// re-projecting the image.
fn representative_image<EntId, EvtId, ImgId>(
    entity: &typed::Entity<EntId, EvtId, ImgId>,
) -> Option<ImgId>
where
    EntId: Ord,
    ImgId: Clone,
{
    entity
        .depictions
        .iter()
        .find(|d| is_exterior(&d.perspective))
        .or_else(|| entity.depictions.first())
        .map(|d| d.other.clone())
}

/// Whether a depiction's perspective settled to `Exterior`.
fn is_exterior<ImgId>(perspective: &typed::Bounded<Claimed<Perspective>, ImgId>) -> bool {
    perspective
        .settled()
        .map(|p| *p == Perspective::Exterior)
        .unwrap_or(false)
}

/// One placeable entity in a viewport: its id, names, current marker, and the
/// date span of its timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(bound(
    deserialize = "EntId: ::serde::Deserialize<'de>, ImgId: ::serde::de::DeserializeOwned"
))]
pub struct EntitySummary<EntId, ImgId> {
    pub id: EntId,
    pub names: Vec<typed::Name<ImgId>>,
    pub point: GeoPoint,
    /// The earliest lower date bound across the timeline, or `None` when the
    /// entity is undated.
    pub earliest: Option<NaiveDate>,
    /// The latest upper date bound across the timeline, or `None` when the
    /// entity is undated.
    pub latest: Option<NaiveDate>,
    /// The image whose thumbnail stands in for this entity on the map, or
    /// `None` when the entity has no depiction. The marker read path resolves
    /// it to a URL; see [`representative_image`].
    pub thumbnail: Option<ImgId>,
}

/// A resume token for [`summaries_in_bbox`]: the snapshot it was minted against
/// and the walk cursor to continue past. Pinning the snapshot lets a resume
/// reject a token from a different view rather than silently mixing states.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListCursor<Cur> {
    pub snapshot: FactId,
    pub walk: Cur,
}

/// One page of a viewport listing: the summaries gathered this page and the
/// cursor to fetch the next, `None` once the viewport is exhausted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(bound(
    deserialize = "EntId: ::serde::Deserialize<'de>, ImgId: ::serde::de::DeserializeOwned, Cur: ::serde::Deserialize<'de>"
))]
pub struct EntityListPage<EntId, ImgId, Cur> {
    pub summaries: Vec<EntitySummary<EntId, ImgId>>,
    pub next: Option<ListCursor<Cur>>,
}

/// A listing failure: a backend error from the walk or projection, or a cursor
/// minted against a different snapshot than the view it was replayed on.
#[derive(Debug, Clone, PartialEq)]
pub enum ListError<E> {
    Backend(E),
    SnapshotMismatch,
}

/// List the entities whose current marker falls in `bbox`, at the view's
/// snapshot, resuming past `cursor`.
///
/// The spatial walk surfaces a representative by any of its historical in-box
/// locations, so a built-then-moved-out entity arrives here with a marker
/// ([`extract_point`], its current location) outside the box. Each is projected
/// once and kept only when that current marker resolves and lands in `bbox`, so
/// every listed pin sits in-view; an entity dropped this way never consumes a
/// `limit` slot. The walk pages by representative, so `limit` doubles as the page
/// size: the result holds at least `limit` summaries (or every one the viewport
/// has) and overshoots by at most one page.
///
/// A `cursor` from a different snapshot yields [`ListError::SnapshotMismatch`];
/// a fresh listing passes `None`.
pub async fn summaries_in_bbox<S, V>(
    view: &V,
    bbox: &Bbox,
    cursor: Option<ListCursor<S::ClassCursor<EntityIdOf<S>>>>,
    limit: NonZeroUsize,
) -> Result<
    EntityListPage<EntityIdOf<S>, ImageIdOf<S>, S::ClassCursor<EntityIdOf<S>>>,
    ListError<S::Error>,
>
where
    S: FactStore,
    V: EntityView<S> + EventView<S> + Sync,
{
    if let Some(c) = &cursor
        && c.snapshot != view.snapshot()
    {
        return Err(ListError::SnapshotMismatch);
    }

    let mut after = cursor.map(|c| c.walk);
    let mut summaries: Vec<EntitySummary<EntityIdOf<S>, ImageIdOf<S>>> = Vec::new();

    let resume = loop {
        let stream = EntityStream::InBbox(bbox);
        let page = view
            .walk_entity_classes(&stream, after, limit)
            .await
            .map_err(ListError::Backend)?;

        // Rows arrive ordered by (representative, fact_id), and `next_class`
        // resumes strictly past the last representative, so a class never
        // straddles a page — deduping consecutive rows covers the whole walk.
        let mut last_rep: Option<EntityIdOf<S>> = None;
        for row in &page.rows {
            if last_rep.as_ref() == Some(&row.representative) {
                continue;
            }
            last_rep = Some(row.representative.clone());

            // A representative surfaced by the walk was named by the fact that
            // placed it, so it always projects; skip a `None` rather than panic.
            let Some((class, projected)) =
                project_entity::<S, V, _>(view, row.representative.clone(), member_lineage)
                    .await
                    .map_err(ListError::Backend)?
            else {
                continue;
            };
            let entity = typed::Entity::parse(&projected, &class);
            if let Some(point) = extract_point(&entity)
                && bbox.contains(&point)
            {
                let (earliest, latest) = timeline_span(entity.timeline.events());
                let thumbnail = representative_image(&entity);
                summaries.push(EntitySummary {
                    id: entity.id,
                    names: entity.names,
                    point,
                    earliest,
                    latest,
                    thumbnail,
                });
            }
        }

        match page.next_class {
            None => break None,
            Some(next) if summaries.len() >= limit.get() => break Some(next),
            Some(next) => after = Some(next),
        }
    };

    Ok(EntityListPage {
        summaries,
        next: resume.map(|walk| ListCursor {
            snapshot: view.snapshot(),
            walk,
        }),
    })
}

/// The date span of a parsed timeline: the earliest lower bound and latest upper
/// bound across every dated event, each `None` when nothing dates that side.
fn timeline_span<EvtId, ImgId>(
    events: &[typed::TimelineEvent<EvtId, ImgId>],
) -> (Option<NaiveDate>, Option<NaiveDate>) {
    let mut earliest: Option<NaiveDate> = None;
    let mut latest: Option<NaiveDate> = None;
    for event in events {
        for date in typed::entry_date_bounds(&event.detail) {
            if let Some(lo) = date.possible.earliest() {
                earliest = Some(earliest.map_or(lo, |cur| cur.min(lo)));
            }
            if let Some(hi) = date.possible.latest() {
                latest = Some(latest.map_or(hi, |cur| cur.max(hi)));
            }
        }
    }
    (earliest, latest)
}
