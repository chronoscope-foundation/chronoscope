//! Submit-time matching for declared subjects.
//!
//! [`match_entities`] / [`match_images`] judge each [`Decl::Local`] against
//! the unified view of committed + in-flight state, returning
//! [`MatchOutcome::Matched`] when a single existing subject is identified and
//! [`MatchOutcome::Unmatched`] otherwise. A match is the matcher's identity
//! judgment — submit records it as a `SameEntity` / `SameArtifact` fact in a
//! machine-authored companion commit, citing the anchor facts in
//! [`MatchOutcome::Matched::basis`]. Async for the same reason as the
//! validator in [`pipeline`](super::pipeline): a SQL backend `.await`s its
//! index reads inside the transaction holding the commit.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::{Future, ready};

use futures_util::{TryFutureExt, TryStreamExt};
use url::Url;

use super::{Decl, EntityIdx, ImageIdx, SubmitFact};
use crate::grammar::assertions::FactualAssertion;
use crate::grammar::citations::{ExternalReference, Language};
use crate::grammar::ids::{AnalyzerProcess, AnalyzerVersion, FactId};
use crate::grammar::{attribute, image};
use crate::store::pagination::{PAGE_SIZE, paginate};
use crate::store::schema::{EntityStream, ImageStream, normalize_name};
use crate::store::{ClassWalkPage, EntityIdOf, EntityView, FactStore, ImageIdOf, ImageView};

// ============================================================================
// Analyzer identity
// ============================================================================

/// The submit matcher's process name — the author of its companion commits
/// and the `process` of their derivation citations.
const MATCHER_PROCESS: &str = "submit-matcher";

/// The submit matcher's analyzer identity: [`MATCHER_PROCESS`] at this
/// build's [`BUILD_VERSION`](crate::BUILD_VERSION).
pub fn matcher_identity() -> (AnalyzerProcess, AnalyzerVersion) {
    (
        AnalyzerProcess::new(MATCHER_PROCESS),
        AnalyzerVersion::new(crate::BUILD_VERSION),
    )
}

// ============================================================================
// MatchOutcome
// ============================================================================

/// What the matcher concluded for one declaration: whether it identified a
/// single existing subject the decl should be classed with.
///
/// One entry per [`Decl::Local`]; [`Decl::Existing`] bypasses the matcher.
/// The decl mints a fresh id regardless; the outcome decides whether submit
/// also asserts a sameness judgment against an existing subject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchOutcome<Id> {
    /// Exactly one existing subject identified. Submit records the identity
    /// with `id` as a machine-authored judgment in the companion commit,
    /// citing `basis`.
    Matched {
        /// The existing subject — the class representative the anchors hit.
        id: Id,
        /// The anchor facts that produced the hit, all from one key family
        /// (a decl carrying references never cites name rows).
        basis: BTreeSet<FactId>,
    },
    /// No single existing subject identified, so no sameness judgment.
    /// `candidates` is empty for no match, non-empty when several existing
    /// ids matched the anchors (flowing to
    /// [`ResolutionOrigin::Ambiguous`](super::result::ResolutionOrigin::Ambiguous)).
    Unmatched {
        /// Existing ids that matched the anchors. Empty = no match,
        /// non-empty = ambiguous.
        candidates: Vec<Id>,
    },
}

// ============================================================================
// match_entities / match_images
// ============================================================================

/// Match each [`Decl::Local`] entity declaration against the unified view of
/// committed + in-flight state.
///
/// Exact-match only: a decl's anchors are the [`attribute::Fact::ExternalReference`]
/// values and normalized `(name, language)` pairs its bundle facts carry, and
/// its candidates are the existing entities sharing an anchor, each
/// canonicalised to its `SameEntity` class representative. A decl carrying any
/// external reference draws candidates from reference walks alone — a
/// reference is an authoritative cross-system identity, so a declared but
/// unknown one is positive evidence of a new subject, not a name twin. Names
/// participate only for a decl with no references at all. Exactly one
/// distinct candidate is [`MatchOutcome::Matched`]; zero or several are
/// [`MatchOutcome::Unmatched`], the candidate list feeding
/// [`ResolutionOrigin::Ambiguous`](super::result::ResolutionOrigin::Ambiguous).
/// A decl with no anchor facts is unmatched.
///
/// Returns a `HashMap<EntityIdx, MatchOutcome<EntityId>>` keyed by decl
/// position. [`Decl::Existing`] decls produce no entry; `submit_commit`
/// handles them directly.
///
/// Async so a backend's indexes can `.await` view reads; a failed read
/// surfaces as `Err(S::Error)` rather than reading as "no match", which on a
/// transient backend failure would leave the decl unclassed from the existing
/// subject it should have matched.
pub async fn match_entities<S: FactStore, V: EntityView<S>>(
    decls: &[Decl<EntityIdOf<S>>],
    facts: &BTreeSet<SubmitFact>,
    view: &mut V,
) -> Result<HashMap<EntityIdx, MatchOutcome<EntityIdOf<S>>>, S::Error> {
    // Anchor values keyed by decl position, one pass over the bundle.
    let mut references: HashMap<EntityIdx, BTreeSet<&ExternalReference>> = HashMap::new();
    let mut names: HashMap<EntityIdx, BTreeSet<(String, &Language)>> = HashMap::new();
    for fact in facts {
        let SubmitFact::Factual {
            assertion: FactualAssertion::Attribute { fact },
            ..
        } = fact
        else {
            continue;
        };
        match fact {
            attribute::Fact::Name {
                entity,
                name,
                language,
                ..
            } => {
                // Every NameType anchors — a historical name still names the
                // entity.
                names
                    .entry(*entity)
                    .or_default()
                    .insert((normalize_name(name.as_str()), language));
            }
            attribute::Fact::ExternalReference { entity, reference } => {
                references.entry(*entity).or_default().insert(reference);
            }
            attribute::Fact::Relationship { .. } => {}
        }
    }

    let mut out = HashMap::new();
    for (i, decl) in decls.iter().enumerate() {
        if !matches!(decl, Decl::Local) {
            continue;
        }
        let idx = EntityIdx(i);
        let mut candidates: BTreeMap<EntityIdOf<S>, BTreeSet<FactId>> = BTreeMap::new();
        for reference in references.get(&idx).into_iter().flatten() {
            let stream = EntityStream::ByExternalReference { reference };
            collect_candidates::<S, _, _, _, _>(&mut candidates, &mut *view, |v, cursor| {
                let stream = &stream;
                async move {
                    let page = v.walk_entity_classes(stream, cursor, PAGE_SIZE).await?;
                    Ok((page, v))
                }
            })
            .await?;
        }
        // A declared reference owns the decl's identity evidence even when its
        // walk finds nothing; names weigh in only for a reference-free decl.
        if !references.contains_key(&idx) {
            for (name, language) in names.get(&idx).into_iter().flatten() {
                let stream = EntityStream::ByName { name, language };
                collect_candidates::<S, _, _, _, _>(&mut candidates, &mut *view, |v, cursor| {
                    let stream = &stream;
                    async move {
                        let page = v.walk_entity_classes(stream, cursor, PAGE_SIZE).await?;
                        Ok((page, v))
                    }
                })
                .await?;
            }
        }
        out.insert(idx, outcome_of(candidates));
    }
    Ok(out)
}

