//! Fact-bag scanning: the per-query extractors and the keyed walk behind the
//! `walk_*` stream arms.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use url::Url;

use super::{MemStoredFact, MemoryEntityId, MemoryEventId, MemoryIds, MemoryImageId, ReadCore};
use crate::geo::{
    GeoPoint, QuadLevel, TileId, Viewport, ViewportTilesError, quadkey_of_location,
    quadkey_tile_prefix,
};
use crate::grammar::assertions::{FactualAssertion, JudgmentAssertion};
use crate::grammar::citations::{ExternalReference, Language};
use crate::grammar::ids::FactId;
use crate::grammar::{attribute, depiction, identity, image};
use crate::store::equiv::EquivAdjacency;
use crate::store::pagination;
use crate::store::schema::{
    CELL_DEPTH, CLUSTER_TILE_N, ClassPage, ClusterCell, DepictionPage, PageItem, RankKey,
    cluster_tile_ranges, fold_cluster_cell, normalize_name,
};
use crate::submit::result::{StoredFactualFact, StoredJudgmentFact};
use crate::submit::{LocatedSubject, StoredFact};

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

/// The entity a stored fact places inside `viewport`, if any — the `InViewport` stream
/// extractor over the shared [`StoredFact::located_subject`] peel. A
/// construction bookend yields its own entity; a `MovedToLocation` yields the
/// entity its event's `HasEvent` owns, read from `owners` (see
/// [`ReadCore::event_entity_map`]). Membership is the shared region predicate
/// [`UnresolvedLocation::known_geometry_intersects`], so a circle overlapping the
/// box from outside counts and a symbolic reference never does.
///
/// [`UnresolvedLocation::known_geometry_intersects`]: crate::location::UnresolvedLocation::known_geometry_intersects
pub(super) fn entity_in_viewport(
    fact: &MemStoredFact,
    viewport: &Viewport,
    owners: &BTreeMap<MemoryEventId, MemoryEntityId>,
) -> Option<MemoryEntityId> {
    let (location, subject) = fact.located_subject()?;
    if !location.known_geometry_intersects(viewport) {
        return None;
    }
    match subject {
        LocatedSubject::Entity(entity) => Some(*entity),
        LocatedSubject::Event(event) => owners.get(event).copied(),
        LocatedSubject::Image(_) => None,
    }
}

