use super::*;
use chrono::TimeZone;
use url::Url;

use crate::date::{DatePrecision, UncertainDate};
use crate::facts::assertions::FactualAssertion;
use crate::facts::assertions::JudgmentAssertion;
use crate::facts::attribute::{self, NameText, NameType};
use crate::facts::bookend;
use crate::facts::citations::{Excerpt, ExternalReference, ExternalSource, FactualCitation};
use crate::facts::citations::{JudgmentSource, Justification, Language};
use crate::facts::identity;
use crate::facts::ids::UserId;
use crate::facts::lifecycle::{DamageCause, DurationalRole, MoveMethod};
use crate::facts::submit::{
    Commit as SubmitBundle, Decl, EntityIdx, EventIdx, ImageIdx, SubmitFact,
};
use crate::facts::submit::{
    CommitAuthor, DateRole, ImageRole, ResolutionOrigin, StoredFact, SubjectKind, SubmitError,
    commit_facts,
};
use crate::location::{LocationReference, UnresolvedLocation};
use crate::nonempty::NonEmptyVec;

pub(super) type TestResult = Result<(), Box<dyn std::error::Error>>;
pub(super) type TestBundle = SubmitBundle<MemoryEntityId, MemoryEventId, MemoryImageId>;
type SubmitErrorBatch = NonEmptyVec<SubmitError<MemoryEntityId, MemoryEventId, MemoryImageId>>;

/// A page limit large enough to fit every fact the backlink tests submit.
const PAGE_100: std::num::NonZeroUsize = match std::num::NonZeroUsize::new(100) {
    Some(n) => n,
    None => std::num::NonZeroUsize::MIN,
};

/// The batch of `SubmitError`s a rejected submit carries, or a test failure if
/// the error was a `Backend` failure. The single entry point every error-path
/// test uses to inspect the accumulated batch.
fn submit_batch(err: MemSubmitCommitError) -> Result<SubmitErrorBatch, Box<dyn std::error::Error>> {
    match err {
        SubmitCommitError::Submit(errs) => Ok(errs),
        other => Err(format!("expected Submit batch, got {other:?}").into()),
    }
}

// --- helpers ---

/// A fixed UTC timestamp for test fixtures.
pub(super) fn fixed_time() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc
        .with_ymd_and_hms(2024, 1, 1, 12, 0, 0)
        .single()
        .unwrap_or_default()
}

fn sample_citation() -> Result<FactualCitation, Box<dyn std::error::Error>> {
    let url = Url::parse("https://example.com/source")?;
    let source = ExternalSource::Url {
        url,
        published: None,
    };
    let excerpts = vec![Excerpt::new("source-text")?];
    Ok(FactualCitation::new(source, excerpts)?)
}

pub(super) fn name_fact(
    entity_idx: usize,
    name: &str,
) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    name_fact_in_language(entity_idx, name, "en")
}

/// A `Name` fact with an explicit language tag, canonicalized at
/// construction.
pub(super) fn name_fact_in_language(
    entity_idx: usize,
    name: &str,
    language: &str,
) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    let language = Language::new(language)?;
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Attribute {
            fact: attribute::Fact::Name {
                entity: EntityIdx(entity_idx),
                name: NameText::new(name),
                language,
                name_type: NameType::Common,
                valid_from: None,
                valid_to: None,
            },
        },
        citation: sample_citation()?,
    })
}

pub(super) fn construction_started_fact(
    entity_idx: usize,
) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    construction_started_in(entity_idx, 1700)
}

/// A `Construction::Started` fact with a year-precision bound.
pub(super) fn construction_started_in(
    entity_idx: usize,
    year: i32,
) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Construction {
            fact: bookend::Fact::Started {
                entity: EntityIdx(entity_idx),
                bound: year_date(year)?,
            },
        },
        citation: sample_citation()?,
    })
}

/// A `Demolition::Completed` fact with a year-precision bound.
pub(super) fn demolition_completed_in(
    entity_idx: usize,
    year: i32,
) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Demolition {
            fact: bookend::Fact::Completed {
                entity: EntityIdx(entity_idx),
                bound: year_date(year)?,
            },
        },
        citation: sample_citation()?,
    })
}

/// A `Construction::Location` fact placing the entity at a named place.
pub(super) fn construction_location_in(
    entity_idx: usize,
    place: &str,
) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Construction {
            fact: bookend::Fact::Location {
                entity: EntityIdx(entity_idx),
                location: UnresolvedLocation::Reference(LocationReference::NamedPlace {
                    name: place.to_owned(),
                }),
            },
        },
        citation: sample_citation()?,
    })
}

/// An `ExternalReference` attribute fact pointing the entity at a Wikidata
/// QID.
pub(super) fn external_reference_fact(
    entity_idx: usize,
    qid: u64,
) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Attribute {
            fact: attribute::Fact::ExternalReference {
                entity: EntityIdx(entity_idx),
                reference: ExternalReference::Wikidata {
                    qid: crate::ids::WikidataEntityId::new(qid),
                },
            },
        },
        citation: sample_citation()?,
    })
}

/// An `image::Fact::Source` fact sourcing the image from `url`.
pub(super) fn image_source_fact(
    image_idx: usize,
    url: &str,
) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Image {
            fact: crate::facts::image::Fact::Source {
                image: ImageIdx(image_idx),
                url: Url::parse(url)?,
            },
        },
        citation: sample_citation()?,
    })
}

/// A `SameEntity` judgment between two entity indices.
pub(super) fn same_entity_fact(
    a: usize,
    b: usize,
) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    Ok(SubmitFact::Judgment {
        assertion: JudgmentAssertion::Identity {
            fact: identity::Fact::same_entity(EntityIdx(a), EntityIdx(b))?,
        },
        citation: judgment_citation()?,
    })
}

/// An event-touching `PointDate` fact for the given event index.
pub(super) fn event_point_date_fact(
    event_idx: usize,
) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    let bound = UncertainDate::with_precision(
        chrono::NaiveDate::from_ymd_opt(1850, 1, 1).ok_or("date")?,
        DatePrecision::Year,
    )?;
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Event {
            fact: crate::facts::event::Fact::PointDate {
                event: EventIdx(event_idx),
                bound,
            },
        },
        citation: sample_citation()?,
    })
}

/// An event-touching `Description` fact for the given event index.
fn event_description_fact(event_idx: usize) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Event {
            fact: crate::facts::event::Fact::Description {
                event: EventIdx(event_idx),
                text: "an interior event".to_owned(),
            },
        },
        citation: sample_citation()?,
    })
}

/// An image-touching `IsPicture` role-claim fact for the given image index.
fn is_picture_fact(image_idx: usize) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Picture {
            fact: crate::facts::picture::Fact::IsPicture {
                image: ImageIdx(image_idx),
            },
        },
        citation: sample_citation()?,
    })
}

/// An image-touching `CapturedDate` fact for the given image index.
fn captured_date_fact(image_idx: usize) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    let bound = UncertainDate::with_precision(
        chrono::NaiveDate::from_ymd_opt(1850, 1, 1).ok_or("date")?,
        DatePrecision::Year,
    )?;
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Picture {
            fact: crate::facts::picture::Fact::CapturedDate {
                image: ImageIdx(image_idx),
                bound,
            },
        },
        citation: sample_citation()?,
    })
}

pub(super) fn user_author() -> Result<CommitAuthor, Box<dyn std::error::Error>> {
    Ok(CommitAuthor::User(UserId::new("alice")))
}

/// A `RetractCommit` meta-fact targeting `target`. References no indices
/// (the target is a `CommitId`), so a bundle carrying only this fact
/// declares no entities / events / images.
pub(super) fn retract_commit_fact(
    target: crate::facts::ids::CommitId,
) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    Ok(SubmitFact::Meta {
        assertion: crate::facts::assertions::MetaAssertion::RetractCommit {
            target,
            reason: crate::facts::assertions::RetractionReason::FactualError,
        },
        citation: crate::facts::citations::MetaSource::PersonalKnowledge {
            user: UserId::new("alice"),
            justification: Justification::new("The targeted commit is wrong.")?,
        },
    })
}

/// A `RetractFact` meta-fact targeting `target`. Declares no subjects (the
/// target is a `FactId`), mirroring [`retract_commit_fact`].
fn retract_fact(
    target: crate::facts::ids::FactId,
) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    Ok(SubmitFact::Meta {
        assertion: crate::facts::assertions::MetaAssertion::RetractFact {
            target,
            reason: crate::facts::assertions::RetractionReason::FactualError,
        },
        citation: crate::facts::citations::MetaSource::PersonalKnowledge {
            user: UserId::new("alice"),
            justification: Justification::new("The targeted fact is wrong.")?,
        },
    })
}

/// A `SupersedeFact` meta-fact replacing `target` with `replacement`.
fn supersede_fact(
    target: crate::facts::ids::FactId,
    replacement: crate::facts::ids::FactId,
) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    Ok(SubmitFact::Meta {
        assertion: crate::facts::assertions::MetaAssertion::SupersedeFact {
            target,
            replacement,
            reason: crate::facts::assertions::RetractionReason::FactualError,
        },
        citation: crate::facts::citations::MetaSource::PersonalKnowledge {
            user: UserId::new("alice"),
            justification: Justification::new("The targeted fact is superseded.")?,
        },
    })
}

/// Commit one `Name` fact on a fresh entity and return its `SubmitResult`.
async fn commit_name(
    store: &MemoryFactStore,
    name: &str,
) -> Result<MemSubmitResult, Box<dyn std::error::Error>> {
    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, name)?].into_iter().collect(),
    };
    Ok(commit_facts(store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?)
}

/// Commit a single `RetractFact` targeting `target`; `secs` offsets the commit
/// time so repeated retractions of one target hash distinctly. Returns its
/// `SubmitResult`.
async fn commit_retract(
    store: &MemoryFactStore,
    target: crate::facts::ids::FactId,
    secs: i64,
) -> Result<MemSubmitResult, Box<dyn std::error::Error>> {
    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(secs),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [retract_fact(target)?].into_iter().collect(),
    };
    Ok(commit_facts(store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?)
}

#[derive(Debug, Clone, Copy)]
enum ExpectedKind {
    Entity,
    Event,
    Image,
}

async fn assert_idx_out_of_range(
    decl_count: usize,
    bad_idx: usize,
    kind: ExpectedKind,
) -> TestResult {
    let store = MemoryFactStore::new();

    let bundle: TestBundle = match kind {
        ExpectedKind::Entity => SubmitBundle {
            author: user_author()?,
            recorded_at: fixed_time(),
            entities: (0..decl_count).map(|_| Decl::Local).collect(),
            events: Vec::new(),
            images: Vec::new(),
            facts: [name_fact(bad_idx, "out-of-range")?].into_iter().collect(),
        },
        ExpectedKind::Event => SubmitBundle {
            author: user_author()?,
            recorded_at: fixed_time(),
            entities: Vec::new(),
            events: (0..decl_count).map(|_| Decl::Local).collect(),
            images: Vec::new(),
            facts: [SubmitFact::Factual {
                assertion: FactualAssertion::Event {
                    fact: crate::facts::event::Fact::PointDate {
                        event: EventIdx(bad_idx),
                        bound: UncertainDate::with_precision(
                            chrono::NaiveDate::from_ymd_opt(1700, 1, 1).ok_or("date")?,
                            DatePrecision::Year,
                        )?,
                    },
                },
                citation: sample_citation()?,
            }]
            .into_iter()
            .collect(),
        },
        ExpectedKind::Image => SubmitBundle {
            author: user_author()?,
            recorded_at: fixed_time(),
            entities: Vec::new(),
            events: Vec::new(),
            images: (0..decl_count).map(|_| Decl::Local).collect(),
            facts: [SubmitFact::Factual {
                assertion: FactualAssertion::Picture {
                    fact: crate::facts::picture::Fact::IsPicture {
                        image: ImageIdx(bad_idx),
                    },
                },
                citation: sample_citation()?,
            }]
            .into_iter()
            .collect(),
        },
    };

    let err = match commit_facts(&store, bundle).await {
        Ok(_) => return Err("expected SubmitError".into()),
        Err(e) => e,
    };

    let errs = submit_batch(err)?;
    assert_eq!(
        errs.len().get(),
        1,
        "expected exactly one out-of-range error"
    );
    match (kind, errs.first()) {
        (
            ExpectedKind::Entity,
            SubmitError::EntityIdxOutOfRange {
                idx,
                decl_count: got,
            },
        ) => {
            assert_eq!(*idx, bad_idx);
            assert_eq!(*got, decl_count);
        }
        (
            ExpectedKind::Event,
            SubmitError::EventIdxOutOfRange {
                idx,
                decl_count: got,
            },
        ) => {
            assert_eq!(*idx, bad_idx);
            assert_eq!(*got, decl_count);
        }
        (
            ExpectedKind::Image,
            SubmitError::ImageIdxOutOfRange {
                idx,
                decl_count: got,
            },
        ) => {
            assert_eq!(*idx, bad_idx);
            assert_eq!(*got, decl_count);
        }
        (k, other) => {
            return Err(format!("expected {k:?} out-of-range error, got {other:?}").into());
        }
    }
    Ok(())
}

