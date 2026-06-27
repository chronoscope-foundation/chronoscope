//! Read-side entity projection.
//!
//! Reads one entity's `SameEntity` equivalence class back as a coherent value.
//! [`project_entity`] resolves the class, drains every member's backlink facts,
//! takes the entity→event hop to reach interior events, and merges them with one
//! generic fold: every field is a [`Slot`], so the whole entity is
//! `facts.map(inject).fold(identity, combine)`.
//!
//! Provenance is in-band — each bracket bound and each membership key carries its
//! own support, threaded through a [`Semiring`](crate::algebra::semiring::Semiring).
//! Conflict is read off the same structure: a restrictive field's consensus
//! collapses to ⊥ under over-determination, surfaced through [`Slot::conflict`].

mod bracket;
mod merge;
mod provenance;
mod slot;
mod types;

pub use bracket::{Bracket, ConsensusConflict, MAX_PROJECTED_LOCATION_CIRCLES};
pub use provenance::{Citation, Cited, MemberLineage};
pub use slot::{FactMap, FactSet, Slot};
pub use types::{
    Bookend, EventRecord, GlueEdge, NameKey, NameRecord, ProjectedEntity, Sameness,
    connecting_glue, sameness_summary,
};

use std::collections::BTreeMap;

use crate::algebra::semiring::{Lineage, Semiring};
use crate::facts::drain::{DRAIN_PAGE, drain_id_facts};
use crate::facts::ids::FactId;
use crate::facts::store::{EntityView, EventView, FactStore, StoredFactOf};

/// The member-aware lineage closure: a `(source id, citation)` pair becomes the
/// singleton atom `{(id, citation)}`. The provenance public callers pass to
/// [`project_entity`] for [`MemberLineage`]-typed projections; keeping the id in
/// the atom is what makes [`connecting_glue`] computable.
pub fn member_lineage<EntId, ImgId>(
    id: &EntId,
    citation: &Citation<ImgId>,
) -> MemberLineage<EntId, ImgId>
where
    EntId: Ord + Clone,
    ImgId: Ord + Clone,
{
    Lineage::Of([(id.clone(), citation.clone())].into_iter().collect())
}

/// Project an entity's `SameEntity` class as a [`ProjectedEntity`] over the
/// `provenance` closure's semiring.
///
/// Resolves the class, drains every member's backlinks (aggregation is
/// class-level — each source's facts stay on its own id), takes the entity→event
/// hop to reach interior events, and folds them per field with the no-winners
/// join. Each fact is tagged through `provenance` against the source id it spoke
/// to — its own subject for an entity-level claim, the reaching member for an
/// interior event. All reads are snapshot-scoped and active-only, so the
/// projection carries no retraction logic of its own.
pub async fn project_entity<S, V, T>(
    view: &V,
    entity_id: S::EntityId,
    provenance: impl Fn(&S::EntityId, &Citation<S::ImageId>) -> T,
) -> Result<ProjectedEntity<S::EntityId, S::EventId, T>, S::Error>
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
    let mut facts: BTreeMap<FactId, StoredFactOf<S>> = BTreeMap::new();
    for member in &class.members {
        let member_facts =
            drain_id_facts(|cursor| view.all_facts_about_entity(member, cursor, DRAIN_PAGE))
                .await?;
        facts.extend(member_facts);
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
    for event in reachers.keys() {
        let event_facts =
            drain_id_facts(|cursor| view.all_facts_about_event(event, cursor, DRAIN_PAGE)).await?;
        facts.extend(event_facts);
    }

    Ok(merge::project_facts(&facts, &reachers, provenance))
}

#[cfg(test)]
use merge::{event_reachers, project_facts};

#[cfg(test)]
mod tests;
