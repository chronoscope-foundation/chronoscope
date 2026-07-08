//! Read-side projection.
//!
//! Reads a subject's equivalence class back as a coherent value.
//! [`project_entity`] resolves an entity's `SameEntity` class, drains every
//! member's backlink facts, takes the entity→event hop to reach interior events,
//! and merges them with one generic fold: every field is a [`Slot`], so the
//! whole entity is `facts.map(inject).fold(identity, combine)`.
//! [`project_image`] does the same over an image's `SameArtifact` class.
//!
//! Provenance is in-band — each bracket bound and each membership key carries its
//! own support, threaded through a [`Semiring`](crate::algebra::semiring::Semiring).
//! Conflict is read off the same structure: a restrictive field's consensus
//! collapses to ⊥ under over-determination, surfaced through [`Slot::conflict`].

mod bracket;
pub mod claimed;
mod merge;
mod provenance;
mod slot;
mod types;

pub use bracket::{Bracket, ConsensusConflict, MAX_PROJECTED_LOCATION_CIRCLES};
pub use claimed::Claimed;
pub use provenance::{Citation, Cited, MemberLineage};
pub use slot::{FactMap, FactSet, Slot};
pub use types::{
    Bookend, DepictionRecord, Entity, Event, GlueEdge, Image, NameKey, NameRecord, RegionRecord,
    Sameness, connecting_glue, sameness_summary,
};

use std::collections::BTreeMap;
use std::future::{Future, ready};

use futures_util::{TryFutureExt, TryStreamExt};

use crate::algebra::semiring::{Lineage, Semiring};
use crate::grammar::ids::{FactId, IdScheme};
use crate::store::pagination::{PAGE_SIZE, paginate};
use crate::store::schema::{EquivClass, FactPage};
use crate::store::{
    EntityIdOf, EntityView, EventIdOf, EventView, FactStore, ImageIdOf, ImageView, StoredFactOf,
    WalkPage,
};
use crate::submit::StoredFact;

/// The member-aware lineage closure: the fact's citation becomes the singleton
/// atom `{(id, citation)}`, keyed by the source id the fact spoke to. The
/// provenance public callers pass to [`project_entity`] / [`project_image`] for
/// [`MemberLineage`]-typed projections; keeping the id in the atom is what makes
/// [`connecting_glue`] computable. A meta fact cites nothing, lifting to the
/// multiplicative identity.
pub fn member_lineage<R, Id>(
    _fact_id: &FactId,
    id: &Id,
    fact: &StoredFact<R>,
) -> MemberLineage<Id, R::Image>
where
    R: IdScheme,
    Id: Ord + Clone,
{
    match merge::citation_of(fact) {
        Some(citation) => Lineage::Of([(id.clone(), citation)].into_iter().collect()),
        None => Lineage::one(),
    }
}

/// Drain each subject's backlink walk into one `FactId`-keyed map. A fact
/// mentioning several subjects lands under one key idempotently, so the map is
/// the deduplicated union of the walks, ordered by `FactId` for a deterministic
/// fold downstream.
async fn collect_backlinks<S, Sub, Rep, F, Fut>(
    subjects: impl IntoIterator<Item = Sub>,
    mut walk: F,
) -> Result<BTreeMap<FactId, StoredFactOf<S>>, S::Error>
where
    S: FactStore,
    Sub: Copy,
    F: FnMut(Sub, Option<S::Cursor>) -> Fut,
    Fut: Future<Output = Result<WalkPage<S, Rep>, S::Error>>,
{
    let mut facts = BTreeMap::new();
    for subject in subjects {
        paginate(|cursor| walk(subject, cursor).map_ok(FactPage::into_parts))
            .try_for_each(|item| {
                facts.insert(item.fact_id, item.fact);
                ready(Ok(()))
            })
            .await?;
    }
    Ok(facts)
}

