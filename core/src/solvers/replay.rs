//! Recompute-from-scratch folds: correctness oracles and rebuild paths, kept off
//! the request path.
//!
//! Everything here scans every fact contributing to an aggregate, at a cost that
//! grows with the entity's fan-in, and has (or will have) a maintained
//! counterpart production reads instead. That pairing is what earns the module: a
//! replay fold is the oracle its counterpart is checked against — the homomorphism
//! law — and the rebuild when a cached entry is evicted.
//!
//! Callers are limited to that role: the homomorphism proptest, cache rebuild and
//! eviction repair, and a background consistency auditor. Today the tests are the
//! whole list — every served verdict comes from the maintained projection, with
//! the partial answer [`lifespan`] describes. Anything else routing through
//! `replay::` is a whole fan-in scan on a live request, so nothing here is
//! re-exported into `solvers` — the explicit path is the signal.

use std::collections::BTreeSet;

use crate::algebra::monoid::CommutativeMonoid;
use crate::algebra::semiring::{Label, Support};
use crate::conflicts::date_for_role;
use crate::date::UncertainDate;
use crate::grammar::ids::IdScheme;
use crate::lifespan::Lifespan;
use crate::projection;
use crate::store::{FactStore, ImageIdOf, ImageView};
use crate::submit::{DateRole, StoredFact};

use super::CitedEntity;

/// Layer the subject-date witnesses a depiction carries onto the entity's
/// projected [`Lifespan`] slot.
///
/// A subject-moment is asserted about the *image*, so the per-fact fold behind
/// that slot — which sees one entity's facts — can never reach it. Reaching it
/// means walking the entity's whole depiction fan-in and projecting each
/// artifact, which is what puts this fold here rather than on the read path.
///
/// This is therefore the complete existence answer and the slot is a partial
/// one, which every read is served from today. Where a depiction outlives a
/// claimed demolition the difference is a wrong verdict, not a narrower one:
/// this fold refutes the claim and reads contested, while a marker reads absent
/// from the claimed removal onward — reporting a building definitely gone while
/// holding a photograph taken after it supposedly came down. Wiring the
/// depiction fan-in into the maintained read is what fixes it.
pub async fn lifespan<S, V>(
    entity: &CitedEntity<S::Ids>,
    view: &mut V,
) -> Result<Lifespan, S::Error>
where
    S: FactStore,
    V: ImageView<S> + Sync,
{
    let mut span = entity.lifespan.clone();

    // A depicting artifact portrays the entity as existing at its subject-moment.
    // Resolve each depicted leaf to its artifact representative so a class is
    // projected once regardless of how many of its scans depict the entity.
    let leaves: Vec<ImageIdOf<S>> = entity.depictions.keys().cloned().collect();
    let reps = view.image_representatives(&leaves).await?;
    let artifacts: BTreeSet<ImageIdOf<S>> = leaves
        .iter()
        .map(|leaf| reps.get(leaf).cloned().unwrap_or_else(|| leaf.clone()))
        .collect();
    for artifact in artifacts {
        // Carry only each fact's subject-date as provenance — the fold reads it
        // per claim, and nothing here consumes the whole stored fact.
        let projected = projection::project_image::<S, V, _>(view, artifact, |_, _, fact| {
            Label::premise(subject_date_of(fact))
        })
        .await?;
        // Each subject-date claim is its own sighting. Disputed claims join to an
        // open interval whose hull affirms no instant, so folding the join alone
        // would drop a closed sighting behind an open rival.
        if let Some((_, image)) = projected {
            for date in image.subject_date.extent.support.atoms().flatten() {
                span = span.combine(Lifespan::witness(date.clone()));
            }
        }
    }

    Ok(span)
}

