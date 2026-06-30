use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroUsize;

use chrono::TimeZone;
use url::Url;

use super::*;
use crate::claimed::Claimed;
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
use crate::facts::event;
use crate::facts::identity::{self, OrderedDistinctPair};
use crate::facts::ids::{FactId, UserId};
use crate::facts::lifecycle::{
    DamageCause, DurationalKind, DurationalRole, LifetimeEventKind, PointKind,
};
use crate::facts::memory::{MemoryEntityId, MemoryFactStore, MemoryIds};
use crate::facts::schema::{FactPage, PageItem};
use crate::facts::store::{EntityIdOf, EventIdOf, FactStore, ImageIdOf};
use crate::facts::submit::{
    Commit as SubmitBundle, CommitAuthor, Decl, EntityIdx, EventIdx, ImageIdx, StoredFact,
    SubmitFact, commit_facts,
};
use crate::location::ConflictStatus;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type MemEntId = EntityIdOf<MemoryFactStore>;
type MemEvtId = EventIdOf<MemoryFactStore>;
type MemImgId = ImageIdOf<MemoryFactStore>;
type Lin = MemberLineage<MemEntId, MemImgId>;

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
) -> Result<crate::facts::submit::SubmitResult<MemoryIds>, Box<dyn std::error::Error>> {
    let bundle: SubmitBundle<MemoryIds> = SubmitBundle {
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

/// Look up a name entry's value record by language, for assertions that don't
/// care about the full key.
fn name_by_language<'e>(
    entity: &'e Entity<MemEntId, MemEvtId, Lin>,
    language: &str,
) -> Option<(&'e NameKey, &'e Cited<NameRecord<Lin>, Lin>)> {
    entity
        .names
        .iter()
        .find(|(key, _)| key.language.as_str() == language)
}

// ------------------------------------------------------------------
// Names — membership union, value-deduped support
// ------------------------------------------------------------------

#[tokio::test]
async fn single_name_projects_one_slot() -> TestResult {
    let store = MemoryFactStore::new();
    let result = submit(&store, 1, vec![name_fact(0, "Pantheon", "en")?]).await?;
    let id = result.entities.get(&EntityIdx(0)).ok_or("missing")?.id;

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let (_, entity) = project_entity::<MemoryFactStore, _, _>(&view, id, member_lineage).await?;

    assert_eq!(entity.names.len(), 1);
    let (key, entry) = entity.names.iter().next().ok_or("no name")?;
    assert_eq!(key.name.as_str(), "Pantheon");
    assert_eq!(key.language.as_str(), "en");
    assert_eq!(key.name_type, NameType::Common);
    // The membership key carries the one fact backing its presence.
    assert_eq!(entry.support.iter().count(), 1);
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
    let (_, entity) = project_entity::<MemoryFactStore, _, _>(&view, id, member_lineage).await?;

    assert_eq!(
        entity.names.len(),
        3,
        "the exact-triple duplicate collapses; the three distinct names stay"
    );
    let langs: BTreeSet<&str> = entity.names.keys().map(|k| k.language.as_str()).collect();
    assert_eq!(langs, BTreeSet::from(["en", "it", "fr"]));

    // The collapsed `en` key cites BOTH backing facts.
    let (_, en) = name_by_language(&entity, "en").ok_or("no en name")?;
    assert_eq!(
        en.support.iter().count(),
        2,
        "a value-deduped key cites every fact that fed it"
    );
    Ok(())
}

// ------------------------------------------------------------------
// Dates — the bracket: extent joins, consensus meets
// ------------------------------------------------------------------

#[tokio::test]
async fn two_overlapping_date_claims_tighten_the_consensus() -> TestResult {
    let store = MemoryFactStore::new();
    // "the 1920s" and "1925": 1925 sits inside the decade, so the consensus
    // (meet) is the tighter 1925 and the field is consistent.
    let decade_1920s = UncertainDate::with_precision(
        chrono::NaiveDate::from_ymd_opt(1920, 1, 1).ok_or("date")?,
        DatePrecision::Decade,
    )?;
    let year_1925 = year_date(1925)?;

    let result = submit(
        &store,
        1,
        vec![
            started_fact(0, decade_1920s.clone(), "https://a.example/src")?,
            started_fact(0, year_1925.clone(), "https://b.example/src")?,
        ],
    )
    .await?;
    let id = result.entities.get(&EntityIdx(0)).ok_or("missing")?.id;

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let (_, entity) = project_entity::<MemoryFactStore, _, _>(&view, id, member_lineage).await?;

    let started = &entity.construction.started_at;
    assert_eq!(
        started.consensus.value,
        decade_1920s.meet(&year_1925),
        "the consensus is the meet — the tighter agreed window"
    );
    assert_eq!(
        started.extent.value,
        decade_1920s.join(&year_1925),
        "the extent is the join — the widest any source allows"
    );
    assert_eq!(started.conflict(), ConflictStatus::Consistent);
    // Both facts cite the merged bound, on each end.
    assert_eq!(started.consensus.support.iter().count(), 2);
    assert_eq!(started.extent.support.iter().count(), 2);
    Ok(())
}

#[tokio::test]
async fn disjoint_date_claims_conflict_with_a_disjunction_extent() -> TestResult {
    let store = MemoryFactStore::new();
    // "the 1920s" and "1935" are genuinely disjoint: the consensus (meet) is
    // empty — an over-determined Conflict — while the extent (join) keeps both
    // intervals rather than fabricating a hull.
    let decade_1920s = UncertainDate::with_precision(
        chrono::NaiveDate::from_ymd_opt(1920, 1, 1).ok_or("date")?,
        DatePrecision::Decade,
    )?;
    let year_1935 = year_date(1935)?;

    let result = submit(
        &store,
        1,
        vec![
            started_fact(0, decade_1920s.clone(), "https://a.example/src")?,
            started_fact(0, year_1935.clone(), "https://b.example/src")?,
        ],
    )
    .await?;
    let id = result.entities.get(&EntityIdx(0)).ok_or("missing")?.id;

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let (_, entity) = project_entity::<MemoryFactStore, _, _>(&view, id, member_lineage).await?;

    let started = &entity.construction.started_at;
    assert_eq!(
        started.conflict(),
        ConflictStatus::Conflict,
        "disjoint claims over-determine the consensus to empty"
    );
    assert!(
        started.consensus.value.intervals().is_empty(),
        "the consensus meet is the empty union"
    );
    assert_eq!(
        started.extent.value.intervals().len(),
        2,
        "the extent keeps both disjoint intervals; no gap is invented"
    );
    // Whole-entity conflict surfaces the field's conflict.
    assert_eq!(entity.conflict(), ConflictStatus::Conflict);
    Ok(())
}

/// A construction-start fact citing a specific source, so two date claims from
/// distinct sources keep distinct lineage citations.
fn started_fact(
    entity_idx: usize,
    bound: UncertainDate,
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
        assertion: FactualAssertion::Construction {
            fact: bookend::Fact::Started {
                entity: EntityIdx(entity_idx),
                bound,
            },
        },
        citation,
    })
}

