use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroUsize;

use chrono::TimeZone;
use url::Url;

use super::*;
use crate::date::{DatePrecision, UncertainDate};
use crate::facts::assertions::{
    FactualAssertion, JudgmentAssertion, MetaAssertion, RetractionReason,
};
use crate::facts::attribute::{self, EntityRelationType, NameText, NameType};
use crate::facts::bookend;
use crate::facts::citations::{
    Excerpt, ExternalReference, ExternalSource, FactualCitation, JudgmentSource, Justification,
    Language, MetaSource,
};
use crate::facts::identity;
use crate::facts::ids::{FactId, UserId};
use crate::facts::lifecycle::{LifetimeEventKind, PointKind};
use crate::facts::memory::{MemoryEntityId, MemoryFactStore};
use crate::facts::schema::{FactPage, PageItem};
use crate::facts::store::FactStore;
use crate::facts::submit::{
    Commit as SubmitBundle, CommitAuthor, Decl, EntityIdx, EventIdx, ImageIdx, StoredFact,
    SubmitFact, commit_facts,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;
type MemEntId = <MemoryFactStore as FactStore>::EntityId;
type MemEvtId = <MemoryFactStore as FactStore>::EventId;
type MemImgId = <MemoryFactStore as FactStore>::ImageId;

fn fixed_time() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc
        .with_ymd_and_hms(2024, 1, 1, 12, 0, 0)
        .single()
        .unwrap_or_default()
}

fn sample_citation() -> Result<FactualCitation, Box<dyn std::error::Error>> {
    let source = ExternalSource::Url {
        url: Url::parse("https://example.com/source")?,
        published: None,
    };
    Ok(FactualCitation::new(
        source,
        vec![Excerpt::new("source-text")?],
    )?)
}

fn judgment_source() -> Result<JudgmentSource<ImageIdx>, Box<dyn std::error::Error>> {
    Ok(JudgmentSource::PersonalKnowledge {
        user: UserId::new("alice"),
        justification: Justification::new("these two are the same entity")?,
    })
}

fn year_date(year: i32) -> Result<UncertainDate, Box<dyn std::error::Error>> {
    Ok(UncertainDate::with_precision(
        chrono::NaiveDate::from_ymd_opt(year, 1, 1).ok_or("date")?,
        DatePrecision::Year,
    )?)
}

fn name_fact(
    entity_idx: usize,
    name: &str,
    language: &str,
) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    name_fact_from(entity_idx, name, language, "https://example.com/source")
}

/// A `Name` fact citing a specific source URL, so two facts asserting the
/// identical name triple are distinct stored facts (different content) that
/// still collapse to one projected slot.
fn name_fact_from(
    entity_idx: usize,
    name: &str,
    language: &str,
    source_url: &str,
) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    let citation = FactualCitation::new(
        ExternalSource::Url {
            url: Url::parse(source_url)?,
            published: None,
        },
        vec![Excerpt::new("source-text")?],
    )?;
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Attribute {
            fact: attribute::Fact::Name {
                entity: EntityIdx(entity_idx),
                name: NameText::new(name),
                language: Language::new(language)?,
                name_type: NameType::Common,
                valid_from: None,
                valid_to: None,
            },
        },
        citation,
    })
}

fn external_ref_fact(
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

fn relationship_fact(from: usize, to: usize) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Attribute {
            fact: attribute::Fact::relationship(
                EntityIdx(from),
                EntityIdx(to),
                EntityRelationType::Replaces,
            )?,
        },
        citation: sample_citation()?,
    })
}

fn construction_started_fact(
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

fn same_entity_fact(a: usize, b: usize) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    Ok(SubmitFact::Judgment {
        assertion: JudgmentAssertion::Identity {
            fact: identity::Fact::same_entity(EntityIdx(a), EntityIdx(b))?,
        },
        citation: judgment_source()?,
    })
}

/// Submit one bundle, mapping the error into a boxed string so `?` works.
async fn submit(
    store: &MemoryFactStore,
    entities: usize,
    facts: Vec<SubmitFact>,
) -> Result<
    crate::facts::submit::SubmitResult<MemEntId, MemEvtId, MemImgId>,
    Box<dyn std::error::Error>,