/// Project an entity's `SameEntity` class as an [`Entity`] over the
/// `provenance` closure's semiring, returning the resolved [`EquivClass`]
/// beside it, or `Ok(None)` when no committed fact ever named the id.
///
/// Resolves the class, drains every member's backlinks (aggregation is
/// class-level — each source's facts stay on its own id), takes the entity→event
/// hop to reach interior events, and folds them per field with the no-winners
/// join. Each fact is tagged through `provenance`, which sees the fact's id and
/// the whole stored fact alongside the source id it spoke to — its own subject
/// for an entity-level claim, the reaching member for an interior event. All
/// reads are snapshot-scoped and active-only, so the projection carries no
/// retraction logic of its own.
///
/// An id no committed fact names drains to an empty backlink set: the class is a
/// lone singleton with nothing to fold, so the entity does not exist at this
/// snapshot and the projection is `Ok(None)`. That is distinct from an `Err`,
/// which signals a backend failure. A real entity is declared alongside at least
/// one fact naming it, so "zero contributing facts" is the honest absence signal.
///
/// Handing the class back lets a caller thread it straight into the typed
/// transform, sparing a second `entity_class` read — one resolution covers the
/// representative, the mention count, and the projection.
pub async fn project_entity<S, V, T>(
    view: &V,
    entity_id: EntityIdOf<S>,
    provenance: impl Fn(&FactId, &EntityIdOf<S>, &StoredFactOf<S>) -> T,
) -> Result<
    Option<(
        EquivClass<EntityIdOf<S>>,
        Entity<EntityIdOf<S>, EventIdOf<S>, ImageIdOf<S>, T>,
    )>,
    S::Error,
>
where
    S: FactStore,
    V: EntityView<S> + EventView<S> + Sync,
    T: Semiring + Clone,
{
    let class = view.entity_class(&entity_id).await?;

    // A fact mentioning two subjects (a relationship, the SameEntity edge, a
    // HasEvent in both the entity and event backlink sets) lands under one
    // FactId key, idempotently; the BTreeMap keeps the set keyed and ordered by
    // FactId for a deterministic fold.
    let mut facts = collect_backlinks::<S, _, _, _, _>(class.members.iter(), |m, c| {
        view.all_facts_about_entity(m, c, PAGE_SIZE)
    })
    .await?;

    if facts.is_empty() {
        return Ok(None);
    }

    // Interior event facts key off their event id, never the entity, so the
    // entity drain alone never reaches them. The `HasEvent` facts (in the entity
    // backlinks — they mention the entity) name the entity's event ids; take a
    // second hop into each event's own backlinks. A per-event-id read mirrors the
    // entity walk one level down; full SameEvent-class aggregation is the same
    // pattern again, deferred.
    //
    // The `HasEvent` facts also name which member reaches each event; the merge
    // tags an interior event's facts with that member as their source id.
    let reachers = merge::event_reachers(&facts);
    facts.extend(
        collect_backlinks::<S, _, _, _, _>(reachers.keys(), |e, c| {
            view.all_facts_about_event(e, c, PAGE_SIZE)
        })
        .await?,
    );

    let entity = merge::project_facts(&facts, &class.members, &reachers, provenance);
    Ok(Some((class, entity)))
}

/// Project an image's `SameArtifact` class as an [`Image`] over the
/// `provenance` closure's semiring, returning the resolved [`EquivClass`]
/// beside it, or `Ok(None)` when no committed fact ever named the id.
///
/// Mirrors [`project_entity`]: resolves the class, drains every member's
/// backlinks, and folds them per field with the no-winners join. Image-level
/// facts tag their support against the member image they name; a composite
/// `IsSubimageOf` edge routes by which end the class holds — the
/// member-as-subimage records its parent, the member-as-parent records its
/// subimage. All reads are snapshot-scoped and active-only. An id no committed
/// fact names drains to an empty backlink set and projects as `Ok(None)`,
/// distinct from an `Err` backend failure.
pub async fn project_image<S, V, T>(
    view: &V,
    image_id: ImageIdOf<S>,
    provenance: impl Fn(&FactId, &ImageIdOf<S>, &StoredFactOf<S>) -> T,
) -> Result<
    Option<(
        EquivClass<ImageIdOf<S>>,
        Image<EntityIdOf<S>, ImageIdOf<S>, T>,
    )>,
    S::Error,
>
where
    S: FactStore,
    V: ImageView<S> + Sync,
    T: Semiring + Clone,
{
    let class = view.image_class(&image_id).await?;

    // A member's backlinks include every image-level fact naming it and every
    // depiction / composite edge it sits on; one FactId keys each fact once.
    let facts = collect_backlinks::<S, _, _, _, _>(class.members.iter(), |m, c| {
        view.all_facts_about_image(m, c, PAGE_SIZE)
    })
    .await?;

    if facts.is_empty() {
        return Ok(None);
    }

    let image = merge::project_image_facts(&facts, &class.members, provenance);
    Ok(Some((class, image)))
}

#[cfg(test)]
use merge::{event_reachers, project_facts, project_image_facts};

#[cfg(test)]
mod tests;