/// Phantom-id rejection kinds; the shared helper dispatches its assertion
/// on this tag.
#[derive(Debug, Clone, Copy)]
enum UnknownKind {
    Entity,
    Event,
    Image,
}

/// Submit a bundle with one `Decl::Existing(phantom)` (a counter-future id
/// unminted in an empty store) and a fact referencing it. Asserts the
/// matching `UnknownExisting*` variant at decl position 0.
async fn assert_unknown_existing(kind: UnknownKind, phantom_counter: u64) -> TestResult {
    let store = MemoryFactStore::new();

    let bundle: TestBundle = match kind {
        UnknownKind::Entity => SubmitBundle {
            author: user_author()?,
            recorded_at: fixed_time(),
            entities: vec![Decl::Existing {
                id: MemoryEntityId(phantom_counter),
            }],
            events: Vec::new(),
            images: Vec::new(),
            facts: [name_fact(0, "phantom-name")?].into_iter().collect(),
        },
        UnknownKind::Event => SubmitBundle {
            author: user_author()?,
            recorded_at: fixed_time(),
            entities: Vec::new(),
            events: vec![Decl::Existing {
                id: MemoryEventId(phantom_counter),
            }],
            images: Vec::new(),
            facts: [SubmitFact::Factual {
                assertion: FactualAssertion::Event {
                    fact: crate::facts::event::Fact::PointDate {
                        event: EventIdx(0),
                        bound: UncertainDate::with_precision(
                            chrono::NaiveDate::from_ymd_opt(1700, 1, 1).ok_or("date")?,
                            DatePrecision::Year,
                        )?,
                    },
                },
                citation: sample_citation()?,
            }]
            .into_iter()
            .collect(),
        },
        UnknownKind::Image => SubmitBundle {
            author: user_author()?,
            recorded_at: fixed_time(),
            entities: Vec::new(),
            events: Vec::new(),
            images: vec![Decl::Existing {
                id: MemoryImageId(phantom_counter),
            }],
            facts: [SubmitFact::Factual {
                assertion: FactualAssertion::Picture {
                    fact: crate::facts::picture::Fact::IsPicture { image: ImageIdx(0) },
                },
                citation: sample_citation()?,
            }]
            .into_iter()
            .collect(),
        },
    };

    let err = match commit_facts(&store, bundle).await {
        Ok(_) => return Err("expected SubmitError".into()),
        Err(e) => e,
    };

    let errs = submit_batch(err)?;
    assert_eq!(
        errs.len().get(),
        1,
        "expected exactly one UnknownExisting* error"
    );
    match (kind, errs.first()) {
        (UnknownKind::Entity, SubmitError::UnknownExistingEntity { decl_position }) => {
            assert_eq!(*decl_position, EntityIdx(0));
        }
        (UnknownKind::Event, SubmitError::UnknownExistingEvent { decl_position }) => {
            assert_eq!(*decl_position, EventIdx(0));
        }
        (UnknownKind::Image, SubmitError::UnknownExistingImage { decl_position }) => {
            assert_eq!(*decl_position, ImageIdx(0));
        }
        (k, other) => {
            return Err(format!("expected {k:?} UnknownExisting* error, got {other:?}").into());
        }
    }
    Ok(())
}

// --- roundtrip & resolution ---

#[tokio::test]
async fn roundtrip_small_commit_through_fact_lookup() -> TestResult {
    let store = MemoryFactStore::new();
    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "Pantheon")?, construction_started_fact(0)?]
            .into_iter()
            .collect(),
    };

    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(result.fact_ids.len(), 2);
    assert!(!result.previously_committed);

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let resolved_entity = result
        .entities
        .get(&EntityIdx(0))
        .ok_or("entity resolution missing")?
        .id;

    let first_id = *result.fact_ids.first().ok_or("no fact ids")?;
    let lookup = view.fact(first_id).await.map_err(|e| format!("{e:?}"))?;
    let FactLookup::Active(stored) = lookup else {
        return Err(format!("expected Active, got {lookup:?}").into());
    };
    let StoredFact::Factual(stored_factual) = stored.as_ref() else {
        return Err("expected factual stored fact".into());
    };
    let FactualAssertion::Attribute {
        fact:
            attribute::Fact::Name {
                entity,
                name,
                name_type,
                ..
            },
    } = &stored_factual.assertion
    else {
        return Err("expected Name attribute".into());
    };
    assert_eq!(entity, &resolved_entity);
    assert_eq!(name.as_str(), "Pantheon");
    assert_eq!(*name_type, NameType::Common);

    Ok(())
}

#[tokio::test]
async fn existing_decl_passes_through_to_supplied_id() -> TestResult {
    let store = MemoryFactStore::new();

    let first: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "first")?].into_iter().collect(),
    };
    let first_result = commit_facts(&store, first)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let minted_entity = first_result
        .entities
        .get(&EntityIdx(0))
        .ok_or("missing")?
        .id;

    let second: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: vec![Decl::Existing { id: minted_entity }],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "second")?].into_iter().collect(),
    };
    let second_result = commit_facts(&store, second)
        .await
        .map_err(|e| format!("{e:?}"))?;

    let resolution = second_result.entities.get(&EntityIdx(0)).ok_or("missing")?;
    assert_eq!(resolution.id, minted_entity);
    assert_eq!(resolution.origin, ResolutionOrigin::DeclaredExisting);

    Ok(())
}

#[tokio::test]
async fn local_decls_mint_distinct_newly_minted_ids() -> TestResult {
    let store = MemoryFactStore::new();

    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local, Decl::Local, Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "a")?, name_fact(1, "b")?, name_fact(2, "c")?]
            .into_iter()
            .collect(),
    };

    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(result.entities.len(), 3);

    let mut seen_ids = std::collections::HashSet::new();
    for idx in 0..3 {
        let resolution = result.entities.get(&EntityIdx(idx)).ok_or("missing")?;
        assert_eq!(resolution.origin, ResolutionOrigin::NewlyMinted);
        assert!(
            seen_ids.insert(resolution.id),
            "minted ids must be distinct; got duplicate {:?}",
            resolution.id
        );
    }

    Ok(())
}

// --- walk conformance ---

/// A commit's entity-touching facts must appear in a `walk_entities(All,
/// ...)` page — the walk-conformance check every backend owes. `#[ignore]`d
/// while the non-keyed `walk_entities` arms return an empty stub; flips
/// green once the `All` arm reads the fact bag.
#[tokio::test]
#[ignore = "the non-keyed walk_entities arms (All / InBbox / InTimeRange / InBboxAndTimeRange) are stubbed to an empty page; only the keyed ByName / ByExternalReference arms read the fact bag. This pins the walk-returns-submitted-facts contract and flips green once the All arm is implemented (a backend stubbing/lying about walk support fails this check)"]
async fn walk_entities_returns_submitted_entity_facts() -> TestResult {
    let store = MemoryFactStore::new();
    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "Pantheon")?, construction_started_fact(0)?]
            .into_iter()
            .collect(),
    };

    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let submitted: std::collections::BTreeSet<FactId> = result.fact_ids.iter().copied().collect();
    assert_eq!(submitted.len(), 2, "expected two submitted facts");

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let page = view
        .walk_entities(&EntityStream::All, FactId::new(0), PAGE_100)
        .await
        .map_err(|e| format!("{e:?}"))?;

    let returned: std::collections::BTreeSet<FactId> =
        page.items.iter().map(|item| item.fact_id).collect();
    assert!(
        submitted.is_subset(&returned),
        "walk_entities(All) must return every submitted entity-touching fact; \
         submitted={submitted:?}, returned={returned:?}"
    );
    Ok(())
}

/// Every fact mentioning an entity must appear in
/// `all_facts_about_entity(entity, ...)`. Commits two entity-touching facts
/// on one entity and asserts both come back. `#[ignore]`d while stubbed;
/// flips green once the backlink index reads the fact bag.
#[tokio::test]
async fn all_facts_about_entity_returns_facts_mentioning_it() -> TestResult {
    let store = MemoryFactStore::new();
    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "Pantheon")?, construction_started_fact(0)?]
            .into_iter()
            .collect(),
    };

    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let submitted: std::collections::BTreeSet<FactId> = result.fact_ids.iter().copied().collect();
    assert_eq!(submitted.len(), 2, "expected two submitted facts");
    let entity = result
        .entities
        .get(&EntityIdx(0))
        .ok_or("missing entity")?
        .id;

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let page = view
        .all_facts_about_entity(&entity, FactId::new(0), PAGE_100)
        .await
        .map_err(|e| format!("{e:?}"))?;

    let returned: std::collections::BTreeSet<FactId> =
        page.items.iter().map(|item| item.fact_id).collect();
    assert!(
        submitted.is_subset(&returned),
        "all_facts_about_entity must return every fact mentioning the entity; \
         submitted={submitted:?}, returned={returned:?}"
    );
    Ok(())
}

/// Event analogue of [`walk_entities_returns_submitted_entity_facts`].
/// `#[ignore]`d while stubbed; flips green once `walk_events` reads the
/// fact bag.
#[tokio::test]
#[ignore = "walk_events is stubbed to an empty page; this pins the walk-returns-submitted-facts contract and flips green once walk_events reads the fact bag"]
async fn walk_events_returns_submitted_event_facts() -> TestResult {
    let store = MemoryFactStore::new();
    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: Vec::new(),
        events: vec![Decl::Local],
        images: Vec::new(),
        facts: [event_point_date_fact(0)?, event_description_fact(0)?]
            .into_iter()
            .collect(),
    };

    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let submitted: std::collections::BTreeSet<FactId> = result.fact_ids.iter().copied().collect();
    assert_eq!(submitted.len(), 2, "expected two submitted facts");

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let page = view
        .walk_events(&EventStream::All, FactId::new(0), PAGE_100)
        .await
        .map_err(|e| format!("{e:?}"))?;

    let returned: std::collections::BTreeSet<FactId> =
        page.items.iter().map(|item| item.fact_id).collect();
    assert!(
        submitted.is_subset(&returned),
        "walk_events(All) must return every submitted event-touching fact; \
         submitted={submitted:?}, returned={returned:?}"
    );
    Ok(())
}

/// Every fact mentioning an event must appear in
/// `all_facts_about_event(event, ...)`. `#[ignore]`d while stubbed; flips
/// green once the backlink index is implemented.
#[tokio::test]
async fn all_facts_about_event_returns_facts_mentioning_it() -> TestResult {
    let store = MemoryFactStore::new();
    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: Vec::new(),
        events: vec![Decl::Local],
        images: Vec::new(),
        facts: [event_point_date_fact(0)?, event_description_fact(0)?]
            .into_iter()
            .collect(),
    };

    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let submitted: std::collections::BTreeSet<FactId> = result.fact_ids.iter().copied().collect();
    assert_eq!(submitted.len(), 2, "expected two submitted facts");
    let event = result.events.get(&EventIdx(0)).ok_or("missing event")?.id;

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let page = view
        .all_facts_about_event(&event, FactId::new(0), PAGE_100)
        .await
        .map_err(|e| format!("{e:?}"))?;

    let returned: std::collections::BTreeSet<FactId> =
        page.items.iter().map(|item| item.fact_id).collect();
    assert!(
        submitted.is_subset(&returned),
        "all_facts_about_event must return every fact mentioning the event; \
         submitted={submitted:?}, returned={returned:?}"
    );
    Ok(())
}

/// Image analogue of [`walk_entities_returns_submitted_entity_facts`].
/// `#[ignore]`d while the non-keyed `walk_images` arms return an empty
/// stub; flips green once the `All` arm reads the fact bag.
#[tokio::test]
#[ignore = "the non-keyed walk_images arms (All / InBbox / InTimeRange / InBboxAndTimeRange) are stubbed to an empty page; only the keyed BySourceUrl arm reads the fact bag. This pins the walk-returns-submitted-facts contract and flips green once the All arm is implemented"]
async fn walk_images_returns_submitted_image_facts() -> TestResult {
    let store = MemoryFactStore::new();
    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: Vec::new(),
        events: Vec::new(),
        images: vec![Decl::Local],
        facts: [is_picture_fact(0)?, captured_date_fact(0)?]
            .into_iter()
            .collect(),
    };

    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let submitted: std::collections::BTreeSet<FactId> = result.fact_ids.iter().copied().collect();
    assert_eq!(submitted.len(), 2, "expected two submitted facts");

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let page = view
        .walk_images(&ImageStream::All, FactId::new(0), PAGE_100)
        .await
        .map_err(|e| format!("{e:?}"))?;

    let returned: std::collections::BTreeSet<FactId> =
        page.items.iter().map(|item| item.fact_id).collect();
    assert!(
        submitted.is_subset(&returned),
        "walk_images(All) must return every submitted image-touching fact; \
         submitted={submitted:?}, returned={returned:?}"
    );
    Ok(())
}

