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

use crate::geo::{Bbox, GeoPoint};
use crate::grammar::depiction::Perspective;
use crate::grammar::ids::FactId;
use crate::projection::{
    Bracket, Claimed, DepictionRecord, FactMap, MemberLineage, member_lineage, project_entity,
};
use crate::store::schema::EntityStream;
use crate::store::{EntityIdOf, EntityView, EventView, FactStore, ImageIdOf};
use crate::typed;

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
/// entity has none. Perspective is read straight off each depiction record's
/// bracket, so the pick settles a representative without materializing typed
/// depictions or cloning their localization geometry and citations.
fn representative_image<EntId, ImgId>(
    depictions: &FactMap<
        ImgId,
        DepictionRecord<MemberLineage<EntId, ImgId>>,
        MemberLineage<EntId, ImgId>,
    >,
) -> Option<ImgId>
where
    EntId: Ord + Clone,
    ImgId: Ord + Clone,
{
    let mut first: Option<&ImgId> = None;
    for (image, entry) in depictions {
        first.get_or_insert(image);
        if is_exterior(&entry.value.perspective) {
            return Some(image.clone());
        }
    }
    first.cloned()
}

/// Whether a depiction's perspective settled to `Exterior`, flattened and
/// settled off the record's bracket the same way the typed projection reads it.
fn is_exterior<EntId, ImgId>(
    perspective: &Bracket<Claimed<Perspective>, MemberLineage<EntId, ImgId>>,
) -> bool
where
    EntId: Ord + Clone,
    ImgId: Ord + Clone,
{
    typed::bracket(perspective)
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
/// and the walk cursor to continue past. The snapshot lets a resume re-open the
/// exact past view the first page read, so the walk continues over one stable
/// snapshot and never sees writes that landed after it started.
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

/// A listing failure: a backend error from the walk or projection.
#[derive(Debug, Clone, PartialEq)]
pub enum ListError<E> {
    Backend(E),
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
/// `view` is read at whatever snapshot the caller opened it on; a resume opens
/// the view at the `cursor`'s snapshot, so the walk continues over the same
/// pinned state the first page read. The `next` cursor carries that snapshot
/// forward. A fresh listing passes `None`.
pub async fn summaries_in_bbox<S, V>(
    view: &mut V,
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
    let snapshot = view.snapshot().await.map_err(ListError::Backend)?;
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
                project_entity::<S, V, _>(&mut *view, row.representative.clone(), member_lineage)
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
                // One representative depicted-image id — the raw depiction map
                // key. It needn't equal a tile's representative id from the images
                // sub-resource, which keys each tile by its image's `SameArtifact`
                // class; both ids resolve to the same image through that class, so
                // the marker thumbnail still loads.
                let thumbnail = representative_image(&projected.depictions);
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
        next: resume.map(|walk| ListCursor { snapshot, walk }),
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
