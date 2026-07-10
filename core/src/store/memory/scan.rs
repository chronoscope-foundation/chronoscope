//! Fact-bag scanning: the per-query extractors and the keyed walk behind the
//! `walk_*` stream arms.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use url::Url;

use super::{MemStoredFact, MemoryEntityId, MemoryEventId, MemoryIds, MemoryImageId, ReadCore};
use crate::geo::Bbox;
use crate::grammar::assertions::{FactualAssertion, JudgmentAssertion};
use crate::grammar::citations::{ExternalReference, Language};
use crate::grammar::ids::FactId;
use crate::grammar::{attribute, bookend, depiction, event, identity, image};
use crate::store::equiv::EquivAdjacency;
use crate::store::pagination;
use crate::store::schema::{ClassPage, DepictionPage, PageItem, normalize_name};
use crate::submit::StoredFact;
use crate::submit::result::{StoredFactualFact, StoredJudgmentFact};

// Aliases to keep the spellings short.
type MemFactualAssertion = FactualAssertion<MemoryIds>;
type MemJudgmentAssertion = JudgmentAssertion<MemoryIds>;

// The stored-fact extractors below all unwrap the same wrapper layers before
// their per-query predicates diverge, so the unwrapping lives once in
// `factual_assertion` / `judgment_assertion` and a wrapper-shape change lands
// in one place.

/// The factual assertion under a stored fact, when it carries one.
fn factual_assertion(fact: &MemStoredFact) -> Option<&MemFactualAssertion> {
    match fact {
        StoredFact::Factual(StoredFactualFact { assertion, .. }) => Some(assertion),
        StoredFact::Judgment(_) | StoredFact::Meta(_) => None,
    }
}

/// The judgment assertion under a stored fact, when it carries one.
fn judgment_assertion(fact: &MemStoredFact) -> Option<&MemJudgmentAssertion> {
    match fact {
        StoredFact::Judgment(StoredJudgmentFact { assertion, .. }) => Some(assertion),
        StoredFact::Factual(_) | StoredFact::Meta(_) => None,
    }
}

/// The `SameEntity` union edge a stored fact contributes, if any. Callers
/// gate on retraction and snapshot.
pub(super) fn same_entity_edge(fact: &MemStoredFact) -> Option<(MemoryEntityId, MemoryEntityId)> {
    if let JudgmentAssertion::Identity {
        fact: identity::Fact::SameEntity { pair },
    } = judgment_assertion(fact)?
    {
        Some((*pair.a(), *pair.b()))
    } else {
        None
    }
}

/// The `SameArtifact` union edge a stored fact contributes, if any. Callers
/// gate on retraction and snapshot.
pub(super) fn same_artifact_edge(fact: &MemStoredFact) -> Option<(MemoryImageId, MemoryImageId)> {
    if let JudgmentAssertion::Identity {
        fact: identity::Fact::SameArtifact { pair },
    } = judgment_assertion(fact)?
    {
        Some((*pair.a(), *pair.b()))
    } else {
        None
    }
}

/// The entity a stored `Name` fact names, when its normalized name and exact
/// language match the query key. `needle` arrives pre-normalized.
pub(super) fn entity_named(
    fact: &MemStoredFact,
    needle: &str,
    language: &Language,
) -> Option<MemoryEntityId> {
    if let FactualAssertion::Attribute {
        fact:
            attribute::Fact::Name {
                entity,
                name,
                language: l,
                ..
            },
    } = factual_assertion(fact)?
        && normalize_name(name.as_str()) == needle
        && l == language
    {
        Some(*entity)
    } else {
        None
    }
}

/// The entity a stored `ExternalReference` fact names, when its reference
/// equals the query key.
pub(super) fn entity_referenced(
    fact: &MemStoredFact,
    reference: &ExternalReference,
) -> Option<MemoryEntityId> {
    if let FactualAssertion::Attribute {
        fact:
            attribute::Fact::ExternalReference {
                entity,
                reference: r,
            },
    } = factual_assertion(fact)?
        && r == reference
    {
        Some(*entity)
    } else {
        None
    }
}