/// Every fact mentioning an image must appear in
/// `all_facts_about_image(image, ...)`. `#[ignore]`d while stubbed; flips
/// green once the backlink index is implemented.
#[tokio::test]
async fn all_facts_about_image_returns_facts_mentioning_it() -> TestResult {
    let store = MemoryFactStore::new();
    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: Vec::new(),
        events: Vec::new(),
        images: vec![Decl::Local],
        facts: [is_picture_fact(0)?, captured_date_fact(0)?]
            .into_iter()
            .collect(),
    };

    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let submitted: std::collections::BTreeSet<FactId> = result.fact_ids.iter().copied().collect();
    assert_eq!(submitted.len(), 2, "expected two submitted facts");
    let image = result.images.get(&ImageIdx(0)).ok_or("missing image")?.id;

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let page = view
        .all_facts_about_image(&image, FactId::new(0), PAGE_100)
        .await
        .map_err(|e| format!("{e:?}"))?;

    let returned: std::collections::BTreeSet<FactId> =
        page.items.iter().map(|item| item.fact_id).collect();
    assert!(
        submitted.is_subset(&returned),
        "all_facts_about_image must return every fact mentioning the image; \
         submitted={submitted:?}, returned={returned:?}"
    );
    Ok(())
}

// --- equivalence-class conformance ---
//
// These exercise the `*_class` / `*_representative` view methods. Each
// commits a `Same*` fact between two freshly-minted ids, so the stored
// fact carries two distinct ids in one class. The event reads are still the
// singleton stubs, so the event tests stay ignored.

/// After a `SameEntity` fact links two ids, `entity_class` of either member
/// must contain both.
#[tokio::test]
async fn entity_class_contains_both_same_entity_members() -> TestResult {
    let store = MemoryFactStore::new();
    let identity_pair = identity::Fact::same_entity(EntityIdx(0), EntityIdx(1))?;
    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local, Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [SubmitFact::Judgment {
            assertion: JudgmentAssertion::Identity {
                fact: identity_pair,
            },
            citation: JudgmentSource::PersonalKnowledge {
                user: UserId::new("alice"),
                justification: Justification::new("These two refer to the same entity.")?,
            },
        }]
        .into_iter()
        .collect(),
    };

    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let a = result.entities.get(&EntityIdx(0)).ok_or("missing a")?.id;
    let b = result.entities.get(&EntityIdx(1)).ok_or("missing b")?.id;
    assert_ne!(a, b, "the two Local decls must mint distinct ids");

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let class = view.entity_class(&a).await.map_err(|e| format!("{e:?}"))?;
    assert!(
        class.members.contains(&a) && class.members.contains(&b),
        "entity_class(a) must contain both equivalence members; got {:?}",
        class.members
    );
    Ok(())
}

/// A `SameEntity`-linked pair must resolve to one representative whichever
/// member is queried, and it must be a class member agreeing with
/// `entity_class(..).representative`.
#[tokio::test]
async fn entity_representative_is_canonical_across_same_entity_members() -> TestResult {
    let store = MemoryFactStore::new();
    let identity_pair = identity::Fact::same_entity(EntityIdx(0), EntityIdx(1))?;
    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local, Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [SubmitFact::Judgment {
            assertion: JudgmentAssertion::Identity {
                fact: identity_pair,
            },
            citation: JudgmentSource::PersonalKnowledge {
                user: UserId::new("alice"),
                justification: Justification::new("These two refer to the same entity.")?,
            },
        }]
        .into_iter()
        .collect(),
    };

    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let a = result.entities.get(&EntityIdx(0)).ok_or("missing a")?.id;
    let b = result.entities.get(&EntityIdx(1)).ok_or("missing b")?.id;
    assert_ne!(a, b, "the two Local decls must mint distinct ids");

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let rep_a = view
        .entity_representative(&a)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let rep_b = view
        .entity_representative(&b)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        rep_a, rep_b,
        "both members of a SameEntity class must share one representative"
    );
    assert!(
        rep_a == a || rep_a == b,
        "the representative must be a member of the class; got {rep_a:?} for {{{a:?}, {b:?}}}"
    );
    let class = view.entity_class(&a).await.map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        class.representative, rep_a,
        "entity_class.representative must agree with entity_representative"
    );
    Ok(())
}

/// Event analogue of [`entity_class_contains_both_same_entity_members`].
/// `#[ignore]`d while stubbed; flips green once the class read unions the
/// equivalence facts.
#[tokio::test]
#[ignore = "event_class is stubbed to the singleton {member}; this pins that a SameEvent-linked pair shares a two-member class and flips green once the union-find read is implemented"]
async fn event_class_contains_both_same_event_members() -> TestResult {
    let store = MemoryFactStore::new();
    let identity_pair = identity::Fact::same_event(EventIdx(0), EventIdx(1))?;
    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: Vec::new(),
        events: vec![Decl::Local, Decl::Local],
        images: Vec::new(),
        facts: [SubmitFact::Judgment {
            assertion: JudgmentAssertion::Identity {
                fact: identity_pair,
            },
            citation: JudgmentSource::PersonalKnowledge {
                user: UserId::new("alice"),
                justification: Justification::new("These two refer to the same event.")?,
            },
        }]
        .into_iter()
        .collect(),
    };

    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let a = result.events.get(&EventIdx(0)).ok_or("missing a")?.id;
    let b = result.events.get(&EventIdx(1)).ok_or("missing b")?.id;
    assert_ne!(a, b, "the two Local decls must mint distinct ids");

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let class = view.event_class(&a).await.map_err(|e| format!("{e:?}"))?;
    assert!(
        class.members.contains(&a) && class.members.contains(&b),
        "event_class(a) must contain both equivalence members; got {:?}",
        class.members
    );
    Ok(())
}

/// Event analogue of
/// [`entity_representative_is_canonical_across_same_entity_members`].
/// `#[ignore]`d while stubbed; flips green once representative selection
/// reads the equivalence facts.
#[tokio::test]
#[ignore = "event_representative is stubbed to return member itself; this pins that both SameEvent members share one canonical representative and flips green once representative selection is implemented"]
async fn event_representative_is_canonical_across_same_event_members() -> TestResult {
    let store = MemoryFactStore::new();
    let identity_pair = identity::Fact::same_event(EventIdx(0), EventIdx(1))?;
    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: Vec::new(),
        events: vec![Decl::Local, Decl::Local],
        images: Vec::new(),
        facts: [SubmitFact::Judgment {
            assertion: JudgmentAssertion::Identity {
                fact: identity_pair,
            },
            citation: JudgmentSource::PersonalKnowledge {
                user: UserId::new("alice"),
                justification: Justification::new("These two refer to the same event.")?,
            },
        }]
        .into_iter()
        .collect(),
    };

    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let a = result.events.get(&EventIdx(0)).ok_or("missing a")?.id;
    let b = result.events.get(&EventIdx(1)).ok_or("missing b")?.id;
    assert_ne!(a, b, "the two Local decls must mint distinct ids");

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let rep_a = view
        .event_representative(&a)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let rep_b = view
        .event_representative(&b)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        rep_a, rep_b,
        "both members of a SameEvent class must share one representative"
    );
    assert!(
        rep_a == a || rep_a == b,
        "the representative must be a member of the class; got {rep_a:?} for {{{a:?}, {b:?}}}"
    );
    let class = view.event_class(&a).await.map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        class.representative, rep_a,
        "event_class.representative must agree with event_representative"
    );
    Ok(())
}

/// Image analogue of [`entity_class_contains_both_same_entity_members`].
#[tokio::test]
async fn image_class_contains_both_same_artifact_members() -> TestResult {
    let store = MemoryFactStore::new();
    let identity_pair = identity::Fact::same_artifact(ImageIdx(0), ImageIdx(1))?;
    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: Vec::new(),
        events: Vec::new(),
        images: vec![Decl::Local, Decl::Local],
        facts: [SubmitFact::Judgment {
            assertion: JudgmentAssertion::Identity {
                fact: identity_pair,
            },
            citation: JudgmentSource::PersonalKnowledge {
                user: UserId::new("alice"),
                justification: Justification::new("These two scans are the same artifact.")?,
            },
        }]
        .into_iter()
        .collect(),
    };

    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let a = result.images.get(&ImageIdx(0)).ok_or("missing a")?.id;
    let b = result.images.get(&ImageIdx(1)).ok_or("missing b")?.id;
    assert_ne!(a, b, "the two Local decls must mint distinct ids");

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let class = view.image_class(&a).await.map_err(|e| format!("{e:?}"))?;
    assert!(
        class.members.contains(&a) && class.members.contains(&b),
        "image_class(a) must contain both equivalence members; got {:?}",
        class.members
    );
    Ok(())
}

/// Image analogue of
/// [`entity_representative_is_canonical_across_same_entity_members`].
#[tokio::test]
async fn image_representative_is_canonical_across_same_artifact_members() -> TestResult {
    let store = MemoryFactStore::new();
    let identity_pair = identity::Fact::same_artifact(ImageIdx(0), ImageIdx(1))?;
    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: Vec::new(),
        events: Vec::new(),
        images: vec![Decl::Local, Decl::Local],
        facts: [SubmitFact::Judgment {
            assertion: JudgmentAssertion::Identity {
                fact: identity_pair,
            },
            citation: JudgmentSource::PersonalKnowledge {
                user: UserId::new("alice"),
                justification: Justification::new("These two scans are the same artifact.")?,
            },
        }]
        .into_iter()
        .collect(),
    };

    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let a = result.images.get(&ImageIdx(0)).ok_or("missing a")?.id;
    let b = result.images.get(&ImageIdx(1)).ok_or("missing b")?.id;
    assert_ne!(a, b, "the two Local decls must mint distinct ids");

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let rep_a = view
        .image_representative(&a)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let rep_b = view
        .image_representative(&b)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        rep_a, rep_b,
        "both members of a SameArtifact class must share one representative"
    );
    assert!(
        rep_a == a || rep_a == b,
        "the representative must be a member of the class; got {rep_a:?} for {{{a:?}, {b:?}}}"
    );
    let class = view.image_class(&a).await.map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        class.representative, rep_a,
        "image_class.representative must agree with image_representative"
    );
    Ok(())
}

// `entity_topological_subgraph` is not pinned here: the grammar has two
// entity-to-entity edge relations — `attribute::Fact::Relationship` and
// `observation::Fact::Spatial` — and which feeds the canonical `Topological`
// walk isn't fixed yet. The two choices yield different pins, so pinning
// either risks asserting the wrong contract.

// --- canonical-form wire boundary ---
//
// Commits are content-addressed over their producer-form bytes, so the wire
// boundary rejects non-canonical values: anything that deserializes
// re-serializes byte-identically.

/// Deserializing a non-NFC name errors, and the error names the canonical
/// form.
#[test]
fn deserializing_non_nfc_name_is_rejected() -> TestResult {
    let result = serde_json::from_str::<NameText>("\"Panthe\\u0301on\"");
    let Err(e) = result else {
        return Err(format!("expected a non-NFC rejection, got {result:?}").into());
    };
    assert!(
        e.to_string().contains("Panth\u{e9}on"),
        "the rejection must name the canonical form; got {e}"
    );
    Ok(())
}

/// Deserializing a non-canonical language tag errors, and the error names
/// the canonical form.
#[test]
fn deserializing_non_canonical_language_tag_is_rejected() -> TestResult {
    let result = serde_json::from_str::<Language>("\"en-us\"");
    let Err(e) = result else {
        return Err(format!("expected a non-canonical rejection, got {result:?}").into());
    };
    assert!(
        e.to_string().contains("en-US"),
        "the rejection must name the canonical form; got {e}"
    );
    Ok(())
}

/// A canonical language tag deserializes and re-serializes byte-identically.
#[test]
fn canonical_language_tag_round_trips_byte_identically() -> TestResult {
    let wire = "\"en-US\"";
    let tag: Language = serde_json::from_str(wire)?;
    assert_eq!(serde_json::to_string(&tag)?, wire);
    Ok(())
}

// --- index-reference error paths ---

#[tokio::test]
async fn fact_referencing_out_of_range_entity_idx_returns_error() -> TestResult {
    assert_idx_out_of_range(3, 5, ExpectedKind::Entity).await
}

#[tokio::test]
async fn fact_referencing_out_of_range_event_idx_returns_error() -> TestResult {
    assert_idx_out_of_range(3, 5, ExpectedKind::Event).await
}

#[tokio::test]
async fn fact_referencing_out_of_range_image_idx_returns_error() -> TestResult {
    assert_idx_out_of_range(3, 5, ExpectedKind::Image).await
}

/// The first invalid index — `idx == decl_count` — is the boundary the
/// range check guards. With three entity decls (valid indices 0..=2), index
/// 3 is rejected with `EntityIdxOutOfRange`. Entity kind alone pins the
/// boundary; the three out-of-range tests above share the dispatch.
#[tokio::test]
async fn entity_idx_at_decl_count_is_first_rejected() -> TestResult {
    assert_idx_out_of_range(3, 3, ExpectedKind::Entity).await
}

