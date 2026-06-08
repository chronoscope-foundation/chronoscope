use super::*;
use chrono::TimeZone;
use oxilangtag::LanguageTag;
use url::Url;

use crate::date::{DatePrecision, UncertainDate};
use crate::facts::assertions::FactualAssertion;
use crate::facts::assertions::JudgmentAssertion;
use crate::facts::attribute::{self, NameType};
use crate::facts::bookend;
use crate::facts::citations::{Excerpt, ExternalSource, FactualCitation};
use crate::facts::citations::{JudgmentSource, Justification};
use crate::facts::identity;
use crate::facts::ids::UserId;
use crate::facts::submit::{
    Commit as SubmitBundle, Decl, EntityIdx, EventIdx, ImageIdx, SubmitFact,
};
use crate::facts::submit::{CommitAuthor, ResolutionOrigin, StoredFact, SubmitError, commit_facts};

type TestResult = Result<(), Box<dyn std::error::Error>>;
type TestBundle = SubmitBundle<MemoryEntityId, MemoryEventId, MemoryImageId>;

// --- helpers ---

/// A fixed UTC timestamp for test fixtures.
fn fixed_time() -> chrono::DateTime<chrono::Utc> {
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

fn name_fact(entity_idx: usize, name: &str) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    let language = LanguageTag::parse("en".to_owned())?;
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Attribute {
            fact: attribute::Fact::Name {
                entity: EntityIdx(entity_idx),
                name: name.to_owned(),
                language,
                name_type: NameType::Common,
                valid_from: None,
                valid_to: None,
            },
        },
        citation: sample_citation()?,
    })
}

fn construction_started_fact(entity_idx: usize) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    let bound = UncertainDate::with_precision(
        chrono::NaiveDate::from_ymd_opt(1700, 1, 1).ok_or("date")?,
        DatePrecision::Year,
    )?;
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