> {
    let bundle: SubmitBundle<MemEntId, MemEvtId, MemImgId> = SubmitBundle {
        author: CommitAuthor::User(UserId::new("alice")),
        recorded_at: fixed_time(),
        entities: (0..entities).map(|_| Decl::Local).collect(),
        events: Vec::new(),
        images: Vec::new(),
        facts: facts.into_iter().collect(),
    };
    commit_facts(store, bundle)
        .await
        .map_err(|e| format!("{e:?}").into())
}

// ------------------------------------------------------------------
// Names
// ------------------------------------------------------------------

#[tokio::test]
async fn single_name_projects_one_slot() -> TestResult {
    let store = MemoryFactStore::new();
    let result = submit(&store, 1, vec![name_fact(0, "Pantheon", "en")?]).await?;
    let id = result.entities.get(&EntityIdx(0)).ok_or("missing")?.id;

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let projected = project_entity::<MemoryFactStore, _>(&view, id).await?;

    assert_eq!(projected.entity.names.len(), 1);
    let name = projected.entity.names.first().ok_or("no name")?;
    assert_eq!(name.name.as_str(), "Pantheon");
    assert_eq!(name.language.as_str(), "en");
    assert_eq!(name.name_type, NameType::Common);

    let path = JsonPath::root().field("names").index(0);
    let prov = projected
        .citations
        .get(&path)
        .ok_or("no citation at $.names[0]")?;
    assert_eq!(prov.supports.len(), 1);
    Ok(())
}

#[tokio::test]
async fn multiple_names_all_languages_preserved() -> TestResult {
    let store = MemoryFactStore::new();
    // Three distinct names plus a fourth from a different source that
    // duplicates the `en` triple — a distinct stored fact that collapses
    // into the same projected slot.
    let result = submit(
        &store,
        1,
        vec![
            name_fact(0, "Pantheon", "en")?,
            name_fact(0, "Pantheon", "it")?,
            name_fact(0, "Panthéon", "fr")?,
            name_fact_from(0, "Pantheon", "en", "https://other.example/source")?,
        ],
    )
    .await?;
    let id = result.entities.get(&EntityIdx(0)).ok_or("missing")?.id;

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let projected = project_entity::<MemoryFactStore, _>(&view, id).await?;

    assert_eq!(
        projected.entity.names.len(),
        3,
        "the exact-triple duplicate collapses; the three distinct names stay"
    );
    let langs: BTreeSet<&str> = projected
        .entity
        .names
        .iter()
        .map(|n| n.language.as_str())
        .collect();
    assert_eq!(langs, BTreeSet::from(["en", "it", "fr"]));

    // The collapsed `en` slot cites BOTH backing facts.
    let en_idx = projected
        .entity
        .names
        .iter()
        .position(|n| n.language.as_str() == "en")
        .ok_or("no en name")?;
    let path = JsonPath::root().field("names").index(en_idx);
    let prov = projected.citations.get(&path).ok_or("no en citation")?;
    assert_eq!(
        prov.supports.len(),
        2,
        "a value-deduped slot cites every fact that fed it"
    );
    Ok(())
}

// ------------------------------------------------------------------
// Dates — join, not hull
// ------------------------------------------------------------------