/// The last valid index — `idx == decl_count - 1` — is accepted. With three
/// entity decls, index 2 resolves; all three decls are referenced so the
/// only thing under test is the upper bound, not `UnusedDeclaration`.
#[tokio::test]
async fn entity_idx_at_decl_count_minus_one_is_accepted() -> TestResult {
    let store = MemoryFactStore::new();
    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local, Decl::Local, Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [
            name_fact(0, "a")?,
            name_fact(1, "b")?,
            name_fact(2, "last-valid")?,
        ]
        .into_iter()
        .collect(),
    };
    commit_ok(&store, bundle).await
}

// --- unused-declaration error path ---

/// A declared entity that no fact references is rejected with
/// `UnusedDeclaration` naming its position. The bundle pairs the unused decl
/// (position 0) with a referenced one (position 1) so the only complaint is
/// the unreferenced decl.
#[tokio::test]
async fn unreferenced_entity_decl_rejected_as_unused() -> TestResult {
    let store = MemoryFactStore::new();
    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        // Decl 0 is never referenced; decl 1 is.
        entities: vec![Decl::Local, Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(1, "referenced")?].into_iter().collect(),
    };
    let errs = commit_err(&store, bundle).await?;
    assert!(
        errs.iter().any(|e| matches!(
            e,
            SubmitError::UnusedDeclaration {
                kind: SubjectKind::Entity,
                position: 0,
            }
        )),
        "expected UnusedDeclaration for entity decl 0, got {errs:?}"
    );
    Ok(())
}

// --- unknown-existing-id error paths ---

#[tokio::test]
async fn decl_existing_unknown_entity_id_rejected() -> TestResult {
    assert_unknown_existing(UnknownKind::Entity, 999).await
}

#[tokio::test]
async fn decl_existing_unknown_event_id_rejected() -> TestResult {
    assert_unknown_existing(UnknownKind::Event, 999).await
}

#[tokio::test]
async fn decl_existing_unknown_image_id_rejected() -> TestResult {
    assert_unknown_existing(UnknownKind::Image, 999).await
}

// --- substitution-time rejection ---

/// Two `Decl::Existing(same_id)` slots resolving to one persistent entity
/// id, plus an identity fact over the two indices, are rejected. The shared id
/// trips two independent guards that accumulate together: the substitution-time
/// `IdentityEntitySelfEquivalence` (the identity fact collapsed) and the
/// decl-distinctness `DuplicateEntityDecl`, both carrying the shared typed id.
#[tokio::test]
async fn same_entity_resolving_to_one_id_rejected_at_substitution() -> TestResult {
    let store = MemoryFactStore::new();

    let mint: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "real-entity")?].into_iter().collect(),
    };
    let minted = commit_facts(&store, mint)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let entity_id = minted
        .entities
        .get(&EntityIdx(0))
        .ok_or("expected mint")?
        .id;

    let identity_pair = identity::Fact::same_entity(EntityIdx(0), EntityIdx(1))?;
    let user = UserId::new("alice");
    let justification =
        Justification::new("Two existing decls collapse to the same id after resolution.")?;
    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: vec![
            Decl::Existing { id: entity_id },
            Decl::Existing { id: entity_id },
        ],
        events: Vec::new(),
        images: Vec::new(),
        facts: [SubmitFact::Judgment {
            assertion: JudgmentAssertion::Identity {
                fact: identity_pair,
            },
            citation: JudgmentSource::PersonalKnowledge {
                user,
                justification,
            },
        }]
        .into_iter()
        .collect(),
    };

    let err = match commit_facts(&store, bundle).await {
        Ok(_) => return Err("expected IdentityEntitySelfEquivalence rejection".into()),
        Err(e) => e,
    };

    let errs = submit_batch(err)?;
    assert!(
        errs.iter().any(
            |e| matches!(e, SubmitError::IdentityEntitySelfEquivalence { id } if *id == entity_id)
        ),
        "expected IdentityEntitySelfEquivalence on {entity_id:?}, got {errs:?}"
    );
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::DuplicateEntityDecl { id } if *id == entity_id)),
        "expected DuplicateEntityDecl on {entity_id:?}, got {errs:?}"
    );
    Ok(())
}

/// Two `Decl::Existing { id: X }` slots on one minted entity, each referenced by
/// a distinct fact, resolve to the same persistent id and are rejected with
/// `DuplicateEntityDecl` carrying that id. Both decls are referenced, so the
/// rejection is the distinctness guard, not `UnusedDeclaration`.
#[tokio::test]
async fn two_entity_decls_resolving_to_one_id_rejected() -> TestResult {
    let store = MemoryFactStore::new();

    let mint: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "real-entity")?].into_iter().collect(),
    };
    let minted = commit_facts(&store, mint)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let entity_id = minted
        .entities
        .get(&EntityIdx(0))
        .ok_or("expected mint")?
        .id;

    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: vec![
            Decl::Existing { id: entity_id },
            Decl::Existing { id: entity_id },
        ],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "via-decl-0")?, name_fact(1, "via-decl-1")?]
            .into_iter()
            .collect(),
    };
    let errs = commit_err(&store, bundle).await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::DuplicateEntityDecl { id } if *id == entity_id)),
        "got {errs:?}"
    );
    Ok(())
}

// --- retract-commit target existence ---

/// A `RetractCommit` whose target was never recorded is rejected with
/// `CommitNotFound` carrying the id. The id is a valid 64-char hex
/// `CommitId` no commit hashes to in an empty store, so it exercises the
/// `commit_known(...) == false` arm rather than a structural pre-check.
#[tokio::test]
async fn retract_commit_targeting_unrecorded_commit_rejected() -> TestResult {
    let store = MemoryFactStore::new();
    let phantom = crate::facts::ids::CommitId::parse("0".repeat(64))?;

    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [retract_commit_fact(phantom.clone())?]
            .into_iter()
            .collect(),
    };

    let err = match commit_facts(&store, bundle).await {
        Ok(_) => return Err("expected CommitNotFound rejection".into()),
        Err(e) => e,
    };

    let errs = submit_batch(err)?;
    assert_eq!(errs.len().get(), 1);
    let SubmitError::CommitNotFound { id } = errs.first() else {
        return Err(format!("expected CommitNotFound, got {:?}", errs.first()).into());
    };
    assert_eq!(*id, phantom);
    Ok(())
}

/// A `RetractCommit` targeting an already-recorded commit succeeds.
/// Commits a fact-bearing bundle for a real `CommitId`, then retracts it;
/// the `commit_known` check passes because the first commit recorded it.
#[tokio::test]
async fn retract_commit_targeting_recorded_commit_succeeds() -> TestResult {
    let store = MemoryFactStore::new();

    let first: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "to-be-retracted")?].into_iter().collect(),
    };
    let first_result = commit_facts(&store, first)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let target = first_result.commit_id.clone();

    let retraction: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [retract_commit_fact(target)?].into_iter().collect(),
    };
    let retraction_result = commit_facts(&store, retraction)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        retraction_result.fact_ids.len(),
        1,
        "the retraction's meta-fact must be recorded"
    );
    assert!(!retraction_result.previously_committed);
    Ok(())
}

// --- transaction brand ---

/// Positive control for the brand pattern: a tx handle is usable across
/// multiple `submit_commit` calls inside its own closure. The pattern
/// blocks cross-instance misuse, not within-instance re-use.
#[tokio::test]
async fn two_commits_share_one_with_tx_brand() -> TestResult {
    let store = MemoryFactStore::new();

    let bundle1: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "first")?].into_iter().collect(),
    };
    let bundle2: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "second")?].into_iter().collect(),
    };

    let (r1, r2) = store
        .with_tx(|s, tx| {
            Box::pin(async move {
                let r1 = s
                    .submit_commit(tx, bundle1)
                    .await
                    .map_err(|e| format!("{e:?}"))?;
                let r2 = s
                    .submit_commit(tx, bundle2)
                    .await
                    .map_err(|e| format!("{e:?}"))?;
                Ok::<_, String>((r1, r2))
            })
        })
        .await
        .map_err(|e| format!("{e:?}"))??;

    assert_eq!(r1.fact_ids.len(), 1);
    assert_eq!(r2.fact_ids.len(), 1);
    assert_ne!(r1.commit_id, r2.commit_id);
    Ok(())
}

// --- UnionSource internals ---

/// Stage a fact via the [`UnionSource`] + [`Pending`] + [`apply_pending`]
/// path and verify it lands.
///
/// Starts with one committed fact, builds a [`UnionSource`], mints one id of
/// each kind, stages a fact, drains, and applies. Verifies `apply_pending`
/// assigns the staged fact the slot one past the committed facts
/// (`committed.len()`) and advances the counters by the mints.
#[tokio::test]
async fn union_source_mint_push_apply_lands_at_expected_fact_id() -> TestResult {
    // Commit one fact so the store has committed state to union over.
    let store = MemoryFactStore::new();
    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "seed")?].into_iter().collect(),
    };
    let seed_result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(seed_result.fact_ids.len(), 1);

    // Clone the committed fact's body to re-push as the pending fact, avoiding a
    // freshly synthesised StoredFact. Drop the guard before the UnionSource
    // interaction to control re-acquisition.
    let (
        committed_len_before,
        next_entity_before,
        next_event_before,
        next_image_before,
        seed_stored,
    ) = {
        let guard = store.lock_inner().await;
        let seed = guard
            .facts
            .first()
            .ok_or("seed fact missing from inner")?
            .clone();
        (
            guard.facts.len(),
            guard.next_entity_id,
            guard.next_event_id,
            guard.next_image_id,
            seed,
        )
    };

    // The slot the staged fact will occupy: one past the committed facts.
    let expected_fact_id = FactId::new(committed_len_before as u64);

    // Mint one of each kind, stage the seed fact, drain to Pending.
    let pending = {
        let guard = store.lock_inner().await;
        let mut source = UnionSource::from_inner(&guard);

        source.mint_entity();
        source.mint_event();
        source.mint_image();

        source.push_fact(seed_stored.clone());

        // Staged fact is visible at its slot. `with_core` awaits nothing
        // for a `UnionSource` but is async to match the trait.
        let lookup = source
            .with_core(|core| core.fact_at(expected_fact_id))
            .await;
        let FactLookup::Active(_) = lookup else {
            return Err(format!("pending fact must read Active; got {lookup:?}").into());
        };

        source.into_pending()
    }; // guard dropped here

    // Apply the pending payload mutably.
    let commit_id = seed_result.commit_id.clone();
    let assigned = {
        let mut guard = store.lock_inner().await;
        apply_pending(&mut guard, pending, commit_id)
    };

    assert_eq!(assigned.len(), 1);
    assert_eq!(
        assigned.first().copied(),
        Some(expected_fact_id),
        "apply_pending must assign the staged fact its slot's FactId"
    );

    // After apply: facts vec grew by one, counters advanced by the mints.
    {
        let guard = store.lock_inner().await;
        assert_eq!(guard.facts.len(), committed_len_before + 1);
        assert_eq!(guard.next_entity_id, next_entity_before + 1);
        assert_eq!(guard.next_event_id, next_event_before + 1);
        assert_eq!(guard.next_image_id, next_image_before + 1);
    }

    Ok(())
}

// --- self-referential meta rules + retraction visibility ---

/// A `RetractFact` whose target is a fact in the same commit (its own
/// meta-fact id) is rejected with `MetaTargetInSameCommit`.
#[tokio::test]
async fn retract_fact_targeting_same_commit_fact_rejected() -> TestResult {
    let store = MemoryFactStore::new();
    commit_name(&store, "prior").await?;
    // The id this retraction's own meta-fact will take — an in-commit target.
    let in_commit = store.next_fact_id().await.map_err(|e| format!("{e:?}"))?;

    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [retract_fact(in_commit)?].into_iter().collect(),
    };
    let err = match commit_facts(&store, bundle).await {
        Ok(_) => return Err("expected MetaTargetInSameCommit".into()),
        Err(e) => e,
    };
    let errs = submit_batch(err)?;
    assert_eq!(errs.len().get(), 1);
    let SubmitError::MetaTargetInSameCommit { target } = errs.first() else {
        return Err(format!("expected MetaTargetInSameCommit, got {:?}", errs.first()).into());
    };
    assert_eq!(*target, in_commit);
    Ok(())
}

/// A `RetractFact` targeting a prior committed fact is accepted.
#[tokio::test]
async fn retract_fact_targeting_prior_fact_accepted() -> TestResult {
    let store = MemoryFactStore::new();
    let prior = commit_name(&store, "to-retract").await?;
    let target = *prior.fact_ids.first().ok_or("no prior fact id")?;

    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [retract_fact(target)?].into_iter().collect(),
    };
    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        result.fact_ids.len(),
        1,
        "the retraction meta-fact must be recorded"
    );
    Ok(())
}

/// A `SupersedeFact` whose target is in the same commit (with a prior
/// replacement) is rejected with `MetaTargetInSameCommit`.
#[tokio::test]
async fn supersede_fact_targeting_same_commit_fact_rejected() -> TestResult {
    let store = MemoryFactStore::new();
    let prior = commit_name(&store, "replacement").await?;
    let replacement = *prior.fact_ids.first().ok_or("no prior fact id")?;
    let in_commit = store.next_fact_id().await.map_err(|e| format!("{e:?}"))?;

    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [supersede_fact(in_commit, replacement)?]
            .into_iter()
            .collect(),
    };
    let err = match commit_facts(&store, bundle).await {
        Ok(_) => return Err("expected MetaTargetInSameCommit".into()),
        Err(e) => e,
    };
    let errs = submit_batch(err)?;
    assert_eq!(errs.len().get(), 1);
    let SubmitError::MetaTargetInSameCommit { target } = errs.first() else {
        return Err(format!("expected MetaTargetInSameCommit, got {:?}", errs.first()).into());
    };
    assert_eq!(*target, in_commit);
    Ok(())
}