// ------------------------------------------------------------------
// Class union
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
    let (_, entity) = project_entity::<MemoryFactStore, _, _>(&view, a, member_lineage).await?;

    let names: BTreeSet<&str> = entity.names.keys().map(|k| k.name.as_str()).collect();
    assert_eq!(
        names,
        BTreeSet::from(["X", "Y"]),
        "projecting one member yields both members' names"
    );
    assert_eq!(
        entity.refs.len(),
        1,
        "the other member's external ref surfaces too"
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

    let retract_bundle: SubmitBundle<MemoryIds> = SubmitBundle {
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
    let (_, after) = project_entity::<MemoryFactStore, _, _>(&view_now, id, member_lineage).await?;
    assert_eq!(after.names.len(), 1, "the retracted name is gone at now()");
    let (key, _) = after.names.iter().next().ok_or("no name")?;
    assert_eq!(key.name.as_str(), "New");

    // At the pre-retraction snapshot: both names.
    let view_before = store.no_later_than(snapshot_before);
    let (_, before) =
        project_entity::<MemoryFactStore, _, _>(&view_before, id, member_lineage).await?;
    assert_eq!(
        before.names.len(),
        2,
        "before the retraction both names were active"
    );
    Ok(())
}

// ------------------------------------------------------------------
// In-band provenance — every populated field carries support; no Meta leakage
// ------------------------------------------------------------------

#[tokio::test]
async fn populated_fields_carry_factual_support() -> TestResult {
    let store = MemoryFactStore::new();
    // Two entities so a relationship target exists, linked into one class.
    // Names + a bookend + an external ref + a relation across all members.
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
    let (_, entity) = project_entity::<MemoryFactStore, _, _>(&view, a, member_lineage).await?;

    // Every populated value field carries its in-band support.
    for entry in entity.names.values() {
        assert!(
            entry.support.iter().next().is_some(),
            "each name key cites its fact"
        );
    }
    assert!(
        entity
            .construction
            .started_at
            .consensus
            .support
            .iter()
            .next()
            .is_some(),
        "the bookend start cites its fact"
    );
    let (_, ref_entry) = entity.refs.iter().next().ok_or("no ref")?;
    assert!(ref_entry.support.iter().next().is_some());
    let (_, rel_entry) = entity.relations.iter().next().ok_or("no relation")?;
    assert!(rel_entry.support.iter().next().is_some());

    // No field's support cites a meta fact — meta facts back no value. The
    // SameEntity judgment drives grouping, not a field, so every value's
    // support is factual.
    for entry in entity.names.values() {
        assert!(entry.support.iter().all(is_factual));
    }
    assert!(
        entity
            .construction
            .started_at
            .consensus
            .support
            .iter()
            .all(is_factual)
    );
    Ok(())
}

/// Whether a member-aware lineage atom cites a factual fact (vs. a judgment).
fn is_factual((_, citation): &(MemEntId, Citation<MemImgId>)) -> bool {
    matches!(citation, Citation::Factual { .. })
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
    type StubFact = StoredFact<MemoryIds>;
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
// Interior events — product shape, value-mode conflict
//
// `project_facts` is the pure fold over an entity's facts; it is exercised
// here directly on a hand-built fact set, the same shape the entity drain
// hands it.
// ------------------------------------------------------------------

type StoredEventFact = StoredFact<MemoryIds>;

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

/// Project a hand-built fact map through the member-aware lineage fold, the same
/// merge the entity entry point runs. The reacher map comes from the `HasEvent`
/// facts in the bag, exactly as the entry point derives it.
fn project(facts: &BTreeMap<FactId, StoredEventFact>) -> Entity<MemEntId, MemEvtId, Lin> {
    project_facts(facts, &event_reachers(facts), member_lineage)
}

/// Mint an entity and an interior event tied to it by a `HasEvent`, plus a
/// `DurationalDate`, then project the *entity*. The events map must populate —
/// through the real entry point, this is the entity→event hop the `HasEvent`
/// bridge makes reachable.
#[tokio::test]
async fn entity_projects_has_event_linked_event() -> TestResult {
    use crate::facts::event::Fact as EventFact;

    let store = MemoryFactStore::new();
    let bundle: SubmitBundle<MemoryIds> = SubmitBundle {
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
    let (_, entity) = project_entity::<MemoryFactStore, _, _>(&view, id, member_lineage).await?;

    assert_eq!(
        entity.events.len(),
        1,
        "the HasEvent-linked interior event is reachable through project_entity"
    );
    let (_, event) = entity.events.iter().next().ok_or("no event")?;
    let record = &event.value;
    assert_eq!(
        record.kind.consensus.value,
        Claimed::Of {
            values: BTreeSet::from([LifetimeEventKind::Durational {
                kind: DurationalKind::Damaged,
            }])
        },
        "kind reads off the HasEvent"
    );
    assert_eq!(record.started_at.consensus.value, year_date(1850)?);
    assert_eq!(
        record.cause.consensus.value,
        Claimed::Of {
            values: BTreeSet::from([DamageCause::Fire])
        },
        "the value-mode cause payload projects"
    );
    Ok(())
}

#[test]
fn durational_event_splits_start_and_completion() -> TestResult {
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

    let entity = project(&facts);
    assert_eq!(entity.events.len(), 1);
    let (_, event) = entity.events.iter().next().ok_or("no event")?;
    let record = &event.value;
    assert_eq!(
        record.kind.consensus.value,
        Claimed::Of {
            values: BTreeSet::from([LifetimeEventKind::Durational {
                kind: DurationalKind::Modified,
            }])
        }
    );
    // Both endpoints populate, and from distinct claims — the bug guarded
    // against was folding both roles into one slot.
    assert_eq!(record.started_at.consensus.value, year_date(1900)?);
    assert_eq!(record.completed_at.consensus.value, year_date(1905)?);
    assert_ne!(
        record.started_at.consensus.value,
        record.completed_at.consensus.value
    );
    // Each role's bound cites its own fact, not the other's.
    assert_eq!(record.started_at.consensus.support.iter().count(), 1);
    assert_eq!(record.completed_at.consensus.support.iter().count(), 1);
    Ok(())
}

/// The kind reads off `HasEvent`, not the payloads: an event with a
/// `HasEvent { Damaged }` and no `DamageCause` payload still projects a settled
/// `Damaged` kind.
#[test]
fn durational_kind_reads_off_has_event_not_payload() -> TestResult {
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

    let entity = project(&facts);
    let (_, event) = entity.events.iter().next().ok_or("no event")?;
    assert_eq!(
        event.value.kind.consensus.value,
        Claimed::Of {
            values: BTreeSet::from([LifetimeEventKind::Durational {
                kind: DurationalKind::Damaged,
            }])
        }
    );
    Ok(())
}

#[test]
fn point_event_projects_kind_and_payloads() -> TestResult {
    // A designation is point-category: the kind settles to `Designated` and
    // the occurred_at / designation payloads project onto their slots.
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

    let entity = project(&facts);
    let (_, event) = entity.events.iter().next().ok_or("no event")?;
    let record = &event.value;
    assert_eq!(
        record.kind.consensus.value,
        Claimed::Of {
            values: BTreeSet::from([LifetimeEventKind::Point {
                kind: PointKind::Designated,
            }])
        }
    );
    assert_eq!(record.occurred_at.consensus.value, year_date(1966)?);
    assert_eq!(
        record.designation.consensus.value,
        Claimed::Of {
            values: BTreeSet::from(["national landmark".to_owned()])
        }
    );
    Ok(())
}

/// Two sources disagree on a `Damaged` event's cause. The whole value is the
/// atom, so the meet of `{Fire}` and `{Flood}` is empty — an over-determined
/// value-mode conflict, distinct from a membership union.
#[test]
fn disagreeing_damage_cause_is_a_value_mode_conflict() -> TestResult {
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
            event::Fact::DamageCause {
                event: evt(1),
                cause: DamageCause::Fire,
            },
        )?,
        event_stored(
            2,
            event::Fact::DamageCause {
                event: evt(1),
                cause: DamageCause::Flood,
            },
        )?,
    ]
    .into_iter()
    .collect();

    let entity = project(&facts);
    let (_, event) = entity.events.iter().next().ok_or("no event")?;
    assert_eq!(
        event.value.cause.conflict(),
        ConflictStatus::Conflict,
        "different claimed causes over-determine the value"
    );
    assert_eq!(
        event.value.cause.consensus.value,
        Claimed::Of {
            values: BTreeSet::new()
        },
        "the consensus meet of disjoint singletons is empty"
    );
    // The extent records both claims.
    assert_eq!(
        event.value.cause.extent.value,
        Claimed::Of {
            values: BTreeSet::from([DamageCause::Fire, DamageCause::Flood])
        }
    );
    // The conflict propagates to the whole entity.
    assert_eq!(entity.conflict(), ConflictStatus::Conflict);
    Ok(())
}

/// A move event's destination is an unresolved reference. The consensus can't
/// be decided as empty or non-empty until the reference resolves, so the
/// location field is Pending rather than Conflict.
#[test]
fn unresolved_move_location_is_pending() -> TestResult {
    use crate::location::{LocationReference, UnresolvedLocation};

    let reference = UnresolvedLocation::Reference(LocationReference::NamedPlace {
        name: "Springfield".to_owned(),
    });
    let facts: BTreeMap<FactId, _> = [
        has_event_stored(
            0,
            ent(7),
            evt(1),
            LifetimeEventKind::Durational {
                kind: DurationalKind::Moved,
            },
        )?,
        event_stored(
            1,
            event::Fact::MovedToLocation {
                event: evt(1),
                location: reference,
            },
        )?,
    ]
    .into_iter()
    .collect();

    let entity = project(&facts);
    let (_, event) = entity.events.iter().next().ok_or("no event")?;
    assert_eq!(
        event.value.location.conflict(),
        ConflictStatus::Pending,
        "an unresolved reference leaves the location's verdict pending"
    );
    assert_eq!(entity.conflict(), ConflictStatus::Pending);
    Ok(())
}

// ------------------------------------------------------------------
// SameEntity glue — the merge judgment is carried, not dropped
//
// A `SameEntity` judgment records one `sameness` edge keyed by its endpoint
// pair, its support tagged symmetrically against both endpoints. The root
// summary is the ⊔ of every edge's support; `connecting_glue` intersects a field's
// contributing member ids against the edges: a field two merged members both
// asserted surfaces the connecting judgment; a field only one member asserted
// surfaces nothing.
// ------------------------------------------------------------------

/// The two minted member ids of a `SameEntity` class, taken from the submit
/// resolution so tests can name which id a field's support carries.
struct MergedClass {
    x: MemEntId,
    y: MemEntId,
}

/// Submit two entities sharing a construction-start date, a name on the first
/// only, and a `SameEntity` judgment linking them. The shared-date field
/// gathers both members' support; the name field only the first's.
async fn merged_class(store: &MemoryFactStore) -> Result<MergedClass, Box<dyn std::error::Error>> {
    let result = submit(
        store,
        2,
        vec![
            construction_started_fact(0, 1900)?,
            construction_started_fact(1, 1900)?,
            name_fact(0, "Hall", "en")?,
            same_entity_fact(0, 1)?,
        ],
    )
    .await?;
    let x = result.entities.get(&EntityIdx(0)).ok_or("missing x")?.id;
    let y = result.entities.get(&EntityIdx(1)).ok_or("missing y")?.id;
    Ok(MergedClass { x, y })
}

#[tokio::test]
async fn shared_field_surfaces_the_connecting_glue() -> TestResult {
    let store = MemoryFactStore::new();
    let MergedClass { x, y } = merged_class(&store).await?;

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let (_, entity) = project_entity::<MemoryFactStore, _, _>(&view, x, member_lineage).await?;

    // The construction-start field was asserted by both members, so its
    // support carries both ids and the connecting judgment is load-bearing.
    let started = &entity.construction.started_at;
    let support_ids: BTreeSet<&MemEntId> =
        started.extent.support.iter().map(|(id, _)| id).collect();
    assert_eq!(
        support_ids,
        BTreeSet::from([&x, &y]),
        "both merged members asserted the shared construction date"
    );
    let glue = connecting_glue(&entity.sameness, &started.extent.support);
    assert_eq!(
        glue.len(),
        1,
        "the field spanning both members surfaces exactly the connecting judgment"
    );
    let edge = glue.first().ok_or("no glue edge")?;
    assert_eq!(
        edge.endpoints,
        &OrderedDistinctPair::new(x, y)?,
        "the glue edge is the canonical-ordered endpoint pair"
    );
    Ok(())
}

#[tokio::test]
async fn single_member_field_has_no_glue() -> TestResult {
    let store = MemoryFactStore::new();
    let MergedClass { x, .. } = merged_class(&store).await?;

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let (_, entity) = project_entity::<MemoryFactStore, _, _>(&view, x, member_lineage).await?;

    // The name was asserted by one member only; no SameEntity edge fits
    // inside a single-id support set, so nothing is load-bearing for it.
    let (_, name) = name_by_language(&entity, "en").ok_or("no name")?;
    let name_ids: BTreeSet<&MemEntId> = name.support.iter().map(|(id, _)| id).collect();
    assert_eq!(
        name_ids,
        BTreeSet::from([&x]),
        "the name has one contributor"
    );
    assert!(
        connecting_glue(&entity.sameness, &name.support).is_empty(),
        "a single-source field has no load-bearing glue"
    );
    Ok(())
}

#[tokio::test]
async fn identity_root_accumulates_the_merge_judgment() -> TestResult {
    let store = MemoryFactStore::new();
    let MergedClass { x, y } = merged_class(&store).await?;

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let (_, entity) = project_entity::<MemoryFactStore, _, _>(&view, x, member_lineage).await?;

    // The derived root summary is the ⊔ of the class's edge supports. The one
    // SameEntity edge is tagged symmetrically, so the root carries both
    // endpoints' ids.
    let root = sameness_summary(&entity.sameness);
    let root_ids: BTreeSet<&MemEntId> = root.iter().map(|(id, _)| id).collect();
    assert_eq!(
        root_ids,
        BTreeSet::from([&x, &y]),
        "the merge judgment backs the root against both endpoints"
    );
    Ok(())
}

#[tokio::test]
async fn judgment_citation_is_live_in_provenance() -> TestResult {
    let store = MemoryFactStore::new();
    let MergedClass { x, y } = merged_class(&store).await?;

    let view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let (_, entity) = project_entity::<MemoryFactStore, _, _>(&view, x, member_lineage).await?;

    // `Citation::Judgment` reaches projected provenance only because the merge
    // no longer drops judgments: it lands on the `sameness` edge.
    let root = sameness_summary(&entity.sameness);
    assert!(
        root.iter()
            .any(|(_, c)| matches!(c, Citation::Judgment { .. })),
        "the merge judgment's citation surfaces on the derived root"
    );
    let pair = OrderedDistinctPair::new(x, y)?;
    let edge = entity.sameness.get(&pair).ok_or("no sameness edge")?;
    assert!(
        edge.support
            .iter()
            .any(|(_, c)| matches!(c, Citation::Judgment { .. })),
        "the same judgment citation backs the recorded glue edge"
    );
    Ok(())
}