/// Match each [`Decl::Local`] image declaration against the unified view of
/// committed + in-flight state.
///
/// The image analogue of [`match_entities`] with a single anchor kind: the
/// exact source `url` of each [`image::Fact::Source`] the decl's bundle facts
/// carry, candidates canonicalised to their `SameArtifact` class
/// representative.
pub async fn match_images<S: FactStore, V: ImageView<S>>(
    decls: &[Decl<ImageIdOf<S>>],
    facts: &BTreeSet<SubmitFact>,
    view: &mut V,
) -> Result<HashMap<ImageIdx, MatchOutcome<ImageIdOf<S>>>, S::Error> {
    let mut urls: HashMap<ImageIdx, BTreeSet<&Url>> = HashMap::new();
    for fact in facts {
        if let SubmitFact::Factual {
            assertion:
                FactualAssertion::Image {
                    fact: image::Fact::Source { image, url },
                },
            ..
        } = fact
        {
            urls.entry(*image).or_default().insert(url);
        }
    }

    let mut out = HashMap::new();
    for (i, decl) in decls.iter().enumerate() {
        if !matches!(decl, Decl::Local) {
            continue;
        }
        let idx = ImageIdx(i);
        let mut candidates: BTreeMap<ImageIdOf<S>, BTreeSet<FactId>> = BTreeMap::new();
        for url in urls.get(&idx).into_iter().flatten() {
            let stream = ImageStream::BySourceUrl { url };
            collect_candidates::<S, _, _, _, _>(&mut candidates, &mut *view, |v, cursor| {
                let stream = &stream;
                async move {
                    let page = v.walk_image_classes(stream, cursor, PAGE_SIZE).await?;
                    Ok((page, v))
                }
            })
            .await?;
        }
        out.insert(idx, outcome_of(candidates));
    }
    Ok(out)
}

/// Drain a keyed class walk to exhaustion, unioning each row's fact id into
/// `candidates` under its representative — the anchor evidence a match cites as
/// its [`MatchOutcome::Matched::basis`]. Hits inside one equivalence class share
/// a representative, so several collapse to one candidate rather than a spurious
/// ambiguity. Across a decl's keyed walks (each reference, each name) the same
/// representative accumulates its fact ids, so the basis is the union of every
/// anchor that reached the class. `fetch` threads the exclusive view through
/// each page read as [`paginate`]'s walk state.
async fn collect_candidates<S, St, Sub, F, Fut>(
    candidates: &mut BTreeMap<Sub, BTreeSet<FactId>>,
    state: St,
    mut fetch: F,
) -> Result<(), S::Error>
where
    S: FactStore,
    Sub: Clone + Ord + Send,
    F: FnMut(St, Option<S::ClassCursor<Sub>>) -> Fut,
    Fut: Future<Output = Result<(ClassWalkPage<S, Sub>, St), S::Error>>,
{
    paginate(state, move |st, cursor| {
        fetch(st, cursor).map_ok(|(page, st)| {
            let (rows, next) = page.into_parts();
            (rows, next, st)
        })
    })
    .try_for_each(|row| {
        candidates
            .entry(row.representative)
            .or_default()
            .insert(row.fact_id);
        ready(Ok(()))
    })
    .await
}

/// Select the [`MatchOutcome`] for one decl's deduped candidate map: exactly
/// one candidate is a match carrying its anchor rows as the basis; zero or
/// several are unmatched, carrying the candidates onward. The `BTreeMap` order
/// keeps the ambiguous candidate list deterministic.
fn outcome_of<Id: Ord>(candidates: BTreeMap<Id, BTreeSet<FactId>>) -> MatchOutcome<Id> {
    let mut iter = candidates.into_iter();
    match (iter.next(), iter.next()) {
        (Some((id, basis)), None) => MatchOutcome::Matched { id, basis },
        (first, second) => MatchOutcome::Unmatched {
            candidates: first
                .into_iter()
                .chain(second)
                .chain(iter)
                .map(|(id, _)| id)
                .collect(),
        },
    }
}