/// A `SupersedeFact` whose target equals its replacement is rejected with
/// `SupersedeReplacementEqualsTarget`, before any target existence lookup.
#[tokio::test]
async fn supersede_fact_with_equal_target_and_replacement_rejected() -> TestResult {
    let store = MemoryFactStore::new();
    let prior = commit_name(&store, "self-target").await?;
    let fact_id = *prior.fact_ids.first().ok_or("no prior fact id")?;

    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [supersede_fact(fact_id, fact_id)?].into_iter().collect(),
    };
    let err = match commit_facts(&store, bundle).await {
        Ok(_) => return Err("expected SupersedeReplacementEqualsTarget".into()),
        Err(e) => e,
    };
    let errs = submit_batch(err)?;
    assert_eq!(errs.len().get(), 1);
    let SubmitError::SupersedeReplacementEqualsTarget { target } = errs.first() else {
        return Err(format!(
            "expected SupersedeReplacementEqualsTarget, got {:?}",
            errs.first()
        )
        .into());
    };
    assert_eq!(*target, fact_id);
    Ok(())
}

/// A retracted fact reads `Active` at a snapshot before the retraction and
/// `Retracted` by it once the retraction is visible.
#[tokio::test]
async fn retract_fact_hides_target_only_after_its_commit() -> TestResult {
    let store = MemoryFactStore::new();
    let original = commit_name(&store, "fact-x").await?;
    let target = *original.fact_ids.first().ok_or("no original fact id")?;

    let retraction_bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [retract_fact(target)?].into_iter().collect(),
    };
    let retraction = commit_facts(&store, retraction_bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let retractor = *retraction.fact_ids.first().ok_or("no retractor fact id")?;

    let before = store.no_later_than(retractor);
    let lookup = before.fact(target).await.map_err(|e| format!("{e:?}"))?;
    let FactLookup::Active(_) = lookup else {
        return Err(format!("expected Active before retraction, got {lookup:?}").into());
    };

    let after = store.now().await.map_err(|e| format!("{e:?}"))?;
    let lookup = after.fact(target).await.map_err(|e| format!("{e:?}"))?;
    let FactLookup::Retracted { by } = lookup else {
        return Err(format!("expected Retracted after retraction, got {lookup:?}").into());
    };
    assert_eq!(by, retractor);
    Ok(())
}

/// A read snapshot classifies a committed fact as `Committed` and an id at or
/// past the snapshot as `Absent`, and never reports `InFlight` — it has no
/// in-flight commit.
#[tokio::test]
async fn read_snapshot_placement_is_committed_or_absent() -> TestResult {
    let store = MemoryFactStore::new();
    let first = commit_name(&store, "alpha").await?;
    let committed = *first.fact_ids.first().ok_or("no committed fact id")?;
    let snapshot = store.next_fact_id().await.map_err(|e| format!("{e:?}"))?;
    let view = store.now().await.map_err(|e| format!("{e:?}"))?;

    assert_eq!(
        view.placement(committed)
            .await
            .map_err(|e| format!("{e:?}"))?,
        FactPlacement::Committed
    );
    // The next id to be minted is past the snapshot — no fact lives there yet.
    assert_eq!(
        view.placement(snapshot)
            .await
            .map_err(|e| format!("{e:?}"))?,
        FactPlacement::Absent
    );
    Ok(())
}

/// A read snapshot whose watermark sits far past the committed fact count
/// reports `Absent`, not `InFlight`, for ids between the committed count and the
/// watermark — a read view has no in-flight commit, so `InFlight` is
/// structurally unreachable.
#[tokio::test]
async fn read_snapshot_placement_never_inflight_past_watermark() -> TestResult {
    let store = MemoryFactStore::new();
    commit_name(&store, "alpha").await?;
    commit_name(&store, "beta").await?;
    let committed_count = store.next_fact_id().await.map_err(|e| format!("{e:?}"))?;
    assert_eq!(committed_count, crate::facts::ids::FactId::new(2));

    // An "everything" view whose watermark is well past the committed count.
    let view = store.no_later_than(crate::facts::ids::FactId::new(1_000));

    // An id between the committed count and the watermark names no fact — the
    // old lower-bound-only `InFlight` branch wrongly reported `InFlight` here.
    assert_eq!(
        view.placement(crate::facts::ids::FactId::new(500))
            .await
            .map_err(|e| format!("{e:?}"))?,
        FactPlacement::Absent
    );
    // An id below the committed count is `Committed`.
    assert_eq!(
        view.placement(crate::facts::ids::FactId::new(0))
            .await
            .map_err(|e| format!("{e:?}"))?,
        FactPlacement::Committed
    );
    Ok(())
}

/// A `RetractCommit` hides every fact of its target commit.
#[tokio::test]
async fn retract_commit_hides_every_fact_of_target() -> TestResult {
    let store = MemoryFactStore::new();
    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [name_fact(0, "Pantheon")?, construction_started_fact(0)?]
            .into_iter()
            .collect(),
    };
    let original = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(original.fact_ids.len(), 2);
    let target_commit = original.commit_id.clone();

    let retraction_bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [retract_commit_fact(target_commit)?].into_iter().collect(),
    };
    let retraction = commit_facts(&store, retraction_bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let retractor = *retraction.fact_ids.first().ok_or("no retractor fact id")?;

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    for fact_id in &original.fact_ids {
        let lookup = view.fact(*fact_id).await.map_err(|e| format!("{e:?}"))?;
        let FactLookup::Retracted { by } = lookup else {
            return Err(format!("expected Retracted for {fact_id:?}, got {lookup:?}").into());
        };
        assert_eq!(by, retractor);
    }
    Ok(())
}

/// A `SupersedeFact` hides its target but leaves the replacement `Active`.
#[tokio::test]
async fn supersede_fact_hides_target_and_keeps_replacement() -> TestResult {
    let store = MemoryFactStore::new();
    let original = commit_name(&store, "old-value").await?;
    let target = *original.fact_ids.first().ok_or("no target fact id")?;
    let replacement_result = commit_name(&store, "new-value").await?;
    let replacement = *replacement_result
        .fact_ids
        .first()
        .ok_or("no replacement fact id")?;

    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(20),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [supersede_fact(target, replacement)?].into_iter().collect(),
    };
    let supersession = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let supersede_id = *supersession
        .fact_ids
        .first()
        .ok_or("no supersede fact id")?;

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let target_lookup = view.fact(target).await.map_err(|e| format!("{e:?}"))?;
    let FactLookup::Retracted { by } = target_lookup else {
        return Err(format!("expected superseded target Retracted, got {target_lookup:?}").into());
    };
    assert_eq!(by, supersede_id);

    let replacement_lookup = view.fact(replacement).await.map_err(|e| format!("{e:?}"))?;
    let FactLookup::Active(_) = replacement_lookup else {
        return Err(format!("expected replacement Active, got {replacement_lookup:?}").into());
    };
    Ok(())
}

/// Retracting a retraction restores the original fact's visibility. F1 is
/// active at a C1-era snapshot, retracted at a C2-era snapshot, and active
/// again at a C3-era snapshot once the retraction is itself retracted.
#[tokio::test]
async fn retraction_of_retraction_restores_visibility() -> TestResult {
    let store = MemoryFactStore::new();
    let c1 = commit_name(&store, "f1").await?;
    let f1 = *c1.fact_ids.first().ok_or("no f1 id")?;

    let c2_bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [retract_fact(f1)?].into_iter().collect(),
    };
    let c2 = commit_facts(&store, c2_bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let r1 = *c2.fact_ids.first().ok_or("no r1 id")?;

    let c3_bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(20),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [retract_fact(r1)?].into_iter().collect(),
    };
    let c3 = commit_facts(&store, c3_bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let r2 = *c3.fact_ids.first().ok_or("no r2 id")?;

    let era1 = store.no_later_than(r1);
    let lookup = era1.fact(f1).await.map_err(|e| format!("{e:?}"))?;
    let FactLookup::Active(_) = lookup else {
        return Err(format!("C1-era: expected Active, got {lookup:?}").into());
    };

    let era2 = store.no_later_than(r2);
    let lookup = era2.fact(f1).await.map_err(|e| format!("{e:?}"))?;
    let FactLookup::Retracted { by } = lookup else {
        return Err(format!("C2-era: expected Retracted, got {lookup:?}").into());
    };
    assert_eq!(by, r1);

    let era3 = store.now().await.map_err(|e| format!("{e:?}"))?;
    let lookup = era3.fact(f1).await.map_err(|e| format!("{e:?}"))?;
    let FactLookup::Active(_) = lookup else {
        return Err(format!("C3-era: expected Active again, got {lookup:?}").into());
    };
    Ok(())
}

/// A `SupersedeFact` naming a never-minted replacement is rejected with
/// `FactNotFound` for the dangling replacement id.
#[tokio::test]
async fn supersede_fact_with_unminted_replacement_rejected() -> TestResult {
    let store = MemoryFactStore::new();
    let prior = commit_name(&store, "supersede-target").await?;
    let target = *prior.fact_ids.first().ok_or("no prior fact id")?;
    let phantom = FactId::new(999_999);

    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [supersede_fact(target, phantom)?].into_iter().collect(),
    };
    let err = match commit_facts(&store, bundle).await {
        Ok(_) => return Err("expected FactNotFound for replacement".into()),
        Err(e) => e,
    };
    let errs = submit_batch(err)?;
    assert_eq!(errs.len().get(), 1);
    let SubmitError::FactNotFound { id } = errs.first() else {
        return Err(format!("expected FactNotFound, got {:?}", errs.first()).into());
    };
    assert_eq!(*id, phantom);
    Ok(())
}

/// When a fact has several retractors and the lowest-id one is itself
/// retracted, `fact()` reports the lowest STILL-effective retractor.
#[tokio::test]
async fn retracted_by_reports_lowest_still_effective_retractor() -> TestResult {
    let store = MemoryFactStore::new();
    let original = commit_name(&store, "multiply-retracted").await?;
    let f = *original.fact_ids.first().ok_or("no original fact id")?;

    // Two independent retractions of F (ra has the lower id).
    let first = commit_retract(&store, f, 10).await?;
    let ra = *first.fact_ids.first().ok_or("no ra id")?;
    let second = commit_retract(&store, f, 20).await?;
    let rb = *second.fact_ids.first().ok_or("no rb id")?;
    // Retract ra, cancelling it.
    commit_retract(&store, ra, 30).await?;

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let lookup = view.fact(f).await.map_err(|e| format!("{e:?}"))?;
    let FactLookup::Retracted { by } = lookup else {
        return Err(format!("expected Retracted, got {lookup:?}").into());
    };
    assert_eq!(
        by, rb,
        "ra is cancelled, so rb is the lowest still-effective retractor"
    );
    Ok(())
}

// --- cluster-rule + accumulation fixtures ---

/// A single-commit bundle of all-`Local` declarations.
pub(super) fn local_bundle(
    entities: usize,
    events: usize,
    images: usize,
    secs: i64,
    facts: Vec<SubmitFact>,
) -> Result<TestBundle, Box<dyn std::error::Error>> {
    Ok(SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(secs),
        entities: vec![Decl::Local; entities],
        events: vec![Decl::Local; events],
        images: vec![Decl::Local; images],
        facts: facts.into_iter().collect(),
    })
}

/// Commit `bundle`, returning its `SubmitResult` or failing the test if it
/// was rejected.
pub(super) async fn commit_result(
    store: &MemoryFactStore,
    bundle: TestBundle,
) -> Result<MemSubmitResult, Box<dyn std::error::Error>> {
    Ok(commit_facts(store, bundle)
        .await
        .map_err(|e| format!("expected the commit to be accepted, got {e:?}"))?)
}

/// Commit `bundle`, failing the test if it was rejected.
async fn commit_ok(store: &MemoryFactStore, bundle: TestBundle) -> TestResult {
    commit_facts(store, bundle)
        .await
        .map_err(|e| format!("expected the commit to be accepted, got {e:?}"))?;
    Ok(())
}

/// Commit `bundle`, returning its rejection batch or failing if it was accepted.
pub(super) async fn commit_err(
    store: &MemoryFactStore,
    bundle: TestBundle,
) -> Result<SubmitErrorBatch, Box<dyn std::error::Error>> {
    match commit_facts(store, bundle).await {
        Ok(_) => Err("expected the commit to be rejected".into()),
        Err(e) => submit_batch(e),
    }
}

fn sample_location() -> UnresolvedLocation {
    UnresolvedLocation::Reference(LocationReference::NamedPlace {
        name: "somewhere".to_owned(),
    })
}