/// An event-touching `PointDate` fact for the given event index.
fn event_point_date_fact(event_idx: usize) -> Result<SubmitFact, Box<dyn std::error::Error>> {
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

fn user_author() -> Result<CommitAuthor, Box<dyn std::error::Error>> {
    Ok(CommitAuthor::User(UserId::new("alice")))
}

/// A `RetractCommit` meta-fact targeting `target`. References no indices
/// (the target is a `CommitId`), so a bundle carrying only this fact
/// declares no entities / events / images.
fn retract_commit_fact(
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

    match (kind, err) {
        (
            ExpectedKind::Entity,
            SubmitCommitError::Submit(SubmitError::EntityIdxOutOfRange {
                idx,
                decl_count: got,
            }),
        ) => {
            assert_eq!(idx, bad_idx);
            assert_eq!(got, decl_count);
        }
        (
            ExpectedKind::Event,
            SubmitCommitError::Submit(SubmitError::EventIdxOutOfRange {
                idx,
                decl_count: got,
            }),
        ) => {
            assert_eq!(idx, bad_idx);
            assert_eq!(got, decl_count);
        }
        (
            ExpectedKind::Image,
            SubmitCommitError::Submit(SubmitError::ImageIdxOutOfRange {
                idx,
                decl_count: got,
            }),
        ) => {
            assert_eq!(idx, bad_idx);
            assert_eq!(got, decl_count);
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

    match (kind, err) {
        (
            UnknownKind::Entity,
            SubmitCommitError::Submit(SubmitError::UnknownExistingEntity { decl_position }),
        ) => {
            assert_eq!(decl_position, EntityIdx(0));
        }
        (
            UnknownKind::Event,
            SubmitCommitError::Submit(SubmitError::UnknownExistingEvent { decl_position }),
        ) => {
            assert_eq!(decl_position, EventIdx(0));
        }
        (
            UnknownKind::Image,
            SubmitCommitError::Submit(SubmitError::UnknownExistingImage { decl_position }),
        ) => {
            assert_eq!(decl_position, ImageIdx(0));
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
    assert_eq!(name, "Pantheon");
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
/// while the in-memory `walk_*` returns an empty stub; flips green once
/// `walk_entities` reads the fact bag.
#[tokio::test]
#[ignore = "walk_entities is stubbed to an empty page in this backend; this pins the walk-returns-submitted-facts contract and flips green once walk_* is implemented (a backend stubbing/lying about walk support fails this check)"]
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
        .walk_entities(&EntityStream::All, FactId::new(0), 100)
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
#[ignore = "all_facts_about_entity is stubbed to an empty page; this pins the backlink-returns-mentioning-facts contract and flips green once the backlink index is implemented"]
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
        .all_facts_about_entity(&entity, FactId::new(0), 100)
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
        .walk_events(&EventStream::All, FactId::new(0), 100)
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
#[ignore = "all_facts_about_event is stubbed to an empty page; this pins the backlink-returns-mentioning-facts contract and flips green once the backlink index is implemented"]
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
        .all_facts_about_event(&event, FactId::new(0), 100)
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
/// `#[ignore]`d while stubbed; flips green once `walk_images` reads the
/// fact bag.
#[tokio::test]
#[ignore = "walk_images is stubbed to an empty page; this pins the walk-returns-submitted-facts contract and flips green once walk_images reads the fact bag"]
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
        .walk_images(&ImageStream::All, FactId::new(0), 100)
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
#[ignore = "all_facts_about_image is stubbed to an empty page; this pins the backlink-returns-mentioning-facts contract and flips green once the backlink index is implemented"]
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
        .all_facts_about_image(&image, FactId::new(0), 100)
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
// fact carries two distinct ids in one class. The stubs answer as if every
// id were a singleton — contradicting a two-member class below.

/// After a `SameEntity` fact links two ids, `entity_class` of either member
/// must contain both. `#[ignore]`d while the stub returns the singleton;
/// flips green once the class read unions the equivalence facts.
#[tokio::test]
#[ignore = "entity_class is stubbed to the singleton {member}; this pins that a SameEntity-linked pair shares a two-member class and flips green once the union-find read is implemented"]
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
/// `entity_class(..).representative`. `#[ignore]`d while the stub returns
/// `member`; flips green once representative selection reads the
/// equivalence facts.
#[tokio::test]
#[ignore = "entity_representative is stubbed to return member itself; this pins that both SameEntity members share one canonical representative and flips green once representative selection is implemented"]
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
/// `#[ignore]`d while stubbed; flips green once the class read unions the
/// equivalence facts.
#[tokio::test]
#[ignore = "image_class is stubbed to the singleton {member}; this pins that a SameArtifact-linked pair shares a two-member class and flips green once the union-find read is implemented"]
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
/// `#[ignore]`d while stubbed; flips green once representative selection
/// reads the equivalence facts.
#[tokio::test]
#[ignore = "image_representative is stubbed to return member itself; this pins that both SameArtifact members share one canonical representative and flips green once representative selection is implemented"]
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
/// id, plus an identity fact over the two indices, must be rejected at
/// substitution with `IdentityEntitySelfEquivalence` carrying the shared
/// typed id.
#[tokio::test]
async fn same_entity_resolving_to_one_id_rejected_at_substitution() -> TestResult {
    let store = MemoryFactStore::new();

    // 1. Mint a real entity id via a Local commit.
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

    // 2. Submit two `Decl::Existing(entity_id)` slots on the same minted
    //    id, plus a SameEntity fact over the two indices.
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

    let SubmitCommitError::Submit(SubmitError::IdentityEntitySelfEquivalence { id }) = err else {
        return Err(format!("expected IdentityEntitySelfEquivalence, got {err:?}").into());
    };
    assert_eq!(id, entity_id);
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

    let SubmitCommitError::Submit(SubmitError::CommitNotFound { id }) = err else {
        return Err(format!("expected CommitNotFound, got {err:?}").into());
    };
    assert_eq!(id, phantom);
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

/// Smoke test for [`UnionSource`] + [`Pending`] + [`apply_pending`].
///
/// Starts with one committed fact, builds a [`UnionSource`], mints one id
/// of each kind, stages a fact, drains, and applies. Verifies:
///
/// - mints come from the in-flight counters (one past each committed one);
/// - the staged fact is visible at its slot (`committed.len()`) before
///   apply;
/// - `apply_pending` assigns its [`FactId`] at that slot and advances the
///   counters.
#[tokio::test]
async fn union_source_mint_push_apply_lands_at_expected_fact_id() -> TestResult {
    // 1. Commit one fact so the store has committed state to union over.
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

    // 2. Clone the committed fact's body to re-push as the pending fact
    //    (avoids synthesising a fresh StoredFact). Drop the guard before
    //    the UnionSource interaction to control re-acquisition.
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

    // 3. Mint one of each kind, stage the seed fact, drain to Pending.
    let pending;
    let minted_entity;
    let minted_event;
    let minted_image;
    {
        let guard = store.lock_inner().await;
        let mut source = UnionSource::from_inner(&guard);

        // Counters start at the committed values.
        assert_eq!(source.pending.next_entity_id, next_entity_before);
        assert_eq!(source.pending.next_event_id, next_event_before);
        assert_eq!(source.pending.next_image_id, next_image_before);

        minted_entity = source.mint_entity();
        minted_event = source.mint_event();
        minted_image = source.mint_image();

        // Minted ids land at the pre-mint counter value.
        assert_eq!(minted_entity.0, next_entity_before);
        assert_eq!(minted_event.0, next_event_before);
        assert_eq!(minted_image.0, next_image_before);

        // The source considers them known; Inner is still untouched.
        assert!(source.entity_known(&minted_entity));
        assert!(source.event_known(&minted_event));
        assert!(source.image_known(&minted_image));

        // snapshot reports committed + pending; pending is empty pre-push.
        // UFCS because the blanket `FactView::snapshot` also applies.
        assert_eq!(
            CoreSource::snapshot(&source).get(),
            committed_len_before as u64
        );

        source.push_fact(seed_stored.clone());

        // After push, snapshot reflects the new total.
        assert_eq!(
            CoreSource::snapshot(&source).get(),
            (committed_len_before + 1) as u64
        );

        // Staged fact is visible at its slot. `with_core` awaits nothing
        // for a `UnionSource` but is async to match the trait.
        let lookup = source
            .with_core(|core| core.fact_at(expected_fact_id))
            .await;
        let FactLookup::Active(_) = lookup else {
            return Err(format!("pending fact must read Active; got {lookup:?}").into());
        };

        pending = source.into_pending();
    } // guard dropped here

    // 4. Apply the pending payload mutably.
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

    // 5. After apply: facts vec grew by one, counters advanced by the mints.
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
    let SubmitCommitError::Submit(SubmitError::MetaTargetInSameCommit { target }) = err else {
        return Err(format!("expected MetaTargetInSameCommit, got {err:?}").into());
    };
    assert_eq!(target, in_commit);
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
    let SubmitCommitError::Submit(SubmitError::MetaTargetInSameCommit { target }) = err else {
        return Err(format!("expected MetaTargetInSameCommit, got {err:?}").into());
    };
    assert_eq!(target, in_commit);
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
    let SubmitCommitError::Submit(SubmitError::SupersedeReplacementEqualsTarget { target }) = err
    else {
        return Err(format!("expected SupersedeReplacementEqualsTarget, got {err:?}").into());
    };
    assert_eq!(target, fact_id);
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
    let SubmitCommitError::Submit(SubmitError::FactNotFound { id }) = err else {
        return Err(format!("expected FactNotFound, got {err:?}").into());
    };
    assert_eq!(id, phantom);
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