/// Every entity id a stored fact mentions — the `All`-stream extractor, so a
/// fact touching two entity classes lands under both.
pub(super) fn entity_ids_of(fact: &MemStoredFact) -> Vec<MemoryEntityId> {
    let mut ids = Vec::new();
    fact.for_each_id(&mut |e| ids.push(*e), &mut |_| {}, &mut |_| {});
    ids
}

/// Every image id a stored fact mentions — the image `All`-stream extractor.
pub(super) fn image_ids_of(fact: &MemStoredFact) -> Vec<MemoryImageId> {
    let mut ids = Vec::new();
    fact.for_each_id(&mut |_| {}, &mut |_| {}, &mut |i| ids.push(*i));
    ids
}

/// The image a stored `Source` fact names, when its source URL equals the
/// query key.
pub(super) fn image_sourced_from(fact: &MemStoredFact, url: &Url) -> Option<MemoryImageId> {
    if let FactualAssertion::Image {
        fact: image::Fact::Source { image, url: u },
    } = factual_assertion(fact)?
        && u == url
    {
        Some(*image)
    } else {
        None
    }
}

/// The image a stored `Depiction` fact depicts one of `entity_members` in — the
/// depiction-walk extractor, keyed on the depicted entity's `SameEntity` class.
/// A judgment that isn't a depiction, or a depiction whose entity falls outside
/// the class, contributes nothing.
pub(super) fn depiction_of_entity(
    fact: &MemStoredFact,
    entity_members: &BTreeSet<MemoryEntityId>,
) -> Option<MemoryImageId> {
    if let JudgmentAssertion::Depiction {
        fact: depiction::Fact { entity, image, .. },
    } = judgment_assertion(fact)?
        && entity_members.contains(entity)
    {
        Some(*image)
    } else {
        None
    }
}

/// The entity a stored fact places inside `bbox`, if any — the `InBbox` stream
/// extractor. A construction bookend yields its own entity; a `MovedToLocation`
/// yields the entity its event's `HasEvent` owns, read from `owners` (see
/// [`ReadCore::event_entity_map`]). Only a resolved circle carries a point, so a
/// symbolic or combinator location contributes nothing.
pub(super) fn entity_in_bbox(
    fact: &MemStoredFact,
    bbox: &Bbox,
    owners: &BTreeMap<MemoryEventId, MemoryEntityId>,
) -> Option<MemoryEntityId> {
    match factual_assertion(fact)? {
        FactualAssertion::Construction {
            fact: bookend::ConstructionFact::Location { entity, location },
        } => bbox.contains(location.point()?).then_some(*entity),
        FactualAssertion::Event {
            fact: event::Fact::MovedToLocation { event, location },
        } => bbox
            .contains(location.point()?)
            .then(|| owners.get(event).copied())
            .flatten(),
        _ => None,
    }
}