fn year_date(year: i32) -> Result<UncertainDate, Box<dyn std::error::Error>> {
    Ok(UncertainDate::with_precision(
        chrono::NaiveDate::from_ymd_opt(year, 1, 1).ok_or("date")?,
        DatePrecision::Year,
    )?)
}

fn judgment_citation() -> Result<JudgmentSource<ImageIdx>, Box<dyn std::error::Error>> {
    Ok(JudgmentSource::PersonalKnowledge {
        user: UserId::new("alice"),
        justification: Justification::new("a test judgment")?,
    })
}

/// A `Demolition` bookend carrying a location — the rejected shape.
fn demolition_location_fact(entity_idx: usize) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Demolition {
            fact: bookend::Fact::Location {
                entity: EntityIdx(entity_idx),
                location: sample_location(),
            },
        },
        citation: sample_citation()?,
    })
}

/// A `Construction` bookend carrying a location — accepted, unlike demolition.
fn construction_location_fact(entity_idx: usize) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Construction {
            fact: bookend::Fact::Location {
                entity: EntityIdx(entity_idx),
                location: sample_location(),
            },
        },
        citation: sample_citation()?,
    })
}

/// A `Name` fact with optional year-precision validity bounds.
fn name_window_fact(
    entity_idx: usize,
    valid_from: Option<i32>,
    valid_to: Option<i32>,
) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    let language = Language::new("en")?;
    let valid_from = valid_from.map(year_date).transpose()?;
    let valid_to = valid_to.map(year_date).transpose()?;
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Attribute {
            fact: attribute::Fact::Name {
                entity: EntityIdx(entity_idx),
                name: NameText::new("name"),
                language,
                name_type: NameType::Common,
                valid_from,
                valid_to,
            },
        },
        citation: sample_citation()?,
    })
}

fn event_damage_cause_fact(event_idx: usize) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Event {
            fact: crate::facts::event::Fact::DamageCause {
                event: EventIdx(event_idx),
                cause: DamageCause::Fire,
            },
        },
        citation: sample_citation()?,
    })
}

fn event_move_method_fact(event_idx: usize) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Event {
            fact: crate::facts::event::Fact::MoveMethod {
                event: EventIdx(event_idx),
                method: MoveMethod::Whole,
            },
        },
        citation: sample_citation()?,
    })
}

fn event_durational_date_fact(event_idx: usize) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Event {
            fact: crate::facts::event::Fact::DurationalDate {
                event: EventIdx(event_idx),
                role: DurationalRole::Started,
                bound: year_date(1850)?,
            },
        },
        citation: sample_citation()?,
    })
}

/// A composite `IsSubimageOf` fact linking the two image indices.
fn subimage_fact(
    subimage_idx: usize,
    parent_idx: usize,
) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    let region = crate::facts::composites::SubimageRegion::rect(0.0, 0.0, 0.5, 0.5)?;
    Ok(SubmitFact::Judgment {
        assertion: JudgmentAssertion::Composite {
            fact: crate::facts::composites::Fact::IsSubimageOf {
                subimage: ImageIdx(subimage_idx),
                parent: ImageIdx(parent_idx),
                region,
            },
        },
        citation: judgment_citation()?,
    })
}

fn is_map_fact(image_idx: usize) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Map {
            fact: crate::facts::map::Fact::IsMap {
                image: ImageIdx(image_idx),
            },
        },
        citation: sample_citation()?,
    })
}

fn in_picture_fact(
    entity_idx: usize,
    image_idx: usize,
) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    Ok(SubmitFact::Judgment {
        assertion: JudgmentAssertion::Depiction {
            fact: crate::facts::depiction::Fact::InPicture {
                entity: EntityIdx(entity_idx),
                image: ImageIdx(image_idx),
                perspective: crate::facts::depiction::Perspective::Unknown,
                region: None,
            },
        },
        citation: judgment_citation()?,
    })
}

fn on_map_fact(
    entity_idx: usize,
    image_idx: usize,
) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    Ok(SubmitFact::Judgment {
        assertion: JudgmentAssertion::Depiction {
            fact: crate::facts::depiction::Fact::OnMap {
                entity: EntityIdx(entity_idx),
                image: ImageIdx(image_idx),
                geometry: None,
            },
        },
        citation: judgment_citation()?,
    })
}

// --- accumulation ---

/// One bundle violating three independent rules — a demolition location, an
/// inverted name window, and a self-parent subimage — is rejected with all three
/// in a single batch, accumulation rather than first-failure.
#[tokio::test]
async fn multi_rule_violations_accumulate_in_one_batch() -> TestResult {
    let store = MemoryFactStore::new();
    let bundle = local_bundle(
        2,
        0,
        1,
        0,
        vec![
            demolition_location_fact(0)?,
            name_window_fact(1, Some(1900), Some(1800))?,
            subimage_fact(0, 0)?,
        ],
    )?;
    let errs = commit_err(&store, bundle).await?;
    assert_eq!(
        errs.len().get(),
        3,
        "expected three rule violations, got {errs:?}"
    );
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::DemolitionLocation { .. })),
        "missing DemolitionLocation: {errs:?}"
    );
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::NameWindowInverted { .. })),
        "missing NameWindowInverted: {errs:?}"
    );
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::CompositeSelfParent { .. })),
        "missing CompositeSelfParent: {errs:?}"
    );
    Ok(())
}

/// The resolvability check batches every out-of-range index and unknown
/// `Decl::Existing` together and gates the rules out: a rule-violating fact in
/// the same bundle is not reported, because its references can't resolve.
#[tokio::test]
async fn resolvability_gate_batches_and_skips_rules() -> TestResult {
    let store = MemoryFactStore::new();
    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time(),
        entities: vec![Decl::Existing {
            id: MemoryEntityId(999),
        }],
        events: Vec::new(),
        images: vec![Decl::Local],
        facts: [
            name_fact(5, "out-of-range-entity")?,
            is_picture_fact(7)?,
            demolition_location_fact(0)?,
        ]
        .into_iter()
        .collect(),
    };
    let errs = commit_err(&store, bundle).await?;
    assert_eq!(
        errs.len().get(),
        3,
        "expected three resolvability errors only, got {errs:?}"
    );
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::EntityIdxOutOfRange { idx: 5, .. })),
        "missing EntityIdxOutOfRange: {errs:?}"
    );
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::ImageIdxOutOfRange { idx: 7, .. })),
        "missing ImageIdxOutOfRange: {errs:?}"
    );
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::UnknownExistingEntity { .. })),
        "missing UnknownExistingEntity: {errs:?}"
    );
    // The would-be demolition-location and unused-declaration violations only
    // run after resolvability passes, so the early return suppresses them.
    assert!(
        !errs
            .iter()
            .any(|e| matches!(e, SubmitError::DemolitionLocation { .. })),
        "a rule fired despite the resolvability gate: {errs:?}"
    );
    Ok(())
}

// --- backlink reads (active-only, snapshot-scoped) ---

/// `all_facts_about_image` returns only active facts: a retracted fact about
/// the image is excluded.
#[tokio::test]
async fn all_facts_about_image_excludes_retracted() -> TestResult {
    let store = MemoryFactStore::new();
    let c1 = commit_facts(&store, local_bundle(0, 0, 1, 0, vec![is_picture_fact(0)?])?)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let image = c1.images.get(&ImageIdx(0)).ok_or("missing image")?.id;
    let target = *c1.fact_ids.first().ok_or("no fact id")?;

    let retraction: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [retract_fact(target)?].into_iter().collect(),
    };
    commit_facts(&store, retraction)
        .await
        .map_err(|e| format!("{e:?}"))?;

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let page = view
        .all_facts_about_image(&image, FactId::new(0), PAGE_100)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert!(
        !page.items.iter().any(|item| item.fact_id == target),
        "retracted fact must not appear in all_facts_about_image; got {:?}",
        page.items
    );
    Ok(())
}

/// `all_facts_about_image` is snapshot-scoped: a fact about the image committed
/// after the snapshot is absent from a view pinned at it.
#[tokio::test]
async fn all_facts_about_image_respects_snapshot() -> TestResult {
    let store = MemoryFactStore::new();
    let c1 = commit_facts(&store, local_bundle(0, 0, 1, 0, vec![is_picture_fact(0)?])?)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let image = c1.images.get(&ImageIdx(0)).ok_or("missing image")?.id;
    let early = *c1.fact_ids.first().ok_or("no fact id")?;
    let snapshot = store.next_fact_id().await.map_err(|e| format!("{e:?}"))?;

    let c2: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: Vec::new(),
        images: vec![Decl::Existing { id: image }],
        facts: [captured_date_fact(0)?].into_iter().collect(),
    };
    let c2 = commit_facts(&store, c2)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let late = *c2.fact_ids.first().ok_or("no fact id")?;

    let view = store.no_later_than(snapshot);
    let page = view
        .all_facts_about_image(&image, FactId::new(0), PAGE_100)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let returned: std::collections::BTreeSet<FactId> =
        page.items.iter().map(|item| item.fact_id).collect();
    assert!(returned.contains(&early), "pre-snapshot fact must appear");
    assert!(
        !returned.contains(&late),
        "post-snapshot fact must be absent; got {returned:?}"
    );
    Ok(())
}

// --- cluster rules ---

/// A demolition bookend carrying a location is rejected.
#[tokio::test]
async fn demolition_location_rejected() -> TestResult {
    let store = MemoryFactStore::new();
    let errs = commit_err(
        &store,
        local_bundle(1, 0, 0, 0, vec![demolition_location_fact(0)?])?,
    )
    .await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::DemolitionLocation { .. })),
        "got {errs:?}"
    );
    Ok(())
}

/// A construction bookend carrying a location is accepted.
#[tokio::test]
async fn construction_location_accepted() -> TestResult {
    let store = MemoryFactStore::new();
    commit_ok(
        &store,
        local_bundle(1, 0, 0, 0, vec![construction_location_fact(0)?])?,
    )
    .await
}

/// A damage-cause and a move-method on one event have disjoint kind sets, so the
/// commit is rejected.
#[tokio::test]
async fn event_damage_and_move_conflict_rejected() -> TestResult {
    let store = MemoryFactStore::new();
    let errs = commit_err(
        &store,
        local_bundle(
            0,
            1,
            0,
            0,
            vec![event_damage_cause_fact(0)?, event_move_method_fact(0)?],
        )?,
    )
    .await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::EventKindConflict { .. })),
        "got {errs:?}"
    );
    Ok(())
}

/// A damage-cause and a durational date narrow to `{Damaged}` and are accepted.
#[tokio::test]
async fn event_damage_and_durational_accepted() -> TestResult {
    let store = MemoryFactStore::new();
    commit_ok(
        &store,
        local_bundle(
            0,
            1,
            0,
            0,
            vec![event_damage_cause_fact(0)?, event_durational_date_fact(0)?],
        )?,
    )
    .await
}

/// A lone point date (kind-set of size two) is accepted.
#[tokio::test]
async fn event_point_date_alone_accepted() -> TestResult {
    let store = MemoryFactStore::new();
    commit_ok(
        &store,
        local_bundle(0, 1, 0, 0, vec![event_point_date_fact(0)?])?,
    )
    .await
}

/// The kind conflict spans commits: a move-method in C1 and a damage-cause in C2
/// on the same event are rejected, via the event backlink.
#[tokio::test]
async fn event_kind_conflict_across_commits_rejected() -> TestResult {
    let store = MemoryFactStore::new();
    let c1 = commit_facts(
        &store,
        local_bundle(0, 1, 0, 0, vec![event_move_method_fact(0)?])?,
    )
    .await
    .map_err(|e| format!("{e:?}"))?;
    let event = c1.events.get(&EventIdx(0)).ok_or("missing event")?.id;

    let c2: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: vec![Decl::Existing { id: event }],
        images: Vec::new(),
        facts: [event_damage_cause_fact(0)?].into_iter().collect(),
    };
    let errs = commit_err(&store, c2).await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::EventKindConflict { .. })),
        "got {errs:?}"
    );
    Ok(())
}

/// A same-commit retraction of the conflicting pre-commit fact clears the kind
/// conflict: C1 puts a `DamageCause` on event E; C2 retracts it and adds a
/// `MoveMethod` on E. The retraction is visible to the rule read in C2, so the
/// surviving kind set is `{Moved}` and the commit is accepted. Without the
/// pending-retractor overlay the doomed `DamageCause` reads active and the
/// commit is falsely rejected as `EventKindConflict`.
#[tokio::test]
async fn event_kind_conflict_resolved_by_same_commit_retraction_accepted() -> TestResult {
    let store = MemoryFactStore::new();
    let c1 = commit_facts(
        &store,
        local_bundle(0, 1, 0, 0, vec![event_damage_cause_fact(0)?])?,
    )
    .await
    .map_err(|e| format!("{e:?}"))?;
    let event = c1.events.get(&EventIdx(0)).ok_or("missing event")?.id;
    let damage = *c1.fact_ids.first().ok_or("no damage fact id")?;

    let c2: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: vec![Decl::Existing { id: event }],
        images: Vec::new(),
        facts: [retract_fact(damage)?, event_move_method_fact(0)?]
            .into_iter()
            .collect(),
    };
    commit_ok(&store, c2).await
}

