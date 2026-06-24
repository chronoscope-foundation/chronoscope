//! Read-side entity projection.
//!
//! Reads one entity's `SameEntity` equivalence class back as a coherent value.
//! [`project_entity`] resolves the class, drains every member's backlink facts,
//! merges them per field with list-all / join rules (no winners), and builds a
//! pure [`ProjectedEntity`]. Provenance rides alongside in a [`CitationMap`]
//! keyed by [`JsonPath`] addresses into the value, rather than inline on each
//! field.
//!
//! The merge never picks a "best" claim. Multi-valued attributes (names,
//! relations, external references) list every distinct claim; single-valued
//! slots that several facts speak to (a bookend date, a move location) JOIN
//! their claims into one uncertain value — the lattice union, which keeps
//! disjoint claims disjoint instead of fabricating a hull. Each addressed slot
//! cites every fact that fed it, deduped by [`FactId`].
//!
//! The sidecar is re-addressed per projection, never persisted: a [`JsonPath`]
//! is valid only for the shape of the value it was built against. Durable
//! references use [`FactId`]. The map is emit-only — it serializes but does not
//! deserialize, so there is no path that needs a `JsonPath` parser.

mod citations;
mod merge;
mod path;
mod types;

pub use citations::{CitationMap, ProjectedCitation, ProvenanceAtPath};
pub use path::{JsonPath, PathSegment};
pub use types::{
    EntityWithCitations, ProjectedBookend, ProjectedEntity, ProjectedLifetimeEvent, ProjectedName,
    ProjectedRelation,
};

use std::collections::{BTreeMap, BTreeSet};

use crate::facts::drain::{DRAIN_PAGE, drain_id_facts};
use crate::facts::event;
use crate::facts::ids::FactId;
use crate::facts::store::{EntityView, EventView, FactStore, StoredFactOf};

/// Project an entity's `SameEntity` class as an [`EntityWithCitations`].
///
/// Resolves the class, drains every member's backlinks (aggregation is
/// class-level — each source's facts stay on its own id), takes the entity→event
/// hop to reach interior events, and collects them into a [`FactId`]-keyed
/// [`BTreeMap`] (deduped and ordered for deterministic addressing), then merges
/// per field with the no-winners rules. All reads are snapshot-scoped and
/// active-only, so the projection carries no retraction logic of its own.
pub async fn project_entity<S, V>(
    view: &V,
    entity_id: S::EntityId,
) -> Result<EntityWithCitations<S::EntityId, S::EventId, S::ImageId>, S::Error>
where
    S: FactStore,
    V: EntityView<S> + EventView<S> + Sync,
{
    let class = view.entity_class(&entity_id).await?;

    // A fact mentioning two subjects (a relationship, the SameEntity edge, a
    // HasEvent in both the entity and event backlink sets) lands under one
    // FactId key, idempotently; the BTreeMap keeps the set keyed and ordered by
    // FactId so every addressed path is deterministic regardless of member
    // iteration order.
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
    let mut event_ids: BTreeSet<S::EventId> = BTreeSet::new();
    for fact in facts.values() {
        if let Some(event::Fact::HasEvent { event, .. }) = fact.event_fact() {
            event_ids.insert(event.clone());
        }
    }
    for event in &event_ids {
        let event_facts =
            drain_id_facts(|cursor| view.all_facts_about_event(event, cursor, DRAIN_PAGE)).await?;
        facts.extend(event_facts);
    }

    let mut citations = CitationMap::new();
    let mut entity = ProjectedEntity {
        names: Vec::new(),
        construction: None,
        demolition: None,
        events: Vec::new(),
        relations: Vec::new(),
        external_references: Vec::new(),
    };

    let supports = merge::root_supports(&facts);
    citations.insert_supports(JsonPath::root(), supports);

    merge::project_names(&facts, &mut entity.names, &mut citations);
    merge::project_relations(&facts, &mut entity.relations, &mut citations);
    merge::project_external_references(&facts, &mut entity.external_references, &mut citations);
    merge::project_bookends(&facts, &mut entity, &mut citations);
    merge::project_events(&facts, &mut entity.events, &mut citations);

    Ok(EntityWithCitations { entity, citations })
}

#[cfg(test)]
use merge::project_events;

#[cfg(test)]
mod tests;