impl ReadCore<'_> {
    /// Each lifetime event's owning entity, from the active `HasEvent` facts at
    /// this snapshot — the global analogue of the projection's `event_reachers`,
    /// scoped to one walk. A retracted `HasEvent` is no owner edge, so its event
    /// resolves to nothing and a `MovedToLocation` under it stays unattributed.
    pub(super) fn event_entity_map(&self) -> BTreeMap<MemoryEventId, MemoryEntityId> {
        let mut owners = BTreeMap::new();
        for (fid, fact) in self.visible_facts() {
            if self.retracted_by(fid).is_some() {
                continue;
            }
            if let Some((event, entity)) = fact.has_event_owner() {
                owners.insert(*event, *entity);
            }
        }
        owners
    }

    /// The ordered `(representative, fact_id)` index a class walk pages over:
    /// every visible, active fact's subjects resolved to their
    /// equivalence-class representative, keyed `(representative, fact_id)`.
    /// `subjects_of` yields the subjects a fact contributes under the stream;
    /// each resolves to the representative of the component `edge_of`'s edges
    /// induce (see [`Self::equiv_class`]), and the pair enters the set once, so a
    /// fact naming two members of one class lands under a single row and the set
    /// order keeps a class's rows contiguous. The adjacency is built once at the
    /// first membership lookup and every representative resolves from it.
    ///
    /// Shared by [`Self::walk_classes`] and [`Self::walk_depictions`]: they
    /// differ in the predicate they scan with and whether their rows keep the
    /// fact, never in how membership is resolved or ordered.
    fn class_rows<S, I>(
        &self,
        subjects_of: impl Fn(&MemStoredFact) -> I,
        edge_of: impl Fn(&MemStoredFact) -> Option<(S, S)>,
    ) -> BTreeSet<(S, FactId)>
    where
        S: Copy + Ord + std::hash::Hash,
        I: IntoIterator<Item = S>,
    {
        // The adjacency is built at the first membership lookup and reused.
        let mut adjacency: Option<EquivAdjacency<S>> = None;
        // Representative cache: the first subject of a component walks it, then
        // every member is seeded here so the rest resolve without re-walking.
        let mut representatives: HashMap<S, S> = HashMap::new();
        // A set keys the rows by `(representative, fact_id)`, giving the order
        // and folding a fact's same-class subjects to one row.
        let mut rows: BTreeSet<(S, FactId)> = BTreeSet::new();
        for (fid, fact) in self.visible_facts() {
            if self.retracted_by(fid).is_some() {
                continue;
            }
            for subject in subjects_of(fact) {
                let representative = match representatives.get(&subject) {
                    Some(rep) => *rep,
                    None => {
                        let adjacency =
                            adjacency.get_or_insert_with(|| self.equiv_adjacency(&edge_of));
                        let class = adjacency.class_of(subject);
                        let rep = class.representative;
                        // Seed the whole component at once, so the other subjects
                        // in it resolve from the cache instead of re-walking.
                        for m in class.members {
                            representatives.insert(m, rep);
                        }
                        rep
                    }
                };
                rows.insert((representative, fid));
            }
        }
        rows
    }

    /// A page of `(representative, fact_id)` rows ordered by
    /// `(representative, fact_id)`, resuming strictly past `after` (`None` opens
    /// the walk). The scan behind the class `walk_*` stream arms: the index
    /// [`Self::class_rows`] builds, cut by the backend-shared
    /// [`pagination::class_page`].
    pub(super) fn walk_classes<S, I>(
        &self,
        after: Option<(S, FactId)>,
        limit: std::num::NonZeroUsize,
        subjects_of: impl Fn(&MemStoredFact) -> I,
        edge_of: impl Fn(&MemStoredFact) -> Option<(S, S)>,
    ) -> ClassPage<S, (S, FactId)>
    where
        S: Copy + Ord + std::hash::Hash,
        I: IntoIterator<Item = S>,
    {
        let rows = self.class_rows(subjects_of, edge_of);
        pagination::class_page(&rows, after, limit)
    }

    /// A page of the depiction facts depicting `entity_members`, under their
    /// depicted-image `SameArtifact` rep, ordered `(image_rep, fact_id)` and
    /// resuming strictly past `after` (`None` opens the walk). The scan behind
    /// [`walk_entity_depictions`](crate::store::EntityView::walk_entity_depictions):
    /// the index [`Self::class_rows`] builds from the [`depiction_of_entity`]
    /// predicate, cut by the backend-shared [`pagination::grouped_class_page`]
    /// (whole images — `limit` counts distinct image reps), each row carrying
    /// its whole depiction fact.
    pub(super) fn walk_depictions(
        &self,
        entity_members: &BTreeSet<MemoryEntityId>,
        after: Option<(MemoryImageId, FactId)>,
        limit: std::num::NonZeroUsize,
    ) -> DepictionPage<MemStoredFact, MemoryImageId, (MemoryImageId, FactId)> {
        let rows = self.class_rows(
            |fact| depiction_of_entity(fact, entity_members),
            same_artifact_edge,
        );
        let (pairs, next_class) = pagination::grouped_class_page(&rows, after, limit);
        let mut page: Vec<PageItem<MemStoredFact, MemoryImageId>> = Vec::new();
        for (representative, fact_id) in pairs {
            // The row came from a visible, active fact, so its slot is present.
            if let Some(fact) = self.fact_slot(fact_id) {
                page.push(PageItem {
                    fact_id,
                    fact: fact.clone(),
                    representative,
                });
            }
        }
        DepictionPage {
            rows: page,
            next_class,
        }
    }
}