/// A same-commit retraction of the only depiction unmasks the gap: C1 records
/// the sole `InPicture{X, I}` depiction; C2 retracts it and adds an
/// `ImageObservation` of X on I. The retraction is visible to the rule read in
/// C2, so the depiction no longer satisfies the pairing and the commit is
/// rejected as `ObservationWithoutDepiction`. Without the pending-retractor
/// overlay the doomed depiction reads active and the commit is falsely accepted.
#[tokio::test]
async fn observation_depiction_retracted_in_same_commit_rejected() -> TestResult {
    let store = MemoryFactStore::new();
    let c1 = commit_facts(
        &store,
        local_bundle(1, 0, 1, 0, vec![in_picture_fact(0, 0)?])?,
    )
    .await
    .map_err(|e| format!("{e:?}"))?;
    let entity = c1.entities.get(&EntityIdx(0)).ok_or("missing entity")?.id;
    let image = c1.images.get(&ImageIdx(0)).ok_or("missing image")?.id;
    let depiction = *c1.fact_ids.first().ok_or("no depiction fact id")?;

    let c2: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: vec![Decl::Existing { id: entity }],
        events: Vec::new(),
        images: vec![Decl::Existing { id: image }],
        facts: [
            retract_fact(depiction)?,
            observation_feature_fact(0, image_observation_citation(0)?)?,
        ]
        .into_iter()
        .collect(),
    };
    let errs = commit_err(&store, c2).await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::ObservationWithoutDepiction { .. })),
        "got {errs:?}"
    );
    Ok(())
}

/// An inverted name window is rejected.
#[tokio::test]
async fn name_window_inverted_rejected() -> TestResult {
    let store = MemoryFactStore::new();
    let errs = commit_err(
        &store,
        local_bundle(
            1,
            0,
            0,
            0,
            vec![name_window_fact(0, Some(1900), Some(1800))?],
        )?,
    )
    .await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::NameWindowInverted { .. })),
        "got {errs:?}"
    );
    Ok(())
}

/// An equal-bound window (earliest == latest year) is accepted.
#[tokio::test]
async fn name_window_equal_accepted() -> TestResult {
    let store = MemoryFactStore::new();
    commit_ok(
        &store,
        local_bundle(
            1,
            0,
            0,
            0,
            vec![name_window_fact(0, Some(1850), Some(1850))?],
        )?,
    )
    .await
}

/// An open upper bound can't prove inversion, so it's accepted.
#[tokio::test]
async fn name_window_open_bound_accepted() -> TestResult {
    let store = MemoryFactStore::new();
    commit_ok(
        &store,
        local_bundle(1, 0, 0, 0, vec![name_window_fact(0, Some(1900), None)?])?,
    )
    .await
}

/// A subimage that is its own parent is rejected.
#[tokio::test]
async fn composite_self_parent_rejected() -> TestResult {
    let store = MemoryFactStore::new();
    let errs = commit_err(
        &store,
        local_bundle(0, 0, 1, 0, vec![subimage_fact(0, 0)?])?,
    )
    .await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::CompositeSelfParent { .. })),
        "got {errs:?}"
    );
    Ok(())
}

/// Distinct subimage and parent are accepted.
#[tokio::test]
async fn composite_distinct_accepted() -> TestResult {
    let store = MemoryFactStore::new();
    commit_ok(
        &store,
        local_bundle(0, 0, 2, 0, vec![subimage_fact(0, 1)?])?,
    )
    .await
}

/// A second parent for the same subimage, arriving in a later commit, is
/// rejected via the image backlink.
#[tokio::test]
async fn composite_multiple_parents_across_commits_rejected() -> TestResult {
    let store = MemoryFactStore::new();
    let c1 = commit_facts(
        &store,
        local_bundle(0, 0, 2, 0, vec![subimage_fact(0, 1)?])?,
    )
    .await
    .map_err(|e| format!("{e:?}"))?;
    let subimage = c1.images.get(&ImageIdx(0)).ok_or("missing subimage")?.id;

    let c2: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: Vec::new(),
        images: vec![Decl::Existing { id: subimage }, Decl::Local],
        facts: [subimage_fact(0, 1)?].into_iter().collect(),
    };
    let errs = commit_err(&store, c2).await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::CompositeMultipleParents { .. })),
        "got {errs:?}"
    );
    Ok(())
}

/// A self-loop edge `{s←s}` isn't a genuine parent, so `{s←s}` alongside `{s←p}`
/// reports only the self-parent error, not a spurious multiple-parents error.
/// `s` has exactly one real parent, `p`.
#[tokio::test]
async fn composite_self_loop_does_not_trip_multiple_parents() -> TestResult {
    let store = MemoryFactStore::new();
    let errs = commit_err(
        &store,
        local_bundle(0, 0, 2, 0, vec![subimage_fact(0, 0)?, subimage_fact(0, 1)?])?,
    )
    .await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::CompositeSelfParent { .. })),
        "expected the self-parent error: {errs:?}"
    );
    assert!(
        !errs
            .iter()
            .any(|e| matches!(e, SubmitError::CompositeMultipleParents { .. })),
        "self-loop must not count as a second parent: {errs:?}"
    );
    Ok(())
}

/// A chain formed across commits (A←X in C1, then X←B in C2) is rejected: X is
/// both a subimage and a parent.
#[tokio::test]
async fn composite_chain_across_commits_rejected() -> TestResult {
    let store = MemoryFactStore::new();
    // C1: A (img 0) is a subimage of X (img 1).
    let c1 = commit_facts(
        &store,
        local_bundle(0, 0, 2, 0, vec![subimage_fact(0, 1)?])?,
    )
    .await
    .map_err(|e| format!("{e:?}"))?;
    let x = c1.images.get(&ImageIdx(1)).ok_or("missing X")?.id;

    // C2: X is a subimage of a fresh B (img 1).
    let c2: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: Vec::new(),
        images: vec![Decl::Existing { id: x }, Decl::Local],
        facts: [subimage_fact(0, 1)?].into_iter().collect(),
    };
    let errs = commit_err(&store, c2).await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::CompositeChain { .. })),
        "got {errs:?}"
    );
    Ok(())
}

/// Role coherence — a capture-date attribute (presupposes picture) on an image
/// claimed `IsMap` is rejected as an `ImageRoleConflict`.
#[tokio::test]
async fn picture_attribute_on_map_rejected() -> TestResult {
    let store = MemoryFactStore::new();
    let errs = commit_err(
        &store,
        local_bundle(0, 0, 1, 0, vec![is_map_fact(0)?, captured_date_fact(0)?])?,
    )
    .await?;
    assert!(
        errs.iter().any(|e| matches!(
            e,
            SubmitError::ImageRoleConflict {
                used_as: ImageRole::Picture,
                claimed: ImageRole::Map,
                ..
            }
        )),
        "got {errs:?}"
    );
    Ok(())
}

/// Role coherence — a capture date on an image with no role-claim is accepted.
#[tokio::test]
async fn captured_date_on_no_role_accepted() -> TestResult {
    let store = MemoryFactStore::new();
    commit_ok(
        &store,
        local_bundle(0, 0, 1, 0, vec![captured_date_fact(0)?])?,
    )
    .await
}

/// Role coherence — two role *claims* don't conflict: `IsPicture` alongside
/// `IsMap` is accepted (claim-vs-claim disagreement surfaces at projection,
/// not submit).
#[tokio::test]
async fn is_picture_on_map_accepted() -> TestResult {
    let store = MemoryFactStore::new();
    commit_ok(
        &store,
        local_bundle(0, 0, 1, 0, vec![is_map_fact(0)?, is_picture_fact(0)?])?,
    )
    .await
}

/// Role coherence — an in-picture depiction (presupposes picture) of an image
/// claimed `IsMap` is rejected as an `ImageRoleConflict`.
#[tokio::test]
async fn in_picture_on_map_rejected() -> TestResult {
    let store = MemoryFactStore::new();
    let errs = commit_err(
        &store,
        local_bundle(1, 0, 1, 0, vec![is_map_fact(0)?, in_picture_fact(0, 0)?])?,
    )
    .await?;
    assert!(
        errs.iter().any(|e| matches!(
            e,
            SubmitError::ImageRoleConflict {
                used_as: ImageRole::Picture,
                claimed: ImageRole::Map,
                ..
            }
        )),
        "got {errs:?}"
    );
    Ok(())
}

/// Role coherence — an in-picture depiction of an image with no role-claim is
/// accepted.
#[tokio::test]
async fn in_picture_on_no_role_accepted() -> TestResult {
    let store = MemoryFactStore::new();
    commit_ok(
        &store,
        local_bundle(1, 0, 1, 0, vec![in_picture_fact(0, 0)?])?,
    )
    .await
}

/// Role coherence — an on-map depiction (presupposes map) of an image claimed
/// `IsPicture` is rejected as an `ImageRoleConflict`.
#[tokio::test]
async fn on_map_on_picture_rejected() -> TestResult {
    let store = MemoryFactStore::new();
    let errs = commit_err(
        &store,
        local_bundle(1, 0, 1, 0, vec![is_picture_fact(0)?, on_map_fact(0, 0)?])?,
    )
    .await?;
    assert!(
        errs.iter().any(|e| matches!(
            e,
            SubmitError::ImageRoleConflict {
                used_as: ImageRole::Map,
                claimed: ImageRole::Picture,
                ..
            }
        )),
        "got {errs:?}"
    );
    Ok(())
}

// --- observation -> depiction pairing ---

/// An `ImageObservation` citation pointing at the given image index.
fn image_observation_citation(
    image_idx: usize,
) -> Result<JudgmentSource<ImageIdx>, Box<dyn std::error::Error>> {
    Ok(JudgmentSource::ImageObservation {
        image: ImageIdx(image_idx),
        region: None,
        observer: crate::facts::citations::Observer::User {
            user: UserId::new("alice"),
            justification: None,
        },
    })
}

/// An `External`-cited judgment source (carries no observed image).
fn external_judgment_citation() -> Result<JudgmentSource<ImageIdx>, Box<dyn std::error::Error>> {
    Ok(JudgmentSource::External {
        source: ExternalSource::Url {
            url: Url::parse("https://example.com/observed")?,
            published: None,
        },
    })
}

/// A feature observation on `entity_idx`, cited by `citation`.
fn observation_feature_fact(
    entity_idx: usize,
    citation: JudgmentSource<ImageIdx>,
) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    Ok(SubmitFact::Judgment {
        assertion: JudgmentAssertion::Observation {
            fact: crate::facts::observation::Fact::Feature {
                entity: EntityIdx(entity_idx),
                feature: crate::facts::features::Feature::StoryCount { stories: 2 },
            },
        },
        citation,
    })
}

/// An image-observation of an entity with no paired depiction is rejected.
#[tokio::test]
async fn observation_without_depiction_rejected() -> TestResult {
    let store = MemoryFactStore::new();
    let bundle = local_bundle(
        1,
        0,
        1,
        0,
        vec![observation_feature_fact(0, image_observation_citation(0)?)?],
    )?;
    let errs = commit_err(&store, bundle).await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::ObservationWithoutDepiction { .. })),
        "got {errs:?}"
    );
    Ok(())
}

/// A depiction of the entity on the observed image, in the same commit,
/// satisfies the pairing.
#[tokio::test]
async fn observation_with_same_commit_depiction_accepted() -> TestResult {
    let store = MemoryFactStore::new();
    let bundle = local_bundle(
        1,
        0,
        1,
        0,
        vec![
            observation_feature_fact(0, image_observation_citation(0)?)?,
            in_picture_fact(0, 0)?,
        ],
    )?;
    commit_ok(&store, bundle).await
}

/// A depiction in a prior commit satisfies the pairing, via the image backlink.
#[tokio::test]
async fn observation_with_prior_commit_depiction_accepted() -> TestResult {
    let store = MemoryFactStore::new();
    let c1 = commit_facts(
        &store,
        local_bundle(1, 0, 1, 0, vec![in_picture_fact(0, 0)?])?,
    )
    .await
    .map_err(|e| format!("{e:?}"))?;
    let entity = c1.entities.get(&EntityIdx(0)).ok_or("missing entity")?.id;
    let image = c1.images.get(&ImageIdx(0)).ok_or("missing image")?.id;

    let c2: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: vec![Decl::Existing { id: entity }],
        events: Vec::new(),
        images: vec![Decl::Existing { id: image }],
        facts: [observation_feature_fact(0, image_observation_citation(0)?)?]
            .into_iter()
            .collect(),
    };
    commit_ok(&store, c2).await
}

/// An observation cited by `External` carries no observed image, so the pairing
/// requirement doesn't apply.
#[tokio::test]
async fn observation_cited_external_accepted() -> TestResult {
    let store = MemoryFactStore::new();
    let bundle = local_bundle(
        1,
        0,
        0,
        0,
        vec![observation_feature_fact(0, external_judgment_citation()?)?],
    )?;
    commit_ok(&store, bundle).await
}

// --- single-interval stored-date rule ---

