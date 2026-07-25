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
//! own support, threaded through a [`Semiring`].
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
pub(crate) use merge::citation_of;
pub use provenance::{Citation, Cited, DerivationRule, MemberLineage, Premise, derived};
pub use slot::{FactMap, FactSet, Slot};
pub use types::{
    Bookend, DepictionRecord, Entity, Event, GlueEdge, Image, NameKey, NameRecord, RegionRecord,
    Sameness, connecting_glue, sameness_summary,
};

use std::collections::BTreeMap;
use std::future::Future;
use std::num::NonZeroUsize;

use crate::algebra::monoid::CommutativeMonoid;
use crate::algebra::semiring::{Label, Semiring};
use crate::grammar::assertions::JudgmentAssertion;
use crate::grammar::ids::{FactId, IdScheme};
use crate::store::pagination::PAGE_SIZE;
use crate::store::schema::{DepictionPage, EquivClass, PageItem};
use crate::store::{
    EntityIdOf, EntityView, EventIdOf, EventView, FactStore, ImageIdOf, ImageView, StoredFactOf,
};
use crate::submit::StoredFact;
use crate::typed;

/// The member-aware lineage closure: the fact's citation becomes the singleton
/// atom `{(id, citation)}`, keyed by the source id the fact spoke to. The
/// provenance public callers pass to [`project_entity`] / [`project_image`] for
/// [`MemberLineage`]-typed projections; keeping the id in the atom is what makes
/// [`connecting_glue`] computable. A meta fact contributes no derivation — the
/// additive identity.
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
        Some(citation) => Label::premise(Premise::Fact((id.clone(), citation))),
        None => Label::empty(),
    }
}

/// Drain each subject's backlink walk into the shared `FactId`-keyed map. A
/// fact mentioning several subjects lands under one key idempotently, so the
/// map accumulates the walks' deduplicated union, ordered by `FactId` for a
/// deterministic fold downstream.
///
/// A fold terminal: every walk drains to exhaustion by design, so `paginate`
/// stays the streaming surface. `fetch` follows its contract — the walk state
/// (the `&mut` view reborrow) goes in by value and comes back beside each
/// page — but the resume loop is hand-rolled here because a generic `St`
/// can't be reborrowed per subject.
async fn collect_backlinks<S, St, Sub, Rep, F, Fut>(
    facts: &mut BTreeMap<FactId, StoredFactOf<S>>,
    mut state: St,
    subjects: impl IntoIterator<Item = Sub>,
    mut fetch: F,
) -> Result<(), S::Error>
where
    S: FactStore,
    Sub: Copy,
    F: FnMut(St, Sub, Option<S::Cursor>) -> Fut,
    Fut: Future<
        Output = Result<(Vec<PageItem<StoredFactOf<S>, Rep>>, Option<S::Cursor>, St), S::Error>,
    >,
{
    for subject in subjects {
        let mut cursor = None;
        loop {
            let (rows, next, returned) = fetch(state, subject, cursor).await?;
            state = returned;
            for item in rows {
                facts.insert(item.fact_id, item.fact);
            }
            match next {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
    }
    Ok(())
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
    view: &mut V,
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
    let mut facts = BTreeMap::new();
    collect_backlinks::<S, _, _, _, _, _>(
        &mut facts,
        &mut *view,
        class.members.iter(),
        |v, m, c| async move {
            let page = v.all_facts_about_entity(m, c, PAGE_SIZE).await?;
            let (rows, next) = page.into_parts();
            Ok((rows, next, v))
        },
    )
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
    collect_backlinks::<S, _, _, _, _, _>(
        &mut facts,
        &mut *view,
        reachers.keys(),
        |v, e, c| async move {
            let page = v.all_facts_about_event(e, c, PAGE_SIZE).await?;
            let (rows, next) = page.into_parts();
            Ok((rows, next, v))
        },
    )
    .await?;

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
    view: &mut V,
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
    let mut facts = BTreeMap::new();
    collect_backlinks::<S, _, _, _, _, _>(
        &mut facts,
        &mut *view,
        class.members.iter(),
        |v, m, c| async move {
            let page = v.all_facts_about_image(m, c, PAGE_SIZE).await?;
            let (rows, next) = page.into_parts();
            Ok((rows, next, v))
        },
    )
    .await?;

    if facts.is_empty() {
        return Ok(None);
    }

    let image = merge::project_image_facts(&facts, &class.members, provenance);
    Ok(Some((class, image)))
}

/// The `Cited<DepictionRecord>` a page's depiction rows fold into, keyed by
/// image rep — the same entry shape an [`Entity`]'s `depictions` `FactMap`
/// holds, over the image-scoped member lineage.
type ImageDepiction<S> = Cited<
    DepictionRecord<MemberLineage<ImageIdOf<S>, ImageIdOf<S>>>,
    MemberLineage<ImageIdOf<S>, ImageIdOf<S>>,
>;

/// Assemble one page of an entity's depicting images, without projecting the
/// whole entity.
///
/// [`EntityView::walk_entity_depictions`] pages the depicting images — distinct
/// images, each image's raw depiction facts contiguous under its `SameArtifact`
/// rep. Each image's facts fold through `merge::inject_depiction` +
/// `typed::depiction` into one tile keyed by that rep, so competing depictions
/// of the same artifact merge; two sources disagreeing on a merged artifact's
/// perspective leave that axis unsettled. Region-aware disambiguation of the
/// distinct depictions is deferred. The rows carry their facts, so there is no
/// second store read.
///
/// Returns the typed [`Depiction`](typed::Depiction)s keyed by image rep and the
/// walk's `next_class` resume cursor (`None` once the depictions are exhausted).
pub async fn project_entity_images<S, V>(
    view: &mut V,
    entity: EntityIdOf<S>,
    after: Option<S::ClassCursor<ImageIdOf<S>>>,
    limit: NonZeroUsize,
) -> Result<
    (
        Vec<typed::Depiction<ImageIdOf<S>, ImageIdOf<S>>>,
        Option<S::ClassCursor<ImageIdOf<S>>>,
    ),
    S::Error,
>
where
    S: FactStore,
    V: EntityView<S>,
{
    let DepictionPage { rows, next_class } =
        view.walk_entity_depictions(&entity, after, limit).await?;

    // Group the page's rows by image rep, folding each image's depiction facts
    // into one `Cited<DepictionRecord>` — the per-key merge `Entity::parse`'s
    // `depictions` FactMap runs: values meet, supports sum. The source id the
    // depiction's support cites is the image rep, so the atoms mirror the
    // image-side projection.
    let mut folded: BTreeMap<ImageIdOf<S>, ImageDepiction<S>> = BTreeMap::new();
    for PageItem {
        fact_id,
        fact,
        representative,
    } in rows
    {
        let StoredFact::Judgment(judgment) = &fact else {
            continue;
        };
        let JudgmentAssertion::Depiction { fact: depiction } = &judgment.assertion else {
            continue;
        };
        let support = member_lineage(&fact_id, &representative, &fact);
        let entry = Cited {
            value: merge::inject_depiction(depiction, &support),
            support,
        };
        let merged = match folded.remove(&representative) {
            Some(existing) => Cited {
                value: existing.value.combine(entry.value),
                support: existing.support.plus(entry.support),
            },
            None => entry,
        };
        folded.insert(representative, merged);
    }

    let depictions = folded
        .iter()
        .map(|(image, entry)| typed::depiction(image.clone(), entry))
        .collect();
    Ok((depictions, next_class))
}

#[cfg(test)]
use merge::{event_reachers, project_facts, project_image_facts};

#[cfg(test)]
mod tests;