#[tokio::test]
async fn multiple_date_claims_join_to_disjunction() -> TestResult {
    let store = MemoryFactStore::new();
    // Two construction-start claims a decade apart: "the 1920s" and "1935".
    let bound_1920s = UncertainDate::bounded(
        crate::date::DateBound::new(
            chrono::NaiveDate::from_ymd_opt(1920, 1, 1).ok_or("date")?,
            DatePrecision::Decade,
        )
        .ok(),
        crate::date::DateBound::new(
            chrono::NaiveDate::from_ymd_opt(1920, 1, 1).ok_or("date")?,
            DatePrecision::Decade,
        )
        .ok(),
    )?;
    let bound_1935 = year_date(1935)?;

    let started_1920s = SubmitFact::Factual {
        assertion: FactualAssertion::Construction {
            fact: bookend::Fact::Started {
                entity: EntityIdx(0),
                bound: bound_1920s.clone(),
            },
        },
        citation: sample_citation()?,
    };
    let started_1935 = SubmitFact::Factual {
        assertion: FactualAssertion::Construction {
            fact: bookend::Fact::Started {
                entity: EntityIdx(0),
                bound: bound_1935.clone(),
            },
        },
        citation: sample_citation()?,
    };

    let result = submit(&store, 1, vec![started_1920s, started_1935]).await?;
    let id = result.entities.get(&EntityIdx(0)).ok_or("missing")?.id;

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let projected = project_entity::<MemoryFactStore, _>(&view, id).await?;

    let construction = projected
        .entity
        .construction
        .as_ref()
        .ok_or("no construction")?;
    let started = construction.started_at.as_ref().ok_or("no start date")?;

    let expected = bound_1920s.join(&bound_1935);
    assert_eq!(
        started, &expected,
        "the slot is the set union of the two claims, not a hull"
    );
    // The 1920s and 1935 claims are genuinely disjoint, so the union stays
    // two intervals — a hull would have collapsed to one.
    assert_eq!(
        started.intervals().len(),
        2,
        "disjoint claims stay disjoint; no gap is invented"
    );

    let path = JsonPath::root().field("construction").field("started_at");
    let prov = projected.citations.get(&path).ok_or("no start citation")?;
    assert_eq!(
        prov.supports.len(),
        2,
        "both facts cited at the joined slot"
    );
    Ok(())
}

// ------------------------------------------------------------------
// Class union and root provenance
// ------------------------------------------------------------------

#[tokio::test]
async fn same_entity_class_unions_members() -> TestResult {
    let store = MemoryFactStore::new();
    // A: name "X"/en. B: name "Y"/it + an external ref. Linked SameEntity.
    let result = submit(
        &store,
        2,
        vec![
            name_fact(0, "X", "en")?,
            name_fact(1, "Y", "it")?,
            external_ref_fact(1, 42)?,
            same_entity_fact(0, 1)?,
        ],
    )
    .await?;
    let a = result.entities.get(&EntityIdx(0)).ok_or("missing a")?.id;

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let projected = project_entity::<MemoryFactStore, _>(&view, a).await?;

    let names: BTreeSet<&str> = projected
        .entity
        .names
        .iter()
        .map(|n| n.name.as_str())
        .collect();
    assert_eq!(
        names,
        BTreeSet::from(["X", "Y"]),
        "projecting one member yields both members' names"
    );
    assert_eq!(
        projected.entity.external_references.len(),
        1,
        "the other member's external ref surfaces too"
    );

    // Root cites the SameEntity edge's judgment source.
    let prov = projected
        .citations
        .get(&JsonPath::root())
        .ok_or("merged class must cite root")?;
    assert_eq!(prov.supports.len(), 1);
    assert!(matches!(
        prov.supports.first(),
        Some(ProjectedCitation::Judgment { .. })
    ));
    Ok(())
}

#[tokio::test]
async fn singleton_class_leaves_root_unaddressed() -> TestResult {
    let store = MemoryFactStore::new();
    let result = submit(&store, 1, vec![name_fact(0, "Solo", "en")?]).await?;
    let id = result.entities.get(&EntityIdx(0)).ok_or("missing")?.id;

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let projected = project_entity::<MemoryFactStore, _>(&view, id).await?;

    assert!(
        projected.citations.get(&JsonPath::root()).is_none(),
        "a singleton class fabricates no root provenance"
    );
    Ok(())
}

// ------------------------------------------------------------------
// Retraction visibility
// ------------------------------------------------------------------

