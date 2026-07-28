//! The projection's replay folds: whole-fan-in recomputes that start from an
//! accumulator sealed to [`crate::projection`].
//!
//! [`crate::solvers::replay`] re-exports them, and its charter says what earns a
//! place here. Both paths spell `replay::`, so a call site names its cost
//! whichever route it takes.

use std::collections::BTreeSet;

use crate::algebra::monoid::CommutativeMonoid;
use crate::algebra::semiring::{Label, Support};
use crate::conflicts::date_for_role;
use crate::date::UncertainDate;
use crate::grammar::ids::IdScheme;
use crate::lifespan::Lifespan;
use crate::store::{EntityIdOf, EventIdOf, FactStore, ImageIdOf, ImageView};
use crate::submit::{DateRole, StoredFact};

use super::{Entity, project_image};

/// The entity's complete existence answer: its own facts' accumulator joined
/// with the subject-dates its depictions carry.
///
/// A picture of a building standing is evidence that it stood, at the moment the
/// picture portrays. That moment is asserted about the *image*, so reaching it
/// means walking the entity's whole depiction fan-in and projecting each
/// artifact — which is what a verdict costs, and why `replay::` is the name
/// every path here spells.
///
/// The evidence carries real weight: a sighting past a claimed demolition
/// refutes the claim, and the entity reads contested across the stretch the
/// photograph and the removal disagree over.
///
/// `T` is the projection's support: a read projects over [`member_lineage`]
/// while a citation-level pass projects over `fact_lineage`, and the fold reads
/// neither — it starts from the accumulator and walks image ids.
///
/// [`member_lineage`]: super::member_lineage
pub async fn lifespan<S, V, T>(
    entity: &Entity<EntityIdOf<S>, EventIdOf<S>, ImageIdOf<S>, T>,
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
        let projected = project_image::<S, V, _>(view, artifact, |_, _, fact| {
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
    use super::lifespan;

    use chrono::NaiveDate;

    use crate::algebra::semiring::Support;
    use crate::conflicts::fact_lineage;
    use crate::lifespan::ExistenceState::{Absent, Contested, Uncontested};
    use crate::lifespan::Lifespan;
    use crate::projection::project_entity;
    use crate::solvers::inject_derived_bounds;
    use crate::store::FactStore;
    use crate::store::conformance::fixtures::{
        commit_result, construction_started_in, depiction_fact, designated_kind,
        event_point_date_fact, has_event_fact, local_bundle, same_artifact_fact,
        subject_date_before, subject_date_in,
    };
    use crate::store::conformance::{TestError, TestResult};
    use crate::store::memory::{MemoryFactStore, MemoryIds};
    use crate::submit::{Commit, EntityIdx};

    fn nd(y: i32, m: u32, d: u32) -> Result<NaiveDate, TestError> {
        NaiveDate::from_ymd_opt(y, m, d).ok_or_else(|| "invalid date".into())
    }

    /// Commit, project entity 0, and fold its lifespan — the classify-driving
    /// path a served verdict takes, over the real store.
    async fn span_of(
        store: &MemoryFactStore,
        commit: Commit<MemoryIds>,
    ) -> Result<Lifespan, TestError> {
        let result = commit_result(store, commit).await?;
        let id = result.entities.get(&EntityIdx(0)).ok_or("entity 0")?.id;
        let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
        let (_, entity) = project_entity::<MemoryFactStore, _, _>(&mut view, id, fact_lineage)
            .await
            .map_err(|e| format!("{e:?}"))?
            .ok_or("known id should project")?;
        let span = lifespan::<MemoryFactStore, _, _>(&entity, &mut view)
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
        let commit = local_bundle::<MemoryIds>(
            1,
            0,
            1,
            0,
            vec![
                construction_started_in(0, 1900)?,
                subject_date_in(0, 1850)?,
                depiction_fact(0, 0)?,
            ],
        )?;
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
        let commit = local_bundle::<MemoryIds>(
            1,
            0,
            2,
            0,
            vec![
                construction_started_in(0, 1900)?,
                subject_date_in(0, 1850)?,
                depiction_fact(0, 1)?,
                same_artifact_fact(0, 1)?,
            ],
        )?;
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
        let commit = local_bundle::<MemoryIds>(
            1,
            0,
            1,
            0,
            vec![
                construction_started_in(0, 1900)?,
                subject_date_in(0, 1850)?,
                subject_date_before(0, 1900)?,
                depiction_fact(0, 0)?,
            ],
        )?;
        let span = span_of(&store, commit).await?;
        assert_eq!(
            span.classify(nd(1850, 6, 1)?),
            Contested,
            "the closed 1850 sighting survives the disputed open rival, contested by the 1900 build"
        );
        Ok(())
    }

    /// This fold reads the projected `lifespan` accumulator and never re-derives a
    /// bookend from the entity's date slots, so injecting a derived bound into one
    /// leaves the span identical. Re-folding those slots here would let an
    /// inferred premise deny a past no source asserted.
    #[tokio::test]
    async fn lifespan_is_neutral_to_derived_bound_injection() -> TestResult {
        let store = MemoryFactStore::new();
        // A point event's date derives a construction bound, so injection has
        // something to put in the otherwise-empty construction slot.
        let commit = local_bundle::<MemoryIds>(
            1,
            1,
            0,
            0,
            vec![
                has_event_fact(0, 0, designated_kind())?,
                event_point_date_fact(0)?,
            ],
        )?;
        let result = commit_result(&store, commit).await?;
        let id = result.entities.get(&EntityIdx(0)).ok_or("entity 0")?.id;
        let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
        let (_, mut entity) = project_entity::<MemoryFactStore, _, _>(&mut view, id, fact_lineage)
            .await
            .map_err(|e| format!("{e:?}"))?
            .ok_or("known id should project")?;

        let before = lifespan::<MemoryFactStore, _, _>(&entity, &mut view)
            .await
            .map_err(|e| format!("{e:?}"))?;

        inject_derived_bounds::<MemoryIds>(&mut entity);
        assert!(
            !entity.construction.started_at.extent.support.is_zero(),
            "injection populated the construction-start slot, so the property is exercised"
        );

        let after = lifespan::<MemoryFactStore, _, _>(&entity, &mut view)
            .await
            .map_err(|e| format!("{e:?}"))?;

        assert_eq!(
            before, after,
            "the injected premise reaches no slot this fold reads"
        );
        Ok(())
    }
}
