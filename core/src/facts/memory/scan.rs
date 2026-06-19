//! Fact-bag scanning: the per-query extractors and the keyed walk behind the
//! `walk_*` stream arms.

use std::collections::HashMap;

use url::Url;

use super::equiv::EquivAdjacency;
use super::{MemStoredFact, MemoryEntityId, MemoryEventId, MemoryImageId, ReadCore};
use crate::facts::assertions::{FactualAssertion, JudgmentAssertion};
use crate::facts::citations::{ExternalReference, Language};
use crate::facts::ids::FactId;
use crate::facts::schema::{FactPage, PageItem, normalize_name};
use crate::facts::submit::StoredFact;
use crate::facts::submit::result::{StoredFactualFact, StoredJudgmentFact};
use crate::facts::{attribute, identity, image};

// Aliases to keep the spellings short.
type MemFactualAssertion = FactualAssertion<MemoryEntityId, MemoryEventId, MemoryImageId>;
type MemJudgmentAssertion = JudgmentAssertion<MemoryEntityId, MemoryEventId, MemoryImageId>;

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

impl ReadCore<'_> {
    /// A page of the visible facts `subject_of` accepts, active-only and
    /// ascending by fact id from the pagination `cursor`, each row carrying
    /// its subject's equivalence-class representative (the component
    /// `edge_of`'s edges induce — see [`Self::equiv_class`]). The scan behind
    /// the keyed `walk_*` stream arms; pagination mirrors
    /// [`Self::facts_about`].
    pub(super) fn walk_matching<S>(
        &self,
        cursor: FactId,
        limit: std::num::NonZeroUsize,
        subject_of: impl Fn(&MemStoredFact) -> Option<S>,
        edge_of: impl Fn(&MemStoredFact) -> Option<(S, S)>,
    ) -> FactPage<MemStoredFact, S>
    where
        S: Copy + Ord + std::hash::Hash,
    {
        let mut items = Vec::new();
        let mut next_cursor = None;
        // The adjacency is built at the first matching row and reused for
        // every representative lookup on the page.
        let mut adjacency: Option<EquivAdjacency<S>> = None;
        // Representative cache: rows sharing a subject resolve its class once.
        let mut representatives: HashMap<S, S> = HashMap::new();
        for (fid, fact) in self.visible_facts() {
            if fid.get() < cursor.get() {
                continue;
            }
            if items.len() == limit.get() {
                next_cursor = Some(fid);
                break;
            }
            let Some(subject) = subject_of(fact) else {
                continue;
            };
            if self.retracted_by(fid).is_some() {
                continue;
            }
            let representative = match representatives.get(&subject) {
                Some(rep) => *rep,
                None => {
                    let adjacency = adjacency.get_or_insert_with(|| self.equiv_adjacency(&edge_of));
                    let rep = adjacency.class_of(subject).representative;
                    representatives.insert(subject, rep);
                    rep
                }
            };
            items.push(PageItem {
                fact_id: fid,
                fact: fact.clone(),
                representative,
            });
        }
        FactPage { items, next_cursor }
    }
}