/// A two-interval disjunction ("1920 or 1940"), built by joining two disjoint
/// year claims — the read-side shape a stored fact may not carry.
fn disjunctive_date() -> Result<UncertainDate, Box<dyn std::error::Error>> {
    let a = year_date(1920)?;
    let b = year_date(1940)?;
    let joined = a.join(&b);
    assert_eq!(joined.intervals().len(), 2, "expected a real disjunction");
    Ok(joined)
}

/// A `Construction::Started` bookend carrying an arbitrary date — the
/// fact-payload host for the single-interval rule.
fn started_with_date(
    entity_idx: usize,
    bound: UncertainDate,
) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Construction {
            fact: bookend::Fact::Started {
                entity: EntityIdx(entity_idx),
                bound,
            },
        },
        citation: sample_citation()?,
    })
}

/// An `External`-cited judgment whose citation date is `published` — the
/// non-`Factual` citation host (`JudgmentSource::External`). A feature
/// observation cited externally trips no other rule.
fn observation_external_published(
    entity_idx: usize,
    published: Option<UncertainDate>,
) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    let citation = JudgmentSource::External {
        source: ExternalSource::Url {
            url: Url::parse("https://example.com/observed")?,
            published,
        },
    };
    observation_feature_fact(entity_idx, citation)
}

/// A bookend carrying a single interval is accepted (the storable shape).
#[tokio::test]
async fn single_interval_bookend_accepted() -> TestResult {
    let store = MemoryFactStore::new();
    commit_ok(
        &store,
        local_bundle(1, 0, 0, 0, vec![started_with_date(0, year_date(1900)?)?])?,
    )
    .await
}

/// A bookend carrying a disjunction is rejected at the fact-payload host.
#[tokio::test]
async fn disjunctive_bookend_date_rejected() -> TestResult {
    let store = MemoryFactStore::new();
    let errs = commit_err(
        &store,
        local_bundle(1, 0, 0, 0, vec![started_with_date(0, disjunctive_date()?)?])?,
    )
    .await?;
    assert!(
        errs.iter().any(|e| matches!(
            e,
            SubmitError::NonSingleIntervalDate {
                role: DateRole::BookendBound
            }
        )),
        "got {errs:?}"
    );
    Ok(())
}

/// A bookend carrying the empty date (⊥) is rejected — ⊥ is no honest claim.
#[tokio::test]
async fn empty_bookend_date_rejected() -> TestResult {
    let store = MemoryFactStore::new();
    let errs = commit_err(
        &store,
        local_bundle(
            1,
            0,
            0,
            0,
            vec![started_with_date(0, UncertainDate::empty())?],
        )?,
    )
    .await?;
    assert!(
        errs.iter()
            .any(|e| matches!(e, SubmitError::NonSingleIntervalDate { .. })),
        "got {errs:?}"
    );
    Ok(())
}

/// A `JudgmentSource::External` citation carrying a disjunctive `published`
/// date is rejected — the rule reaches citation dates, not just fact payloads.
#[tokio::test]
async fn disjunctive_judgment_citation_date_rejected() -> TestResult {
    let store = MemoryFactStore::new();
    let errs = commit_err(
        &store,
        local_bundle(
            1,
            0,
            0,
            0,
            vec![observation_external_published(
                0,
                Some(disjunctive_date()?),
            )?],
        )?,
    )
    .await?;
    assert!(
        errs.iter().any(|e| matches!(
            e,
            SubmitError::NonSingleIntervalDate {
                role: DateRole::CitationDate
            }
        )),
        "got {errs:?}"
    );
    Ok(())
}

/// A `MetaSource::External` citation carrying a disjunctive `created` date is
/// rejected — the third `ExternalSource` host the traversal must reach.
#[tokio::test]
async fn disjunctive_meta_citation_date_rejected() -> TestResult {
    let store = MemoryFactStore::new();
    // Seed a commit so the retraction has a real target.
    let seed = commit_facts(
        &store,
        local_bundle(1, 0, 0, 0, vec![name_fact(0, "seed")?])?,
    )
    .await
    .map_err(|e| format!("{e:?}"))?;

    let retraction = SubmitFact::Meta {
        assertion: crate::facts::assertions::MetaAssertion::RetractCommit {
            target: seed.commit_id,
            reason: crate::facts::assertions::RetractionReason::FactualError,
        },
        citation: crate::facts::citations::MetaSource::External {
            source: ExternalSource::Archive {
                collection: "fonds".to_owned(),
                catalog_id: None,
                created: Some(disjunctive_date()?),
            },
        },
    };
    let bundle: TestBundle = SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(10),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [retraction].into_iter().collect(),
    };
    let errs = commit_err(&store, bundle).await?;
    assert!(
        errs.iter().any(|e| matches!(
            e,
            SubmitError::NonSingleIntervalDate {
                role: DateRole::CitationDate
            }
        )),
        "got {errs:?}"
    );
    Ok(())
}

// --- property tests ---

mod props {
    //! Property tests for the two unscaffolded algorithms: the backlink
    //! pagination cursor loop (async, over a live store) and the
    //! accumulating index substitution (sync, over the pipeline boundary).

    use std::collections::BTreeSet;
    use std::num::NonZeroUsize;

    use proptest::prelude::*;

    use super::{EntityIdx, MemoryEntityId, MemoryFactStore, SubmitError, name_fact};
    use crate::facts::ids::FactId;
    use crate::facts::store::{EntityView, FactStore};
    use crate::facts::submit::pipeline::substitute_facts_accumulating;

    /// A pagination scenario: `n` facts about one entity, a per-fact retraction
    /// mask of length `n`, and a page `limit` in `1..=n+2`.
    #[derive(Debug, Clone)]
    struct PaginationSpec {
        n: usize,
        retracted: Vec<bool>,
        limit: NonZeroUsize,
    }

    fn pagination_spec() -> impl Strategy<Value = PaginationSpec> {
        (1usize..=12)
            .prop_flat_map(|n| {
                let mask = proptest::collection::vec(any::<bool>(), n);
                let limit = 1usize..=(n + 2);
                (Just(n), mask, limit)
            })
            .prop_filter_map("limit is non-zero", |(n, retracted, limit)| {
                NonZeroUsize::new(limit).map(|limit| PaginationSpec {
                    n,
                    retracted,
                    limit,
                })
            })
    }

    /// Commit `n` distinct name facts about one entity, retract the masked
    /// subset in later commits, then drain the entity backlink page-by-page
    /// with `limit`. Returns the drained ids in page order and the active set
    /// (submitted minus retracted). Any store error becomes a `TestCaseError`.
    async fn drive_pagination(
        spec: &PaginationSpec,
    ) -> Result<(Vec<FactId>, BTreeSet<FactId>), TestCaseError> {
        let store = MemoryFactStore::new();

        let mut facts = BTreeSet::new();
        for i in 0..spec.n {
            let fact = name_fact(0, &format!("name-{i}"))
                .map_err(|e| TestCaseError::fail(format!("{e:?}")))?;
            facts.insert(fact);
        }
        let bundle = super::SubmitBundle {
            author: super::user_author().map_err(|e| TestCaseError::fail(format!("{e:?}")))?,
            recorded_at: super::fixed_time(),
            entities: vec![super::Decl::Local],
            events: Vec::new(),
            images: Vec::new(),
            facts,
        };
        let result = super::commit_facts(&store, bundle)
            .await
            .map_err(|e| TestCaseError::fail(format!("{e:?}")))?;
        let entity = result
            .entities
            .get(&EntityIdx(0))
            .ok_or_else(|| TestCaseError::fail("entity 0 missing"))?
            .id;

        // Submitted fact ids, in push (ascending) order.
        let submitted = &result.fact_ids;
        if submitted.len() != spec.n {
            return Err(TestCaseError::fail(format!(
                "expected {} facts, committed {}",
                spec.n,
                submitted.len()
            )));
        }

        // Retract the masked subset, each in its own later commit so the
        // retractor meta-facts hash distinctly.
        let mut active = BTreeSet::new();
        for (i, &fid) in submitted.iter().enumerate() {
            if spec.retracted.get(i).copied().unwrap_or(false) {
                super::commit_retract(&store, fid, (i as i64) + 1)
                    .await
                    .map_err(|e| TestCaseError::fail(format!("{e:?}")))?;
            } else {
                active.insert(fid);
            }
        }

        // Drive the cursor loop. Cap iterations so a cursor bug can't hang.
        let view = store
            .now()
            .await
            .map_err(|e| TestCaseError::fail(format!("{e:?}")))?;
        let mut drained = Vec::new();
        let mut cursor = FactId::new(0);
        let max_pages = spec.n + 2;
        let mut pages = 0;
        loop {
            if pages > max_pages {
                return Err(TestCaseError::fail(format!(
                    "cursor loop did not terminate after {max_pages} pages: {drained:?}"
                )));
            }
            pages += 1;
            let page = view
                .all_facts_about_entity(&entity, cursor, spec.limit)
                .await
                .map_err(|e| TestCaseError::fail(format!("{e:?}")))?;
            drained.extend(page.items.iter().map(|item| item.fact_id));
            match page.next_cursor {
                Some(next) => cursor = next,
                None => break,
            }
        }

        Ok((drained, active))
    }

    /// A scenario: `decl_count` declared entities and a vector of entity
    /// indices, each `< 2 * decl_count` so roughly half land out of range.
    #[derive(Debug, Clone)]
    struct SubstitutionSpec {
        decl_count: usize,
        indices: Vec<usize>,
    }

    fn substitution_spec() -> impl Strategy<Value = SubstitutionSpec> {
        (1usize..=8)
            .prop_flat_map(|decl_count| {
                // Indices span both in-range (< decl_count) and out-of-range
                // (>= decl_count) so each call exercises both partitions.
                let upper = 2 * decl_count;
                let indices = proptest::collection::vec(0usize..upper, 0..=12);
                (Just(decl_count), indices)
            })
            .prop_map(|(decl_count, indices)| SubstitutionSpec {
                decl_count,
                indices,
            })
    }

    proptest! {
        /// Paging the backlink drains exactly the active set, in strictly
        /// ascending id order. Covers the `next_cursor` resume loop, the
        /// all-retracted empty drain, and the short-page-with-cursor case
        /// (a retracted fact is skipped without consuming a slot).
        #[test]
        fn pagination_drains_exactly_the_active_set(spec in pagination_spec()) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| TestCaseError::fail(format!("runtime: {e}")))?;
            let (drained, active) = rt.block_on(drive_pagination(&spec))?;

            let ascending = drained.windows(2).all(|w| w[0] < w[1]);
            prop_assert!(
                ascending,
                "pages must drain ascending with no dup/skip: {drained:?}"
            );
            let drained_set: BTreeSet<FactId> = drained.into_iter().collect();
            prop_assert_eq!(drained_set, active);
        }

        /// Substitution partitions the fact set by entity-index validity:
        /// every in-range fact lands in `out`, every out-of-range one lands
        /// as a typed `EntityIdxOutOfRange` carrying the input `decl_count`,
        /// and the two partitions cover the input exactly once.
        ///
        /// Order-independence is structural (the input is a `BTreeSet`), so
        /// the pinned property is the count-partition and the typed error,
        /// not ordering.
        #[test]
        fn substitution_partitions_facts_by_index_validity(spec in substitution_spec()) {
            // One distinct fact per index (unique names → distinct BTreeSet
            // elements). Distinct names keep every index its own fact even
            // when indices repeat.
            let mut facts = BTreeSet::new();
            for (slot, &idx) in spec.indices.iter().enumerate() {
                let fact = name_fact(idx, &format!("fact-{slot}"))
                    .map_err(|e| TestCaseError::fail(format!("{e:?}")))?;
                facts.insert(fact);
            }
            let total = facts.len();

            // Resolution map for the in-range indices only.
            let entities: std::collections::HashMap<EntityIdx, MemoryEntityId> = (0..spec.decl_count)
                .map(|i| (EntityIdx(i), MemoryEntityId(i as u64)))
                .collect();
            let events = std::collections::HashMap::new();
            let images = std::collections::HashMap::new();

            let (out, errors) = substitute_facts_accumulating::<
                MemoryEntityId,
                crate::facts::memory::MemoryEventId,
                crate::facts::memory::MemoryImageId,
            >(&facts, &entities, &events, &images);

            let expected_errors = spec
                .indices
                .iter()
                .filter(|&&idx| idx >= spec.decl_count)
                .count();
            // Indices collapsed by the unique-name BTreeSet can't drop an
            // out-of-range one: each name is distinct, so `total == indices`.
            prop_assert_eq!(total, spec.indices.len());
            prop_assert_eq!(errors.len(), expected_errors);

            for err in &errors {
                match err {
                    SubmitError::EntityIdxOutOfRange { decl_count, .. } => {
                        prop_assert_eq!(*decl_count, spec.decl_count);
                    }
                    other => {
                        return Err(TestCaseError::fail(format!(
                            "expected EntityIdxOutOfRange, got {other:?}"
                        )));
                    }
                }
            }

            // Every fact classified exactly once: in-range → out, else error.
            prop_assert_eq!(out.len(), total - expected_errors);
            prop_assert_eq!(out.len() + errors.len(), total);
        }
    }
}