#[tokio::test]
async fn retraction_drops_a_fact_from_the_view() -> TestResult {
    let store = MemoryFactStore::new();
    // Two names on one entity.
    let result = submit(
        &store,
        1,
        vec![name_fact(0, "Old", "en")?, name_fact(0, "New", "en")?],
    )
    .await?;
    let id = result.entities.get(&EntityIdx(0)).ok_or("missing")?.id;

    // Snapshot before any retraction sees both names.
    let snapshot_before = store.next_fact_id().await.map_err(|e| format!("{e:?}"))?;

    // Retract the "Old" name fact in a second commit. Find its FactId by
    // reading the active facts back.
    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let page = view
        .all_facts_about_entity(&id, FactId::new(0), DRAIN_PAGE)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let old_fact_id = page
        .items
        .iter()
        .find_map(|item| match &item.fact {
            StoredFact::Factual(f) => match &f.assertion {
                FactualAssertion::Attribute {
                    fact: attribute::Fact::Name { name, .. },
                } if name.as_str() == "Old" => Some(item.fact_id),
                _ => None,
            },
            _ => None,
        })
        .ok_or("Old name fact not found")?;

    let retract_bundle: SubmitBundle<MemEntId, MemEvtId, MemImgId> = SubmitBundle {
        author: CommitAuthor::User(UserId::new("alice")),
        recorded_at: fixed_time() + chrono::Duration::seconds(1),
        entities: Vec::new(),
        events: Vec::new(),
        images: Vec::new(),
        facts: [SubmitFact::Meta {
            assertion: MetaAssertion::RetractFact {
                target: old_fact_id,
                reason: RetractionReason::FactualError,
            },
            citation: MetaSource::PersonalKnowledge {
                user: UserId::new("alice"),
                justification: Justification::new("the old name was wrong")?,
            },
        }]
        .into_iter()
        .collect(),
    };
    commit_facts(&store, retract_bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;

    // At now(): one name.
    let view_now = store.now().await.map_err(|e| format!("{e:?}"))?;
    let after = project_entity::<MemoryFactStore, _>(&view_now, id).await?;
    assert_eq!(
        after.entity.names.len(),
        1,
        "the retracted name is gone at now()"
    );
    assert_eq!(
        after.entity.names.first().ok_or("no name")?.name.as_str(),
        "New"
    );

    // At the pre-retraction snapshot: both names.
    let view_before = store.no_later_than(snapshot_before);
    let before = project_entity::<MemoryFactStore, _>(&view_before, id).await?;
    assert_eq!(
        before.entity.names.len(),
        2,
        "before the retraction both names were active"
    );
    Ok(())
}

// ------------------------------------------------------------------
// Address correctness; no Meta leakage
// ------------------------------------------------------------------

#[tokio::test]
async fn citation_addressing_resolves_each_field() -> TestResult {
    let store = MemoryFactStore::new();
    // Two entities so a relationship target exists, linked into one class
    // so the relation surfaces. Names + a bookend + an external ref + a
    // relation across all members.
    let result = submit(
        &store,
        2,
        vec![
            name_fact(0, "Hall", "en")?,
            construction_started_fact(0, 1900)?,
            external_ref_fact(0, 7)?,
            relationship_fact(0, 1)?,
            name_fact(1, "Annex", "en")?,
            same_entity_fact(0, 1)?,
        ],
    )
    .await?;
    let a = result.entities.get(&EntityIdx(0)).ok_or("missing a")?.id;

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let projected = project_entity::<MemoryFactStore, _>(&view, a).await?;

    // Every populated value field has a sidecar entry that resolves.
    for (idx, _) in projected.entity.names.iter().enumerate() {
        let path = JsonPath::root().field("names").index(idx);
        assert!(
            projected.citations.get(&path).is_some(),
            "name slot {idx} must be addressed"
        );
    }
    assert!(
        projected
            .citations
            .get(&JsonPath::root().field("construction").field("started_at"))
            .is_some()
    );
    assert!(
        projected
            .citations
            .get(&JsonPath::root().field("external_references").index(0))
            .is_some()
    );
    assert!(
        projected
            .citations
            .get(&JsonPath::root().field("relations").index(0))
            .is_some()
    );

    // Meta facts never back a value (the sidecar sum has no meta arm, so a
    // retracted/superseding fact can't reach a supports list). Here the
    // only Judgment-backed slot is root; every value field is Factual-
    // backed — a misrouted judgment into a value slot would fail this.
    let root = JsonPath::root();
    for (path, prov) in &projected.citations.0 {
        for support in &prov.supports {
            if path == &root {
                assert!(matches!(support, ProjectedCitation::Judgment { .. }));
            } else {
                assert!(
                    matches!(support, ProjectedCitation::Factual { .. }),
                    "value slot {path} must be factual-backed"
                );
            }
        }
    }
    Ok(())
}

// ------------------------------------------------------------------
// Drain — page boundary and short-page-with-cursor
// ------------------------------------------------------------------

#[tokio::test]
async fn projection_drains_past_page_boundary() -> TestResult {
    let store = MemoryFactStore::new();
    // Submit more names than a single page of the drain holds. The drain
    // uses DRAIN_PAGE=256, so directly exercising the in-memory backend
    // across pages would need >256 facts; instead drive the factored drain
    // with a small limit against the real backend.
    let mut facts = Vec::new();
    for i in 0..10 {
        facts.push(name_fact(0, &format!("name-{i}"), "en")?);
    }
    let result = submit(&store, 1, facts).await?;
    let id = result.entities.get(&EntityIdx(0)).ok_or("missing")?.id;

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let tiny: NonZeroUsize = NonZeroUsize::new(3).ok_or("nonzero")?;
    let drained = drain_id_facts(|cursor| view.all_facts_about_entity(&id, cursor, tiny))
        .await
        .map_err(|e| format!("{e:?}"))?;

    let name_count = drained
        .iter()
        .filter(|(_, f)| {
            matches!(
                f,
                StoredFact::Factual(ff)
                    if matches!(
                        &ff.assertion,
                        FactualAssertion::Attribute {
                            fact: attribute::Fact::Name { .. }
                        }
                    )
            )
        })
        .count();
    assert_eq!(
        name_count, 10,
        "every fact must surface across the page boundary (no cursor +1 drop)"
    );
    Ok(())
}

/// A stub page source that returns one item per page with a `Some` cursor
/// until exhausted — and goes a step further by returning an EMPTY page that
/// still carries a cursor before the final page. A length-based terminator
/// (`len < limit ⇒ stop`, or `empty ⇒ stop`) would truncate here; the
/// correct drain only stops on `next_cursor == None`. The in-memory backend
/// can't produce a short-page-with-cursor, hence the stub.
#[tokio::test]
async fn drain_continues_past_short_page_with_cursor() -> TestResult {
    type StubFact = StoredFact<MemEntId, MemEvtId, MemImgId>;
    let item = |id: u64| -> Result<PageItem<StubFact, MemEntId>, Box<dyn std::error::Error>> {
        Ok(PageItem {
            fact_id: FactId::new(id),
            fact: StoredFact::Factual(crate::facts::submit::result::StoredFactualFact {
                assertion: FactualAssertion::Attribute {
                    fact: attribute::Fact::Name {
                        entity: MemoryEntityId(0),
                        name: NameText::new("stub"),
                        language: Language::new("en")?,
                        name_type: NameType::Common,
                        valid_from: None,
                        valid_to: None,
                    },
                },
                citation: sample_citation()?,
            }),
            representative: MemoryEntityId(0),
        })
    };

    // Pages keyed by the cursor the drain passes in: cursor 0 → item 0,
    // resume at 1; cursor 1 → EMPTY, resume at 2; cursor 2 → item 2, resume
    // at 3; cursor 3 → item 3, done.
    let pages: Vec<FactPage<StubFact, MemEntId>> = vec![
        FactPage {
            items: vec![item(0)?],
            next_cursor: Some(FactId::new(1)),
        },
        FactPage {
            items: Vec::new(),
            next_cursor: Some(FactId::new(2)),
        },
        FactPage {
            items: vec![item(2)?],
            next_cursor: Some(FactId::new(3)),
        },
        FactPage {
            items: vec![item(3)?],
            next_cursor: None,
        },
    ];

    let drained: Vec<(FactId, StubFact)> = drain_id_facts(|cursor| {
        let page = pages.get(cursor.get() as usize).cloned();
        async move {
            page.ok_or("stub page source: cursor out of range")
                .map_err(|e: &str| e.to_owned())
        }
    })
    .await?;

    let ids: Vec<u64> = drained.iter().map(|(id, _)| id.get()).collect();
    assert_eq!(
        ids,
        vec![0, 2, 3],
        "the drain collects every item across short and empty pages, \
         stopping only when next_cursor is None"
    );
    Ok(())
}

// ------------------------------------------------------------------
// Interior events — category arms, role split, conflict-as-None
//
// `project_events` is the pure merge over an entity's facts; it is
// exercised here directly on a hand-built fact set, the same shape the
// entity drain hands it.
// ------------------------------------------------------------------

type StoredEventFact = StoredFact<MemEntId, MemEvtId, MemImgId>;

fn ent(id: u64) -> MemEntId {
    MemoryEntityId(id)
}

fn evt(id: u64) -> MemEvtId {
    crate::facts::memory::MemoryEventId(id)
}

/// Wrap an event-cluster fact as a stored factual fact at the given id.
fn event_stored(
    fact_id: u64,
    fact: event::Fact<MemEntId, MemEvtId>,
) -> Result<(FactId, StoredEventFact), Box<dyn std::error::Error>> {
    Ok((
        FactId::new(fact_id),
        StoredFact::Factual(crate::facts::submit::result::StoredFactualFact {
            assertion: FactualAssertion::Event { fact },
            citation: sample_citation()?,
        }),
    ))
}

/// A `HasEvent` stored fact tying an event to an entity at a declared kind.
fn has_event_stored(
    fact_id: u64,
    entity: MemEntId,
    event: MemEvtId,
    kind: LifetimeEventKind,
) -> Result<(FactId, StoredEventFact), Box<dyn std::error::Error>> {
    event_stored(
        fact_id,
        event::Fact::HasEvent {
            entity,
            event,
            kind,
        },
    )
}

/// Mint an entity and an interior event tied to it by a `HasEvent`, plus a
/// `DurationalDate`, then project the *entity*. The events Vec must populate
/// — through the real entry point, this is the entity→event hop the
/// `HasEvent` bridge makes reachable.
#[tokio::test]
async fn entity_projects_has_event_linked_event() -> TestResult {
    use crate::facts::event::Fact as EventFact;
    use crate::facts::lifecycle::{DamageCause, DurationalKind, DurationalRole};

    let store = MemoryFactStore::new();
    let bundle: SubmitBundle<MemEntId, MemEvtId, MemImgId> = SubmitBundle {
        author: CommitAuthor::User(UserId::new("alice")),
        recorded_at: fixed_time(),
        entities: vec![Decl::Local],
        events: vec![Decl::Local],
        images: Vec::new(),
        facts: [
            SubmitFact::Factual {
                assertion: FactualAssertion::Event {
                    fact: EventFact::HasEvent {
                        entity: EntityIdx(0),
                        event: EventIdx(0),
                        kind: LifetimeEventKind::Durational {
                            kind: DurationalKind::Damaged,
                        },
                    },
                },
                citation: sample_citation()?,
            },
            SubmitFact::Factual {
                assertion: FactualAssertion::Event {
                    fact: EventFact::DamageCause {
                        event: EventIdx(0),
                        cause: DamageCause::Fire,
                    },
                },
                citation: sample_citation()?,
            },
            SubmitFact::Factual {
                assertion: FactualAssertion::Event {
                    fact: EventFact::DurationalDate {
                        event: EventIdx(0),
                        role: DurationalRole::Started,
                        bound: year_date(1850)?,
                    },
                },
                citation: sample_citation()?,
            },
        ]
        .into_iter()
        .collect(),
    };
    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let id = result.entities.get(&EntityIdx(0)).ok_or("missing")?.id;

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let projected = project_entity::<MemoryFactStore, _>(&view, id).await?;

    assert_eq!(
        projected.entity.events.len(),
        1,
        "the HasEvent-linked interior event is reachable through project_entity"
    );
    let ProjectedLifetimeEvent::Durational {
        kind, started_at, ..
    } = projected.entity.events.first().ok_or("no event")?
    else {
        return Err("expected a durational event".into());
    };
    assert_eq!(
        *kind,
        BTreeSet::from([DurationalKind::Damaged]),
        "the kind reads off the HasEvent claim"
    );
    assert_eq!(started_at.as_ref(), Some(&year_date(1850)?));
    Ok(())
}

#[test]
fn durational_event_splits_start_and_completion() -> TestResult {
    use crate::facts::lifecycle::{DurationalKind, DurationalRole};
    let facts: BTreeMap<FactId, _> = [
        has_event_stored(
            0,
            ent(7),
            evt(1),
            LifetimeEventKind::Durational {
                kind: DurationalKind::Modified,
            },
        )?,
        event_stored(
            1,
            event::Fact::DurationalDate {
                event: evt(1),
                role: DurationalRole::Started,
                bound: year_date(1900)?,
            },
        )?,
        event_stored(
            2,
            event::Fact::DurationalDate {
                event: evt(1),
                role: DurationalRole::Completed,
                bound: year_date(1905)?,
            },
        )?,
    ]
    .into_iter()
    .collect();

    let mut events = Vec::new();
    let mut citations = CitationMap::new();
    project_events(&facts, &mut events, &mut citations);

    assert_eq!(events.len(), 1);
    let ProjectedLifetimeEvent::Durational {
        kind,
        started_at,
        completed_at,
        ..
    } = events.first().ok_or("no event")?
    else {
        return Err("expected a durational event".into());
    };
    assert_eq!(*kind, BTreeSet::from([DurationalKind::Modified]));
    // Both endpoints populate, and from distinct claims — the bug was
    // folding both roles into one slot.
    assert_eq!(started_at.as_ref(), Some(&year_date(1900)?));
    assert_eq!(completed_at.as_ref(), Some(&year_date(1905)?));
    assert_ne!(started_at, completed_at);

    // Each role's slot cites its own fact, not the other's.
    let started_path = JsonPath::root()
        .field("events")
        .index(0)
        .field("started_at");
    let completed_path = JsonPath::root()
        .field("events")
        .index(0)
        .field("completed_at");
    assert_eq!(
        citations
            .get(&started_path)
            .ok_or("no started cite")?
            .supports
            .len(),
        1
    );
    assert_eq!(
        citations
            .get(&completed_path)
            .ok_or("no completed cite")?
            .supports
            .len(),
        1
    );
    Ok(())
}

/// The kind reads off `HasEvent`, not the payloads: an event with a
/// `HasEvent { Damaged }` and no `DamageCause` payload still projects
/// `kind: Some(Damaged)`.
#[test]
fn durational_kind_reads_off_has_event_not_payload() -> TestResult {
    use crate::facts::lifecycle::{DurationalKind, DurationalRole};
    let facts: BTreeMap<FactId, _> = [
        has_event_stored(
            0,
            ent(7),
            evt(1),
            LifetimeEventKind::Durational {
                kind: DurationalKind::Damaged,
            },
        )?,
        event_stored(
            1,
            event::Fact::DurationalDate {
                event: evt(1),
                role: DurationalRole::Started,
                bound: year_date(1900)?,
            },
        )?,
    ]
    .into_iter()
    .collect();

    let mut events = Vec::new();
    let mut citations = CitationMap::new();
    project_events(&facts, &mut events, &mut citations);

    let ProjectedLifetimeEvent::Durational { kind, .. } = events.first().ok_or("no event")? else {
        return Err("expected a durational event".into());
    };
    assert_eq!(*kind, BTreeSet::from([DurationalKind::Damaged]));
    Ok(())
}

#[test]
fn point_event_has_no_location_slot() -> TestResult {
    // A designation is point-category. The Point arm carries occurred_at and
    // its kind off the HasEvent, and structurally cannot carry a location.
    let facts: BTreeMap<FactId, _> = [
        has_event_stored(
            0,
            ent(7),
            evt(1),
            LifetimeEventKind::Point {
                kind: PointKind::Designated,
            },
        )?,
        event_stored(
            1,
            event::Fact::Designation {
                event: evt(1),
                designation: "national landmark".to_owned(),
            },
        )?,
        event_stored(
            2,
            event::Fact::PointDate {
                event: evt(1),
                bound: year_date(1966)?,
            },
        )?,
    ]
    .into_iter()
    .collect();

    let mut events = Vec::new();
    let mut citations = CitationMap::new();
    project_events(&facts, &mut events, &mut citations);

    let ProjectedLifetimeEvent::Point {
        kind, occurred_at, ..
    } = events.first().ok_or("no event")?
    else {
        return Err("expected a point event".into());
    };
    assert_eq!(*kind, BTreeSet::from([PointKind::Designated]));
    assert_eq!(occurred_at.as_ref(), Some(&year_date(1966)?));
    // A location path is never addressed for a point event.
    let location_path = JsonPath::root().field("events").index(0).field("location");
    assert!(
        citations.get(&location_path).is_none(),
        "a point event has no location slot to address"
    );
    Ok(())
}