/// The image a stored `CapturedLocation` fact places inside `viewport`, if any —
/// the image `InViewport` stream extractor, over the same shared pieces as
/// [`entity_in_viewport`].
pub(super) fn image_captured_in_viewport(
    fact: &MemStoredFact,
    viewport: &Viewport,
) -> Option<MemoryImageId> {
    let (location, subject) = fact.located_subject()?;
    if let LocatedSubject::Image(image) = subject
        && location.known_geometry_intersects(viewport)
    {
        Some(*image)
    } else {
        None
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

    /// One [`ClusterCell`] per non-empty tile of `viewport` at `level`, the
    /// memory mirror of the sqlite clustering read. It applies the same
    /// [`cluster_tile_ranges`], the same per-tile top-[`CLUSTER_TILE_N`]
    /// truncation by `(quadkey, fact_id)`, then over that top-N the same
    /// retraction and `SameEntity` resolution, and folds through the same
    /// [`fold_cluster_cell`] — so the two backends return the same cells. The
    /// fold is viewport-free: a fringe tile the viewport only partly covers
    /// still yields a cell. Memory sorts each tile where sqlite uses the index;
    /// the resulting top-N is identical.
    pub(super) fn cluster_entities(
        &self,
        viewport: &Viewport,
        level: QuadLevel,
        rank: RankKey,
    ) -> Result<Vec<ClusterCell<MemoryEntityId>>, ViewportTilesError> {
        // The sole rank; its `(quadkey, fact_id)` order is the truncation and
        // the per-tile min below.
        match rank {
            RankKey::Unranked => {}
        }
        let tiles = cluster_tile_ranges(viewport, level)?;
        let owners = self.event_entity_map();
        // The adjacency is built at the first representative lookup and reused.
        let mut adjacency: Option<EquivAdjacency<MemoryEntityId>> = None;
        // Bucket every clusterable fact into the tile whose Morton range holds
        // its quadkey — one pass over the fact log. The ranges are disjoint and
        // ascending, so a binary search places each fact.
        let mut buckets: Vec<Vec<(i64, FactId, &MemStoredFact)>> = vec![Vec::new(); tiles.len()];
        for (quadkey, fid, fact) in self.clusterable_facts() {
            let Ok(tile) = tiles.binary_search_by(|r| {
                if quadkey < r.lo {
                    std::cmp::Ordering::Greater
                } else if quadkey > r.hi {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Equal
                }
            }) else {
                continue;
            };
            buckets[tile].push((quadkey, fid, fact));
        }

        let mut cells = Vec::new();
        for candidates in buckets {
            if let Some(cell) = self.fold_tile_bucket(candidates, &owners, &mut adjacency) {
                cells.push(cell);
            }
        }
        Ok(cells)
    }

    /// One [`ClusterCell`] per non-empty sub-tile of container `tile`, the memory
    /// mirror of the sqlite per-tile clustering read — the viewport-free fold
    /// whose cell geometry is keyed by `(snapshot, level, x, y)`.
    ///
    /// The container is one contiguous Morton block; its children at
    /// `level + CELL_DEPTH` partition it, each a bucket over
    /// [`quadkey_tile_prefix`]. Each bucket keeps its `(quadkey, fact_id)`-lowest
    /// [`CLUSTER_TILE_N`] candidates before retraction — the same
    /// [`Self::fold_tile_bucket`] pipeline the viewport read folds through — so a
    /// dense low-Morton corner never starves the container's other sub-tiles, and
    /// the two reads report byte-identical cells for a shared tile.
    pub(super) fn cluster_tile_cells(
        &self,
        tile: TileId,
        rank: RankKey,
    ) -> Vec<ClusterCell<MemoryEntityId>> {
        // The sole rank; its `(quadkey, fact_id)` order is the per-bucket
        // truncation and the fold's min.
        match rank {
            RankKey::Unranked => {}
        }
        let container = tile.range();
        let cell_level = QuadLevel::saturating(tile.level().get() + CELL_DEPTH);
        let owners = self.event_entity_map();
        let mut adjacency: Option<EquivAdjacency<MemoryEntityId>> = None;
        // One pass over the fact log: bucket the container's clusterable facts by
        // their sub-tile prefix at `cell_level`.
        let mut buckets: BTreeMap<i64, Vec<(i64, FactId, &MemStoredFact)>> = BTreeMap::new();
        for (quadkey, fid, fact) in self.clusterable_facts() {
            if quadkey < container.lo || quadkey > container.hi {
                continue;
            }
            let bucket = quadkey_tile_prefix(quadkey, cell_level);
            buckets
                .entry(bucket)
                .or_default()
                .push((quadkey, fid, fact));
        }

        let mut cells = Vec::new();
        for candidates in buckets.into_values() {
            if let Some(cell) = self.fold_tile_bucket(candidates, &owners, &mut adjacency) {
                cells.push(cell);
            }
        }
        cells
    }

    /// Every visible located entity/event fact paired with the [`quadkey`] of the
    /// single point it pins — the shared clustering-eligibility filter the
    /// viewport ([`Self::cluster_entities`]) and per-tile
    /// ([`Self::cluster_tile_cells`]) reads both bucket over. Image subjects and
    /// locations pinning no point are dropped here, in one place, so the two
    /// reads can't drift on which facts cluster.
    ///
    /// [`quadkey`]: crate::geo::quadkey
    fn clusterable_facts(&self) -> impl Iterator<Item = (i64, FactId, &MemStoredFact)> {
        self.visible_facts().filter_map(|(fid, fact)| {
            let (location, subject) = fact.located_subject()?;
            match subject {
                LocatedSubject::Entity(_) | LocatedSubject::Event(_) => {}
                LocatedSubject::Image(_) => return None,
            }
            let quadkey = quadkey_of_location(location)?;
            Some((quadkey, fid, fact))
        })
    }

    /// Fold one tile bucket's candidates to at most one [`ClusterCell`]: the
    /// per-tile top-[`CLUSTER_TILE_N`] cut by `(quadkey, fact_id)`, then over
    /// that top-N the retraction drop, the `SameEntity` resolution, and the
    /// shared [`fold_cluster_cell`]. Shared by [`Self::cluster_entities`] and
    /// [`Self::cluster_tile_cells`] so a viewport tile and its stand-alone tile
    /// fold identically — the equality the tiled client leans on. `adjacency` is
    /// the caller's reused equivalence cache, built lazily at the first lookup.
    fn fold_tile_bucket(
        &self,
        mut candidates: Vec<(i64, FactId, &MemStoredFact)>,
        owners: &BTreeMap<MemoryEventId, MemoryEntityId>,
        adjacency: &mut Option<EquivAdjacency<MemoryEntityId>>,
    ) -> Option<ClusterCell<MemoryEntityId>> {
        // The per-tile top-N mirrors the sqlite LIMIT: truncate by
        // (quadkey, fact_id) before retraction.
        candidates.sort_by_key(|(quadkey, fid, _)| (*quadkey, *fid));
        candidates.truncate(CLUSTER_TILE_N);

        let mut survivors: Vec<(i64, FactId, MemoryEntityId, GeoPoint)> = Vec::new();
        for (quadkey, fid, fact) in candidates {
            if self.retracted_by(fid).is_some() {
                continue;
            }
            let Some((location, subject)) = fact.located_subject() else {
                continue;
            };
            let Some(center) = location.point() else {
                continue;
            };
            let entity = match subject {
                LocatedSubject::Entity(entity) => *entity,
                LocatedSubject::Event(event) => match owners.get(event) {
                    Some(entity) => *entity,
                    None => continue,
                },
                LocatedSubject::Image(_) => continue,
            };
            let adjacency = adjacency.get_or_insert_with(|| self.equiv_adjacency(same_entity_edge));
            let representative = adjacency.class_of(entity).representative;
            survivors.push((quadkey, fid, representative, *center));
        }

        fold_cluster_cell(&survivors)
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