/// The subject-moment an image fact portrays, if it is a `SubjectDate` claim —
/// [`date_for_role`] filtered to the `ImageSubject` role. Folding each claim as
/// its own witness keeps a closed sighting alive behind a disputed open rival,
/// which their join would swallow.
fn subject_date_of<R: IdScheme>(fact: &StoredFact<R>) -> Option<UncertainDate> {
    date_for_role(fact, |role| matches!(role, DateRole::ImageSubject))
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::NaiveDate;

    use crate::conflicts::fact_lineage;
    use crate::grammar::assertions::{FactualAssertion, JudgmentAssertion};
    use crate::grammar::citations::{JudgmentSource, Justification};
    use crate::grammar::ids::UserId;
    use crate::grammar::{depiction, identity, image};
    use crate::lifespan::ExistenceState::{Absent, Contested, Uncontested};
    use crate::projection::project_entity;
    use crate::solvers::inject_derived_bounds;
    use crate::solvers::tests::{
        TestResult, before, citation, construction_started, fixed_time, point_event_at, year,
    };
    use crate::store::memory::{MemoryFactStore, MemoryIds};
    use crate::submit::{
        Commit, CommitAuthor, Decl, EntityIdx, ImageIdx, SubmitFact, commit_facts,
    };

    fn nd(y: i32, m: u32, d: u32) -> Result<NaiveDate, &'static str> {
        NaiveDate::from_ymd_opt(y, m, d).ok_or("invalid date")
    }

    /// A researcher's judgment source, backing the depiction and same-artifact
    /// judgments the subject-date tests submit.
    fn judgment_source() -> Result<JudgmentSource<ImageIdx>, Box<dyn std::error::Error>> {
        Ok(JudgmentSource::PersonalKnowledge {
            user: UserId::new("test")?,
            justification: Justification::new("same artifact under test")?,
        })
    }

    /// A subject-date fact: image `image_idx` portrays its subject as of `y`.
    fn subject_date(image_idx: usize, y: i32) -> Result<SubmitFact, Box<dyn std::error::Error>> {
        Ok(SubmitFact::Factual {
            assertion: FactualAssertion::Image {
                fact: image::Fact::SubjectDate {
                    image: ImageIdx(image_idx),
                    bound: year(y)?,
                },
            },
            citation: citation("https://example.com/subject")?,
        })
    }

    /// A depiction judgment: entity 0 is depicted by image `image_idx`.
    fn depicts(image_idx: usize) -> Result<SubmitFact, Box<dyn std::error::Error>> {
        Ok(SubmitFact::Judgment {
            assertion: JudgmentAssertion::Depiction {
                fact: depiction::Fact {
                    entity: EntityIdx(0),
                    image: ImageIdx(image_idx),
                    localization: None,
                    perspective: None,
                },
            },
            citation: judgment_source()?,
        })
    }

    /// A same-artifact judgment gluing images `a` and `b` into one artifact class.
    fn same_artifact(a: usize, b: usize) -> Result<SubmitFact, Box<dyn std::error::Error>> {
        Ok(SubmitFact::Judgment {
            assertion: JudgmentAssertion::Identity {
                fact: identity::Fact::same_artifact(ImageIdx(a), ImageIdx(b))?,
            },
            citation: judgment_source()?,
        })
    }

    /// Commit, project entity 0, and fold its lifespan — the classify-driving
    /// path the map slider reads, exercised over the real store.
    async fn span_of(
        store: &MemoryFactStore,
        commit: Commit<MemoryIds>,
    ) -> Result<Lifespan, Box<dyn std::error::Error>> {
        let result = commit_facts(store, commit)
            .await
            .map_err(|e| format!("{e:?}"))?;
        let id = result.entities.get(&EntityIdx(0)).ok_or("entity 0")?.id;
        let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
        let (_, entity) = project_entity::<MemoryFactStore, _, _>(&mut view, id, fact_lineage)
            .await
            .map_err(|e| format!("{e:?}"))?
            .ok_or("known id should project")?;
        let span = lifespan::<MemoryFactStore, _>(&entity, &mut view)
            .await
            .map_err(|e| format!("{e:?}"))?;
        Ok(span)
    }

    /// An image's subject-date is an existence witness: a 1900-built entity
    /// depicted portraying it in 1850 is contested before its construction, so the
    /// hull reaches back to 1850 and the pre-construction span reads contested.
    #[tokio::test]
    async fn subject_date_folds_as_existence_witness() -> TestResult {
        let store = MemoryFactStore::new();
        let commit = Commit::<MemoryIds> {
            author: CommitAuthor::User(UserId::new("test")?),
            recorded_at: fixed_time()?,
            entities: vec![Decl::Local],
            events: Vec::new(),
            images: vec![Decl::Local],
            facts: [
                construction_started(1900)?,
                subject_date(0, 1850)?,
                depicts(0)?,
            ]
            .into_iter()
            .collect(),
        };
        let span = span_of(&store, commit).await?;
        assert_eq!(span.classify(nd(1849, 12, 31)?), Absent);
        assert_eq!(span.classify(nd(1850, 6, 1)?), Contested);
        assert_eq!(span.classify(nd(1900, 6, 1)?), Uncontested);
        Ok(())
    }

    /// A subject-date crosses the depicted image's `SameArtifact` class: asserted
    /// on scan A, the entity depicted via scan B, glued A ≡ B. Projecting B's
    /// artifact class drains scan A too, so A's 1850 subject-date reaches the
    /// entity and the pre-construction span reads contested.
    #[tokio::test]
    async fn subject_date_reaches_entity_across_same_artifact() -> TestResult {
        let store = MemoryFactStore::new();
        let commit = Commit::<MemoryIds> {
            author: CommitAuthor::User(UserId::new("test")?),
            recorded_at: fixed_time()?,
            entities: vec![Decl::Local],
            events: Vec::new(),
            images: vec![Decl::Local, Decl::Local],
            facts: [
                construction_started(1900)?,
                subject_date(0, 1850)?,
                depicts(1)?,
                same_artifact(0, 1)?,
            ]
            .into_iter()
            .collect(),
        };
        let span = span_of(&store, commit).await?;
        assert_eq!(span.classify(nd(1850, 6, 1)?), Contested);
        assert_eq!(span.classify(nd(1900, 6, 1)?), Uncontested);
        Ok(())
    }

    /// A disputed subject-date — one scan portraying its subject at a closed 1850,
    /// another "before 1900" — folds each claim as its own sighting. The two join
    /// to an open "(-∞, 1900]" whose hull affirms no instant, so folding the join
    /// alone would lose the 1850 sighting. Per claim, the closed 1850 survives:
    /// contested by the 1900 construction, it reads contested rather than absent.
    #[tokio::test]
    async fn disputed_open_subject_date_keeps_the_closed_sighting() -> TestResult {
        let store = MemoryFactStore::new();
        let before_1900 = SubmitFact::Factual {
            assertion: FactualAssertion::Image {
                fact: image::Fact::SubjectDate {
                    image: ImageIdx(0),
                    bound: before(1900)?,
                },
            },
            citation: citation("https://example.com/subject-before")?,
        };
        let commit = Commit::<MemoryIds> {
            author: CommitAuthor::User(UserId::new("test")?),
            recorded_at: fixed_time()?,
            entities: vec![Decl::Local],
            events: Vec::new(),
            images: vec![Decl::Local],
            facts: [
                construction_started(1900)?,
                subject_date(0, 1850)?,
                before_1900,
                depicts(0)?,
            ]
            .into_iter()
            .collect(),
        };
        let span = span_of(&store, commit).await?;
        assert_eq!(
            span.classify(nd(1850, 6, 1)?),
            Contested,
            "the closed 1850 sighting survives the disputed open rival, contested by the 1900 build"
        );
        Ok(())
    }

    /// This fold reads the projected `lifespan` slot and never re-derives a
    /// bookend from the entity's date slots, so injecting a derived bound into
    /// one leaves the span identical. Re-folding those slots here would let an
    /// inferred premise deny a past no source asserted.
    #[tokio::test]
    async fn lifespan_is_neutral_to_derived_bound_injection() -> TestResult {
        let store = MemoryFactStore::new();
        let commit = Commit::<MemoryIds> {
            author: CommitAuthor::User(UserId::new("test")?),
            recorded_at: fixed_time()?,
            entities: vec![Decl::Local],
            events: vec![Decl::Local],
            images: Vec::new(),
            // A point event's date derives a construction bound, so injection has
            // something to put in the otherwise-empty construction slot.
            facts: point_event_at(81)?.into_iter().collect(),
        };
        let result = commit_facts(&store, commit)
            .await
            .map_err(|e| format!("{e:?}"))?;
        let id = result.entities.get(&EntityIdx(0)).ok_or("entity 0")?.id;
        let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
        let (_, mut entity) = project_entity::<MemoryFactStore, _, _>(&mut view, id, fact_lineage)
            .await
            .map_err(|e| format!("{e:?}"))?
            .ok_or("known id should project")?;

        let before = lifespan::<MemoryFactStore, _>(&entity, &mut view)
            .await
            .map_err(|e| format!("{e:?}"))?;

        inject_derived_bounds::<MemoryIds>(&mut entity);
        assert!(
            !entity.construction.started_at.extent.support.is_zero(),
            "injection populated the construction-start slot, so the property is exercised"
        );

        let after = lifespan::<MemoryFactStore, _>(&entity, &mut view)
            .await
            .map_err(|e| format!("{e:?}"))?;

        assert_eq!(
            before, after,
            "the injected premise reaches no slot this fold reads"
        );
        Ok(())
    }
}
