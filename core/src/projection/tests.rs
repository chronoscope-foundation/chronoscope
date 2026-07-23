use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroUsize;

use chrono::TimeZone;
use futures_util::TryStreamExt;
use url::Url;

use super::*;
use crate::algebra::semiring::Support;
use crate::date::{DatePrecision, UncertainDate};
use crate::grammar::assertions::{
    FactualAssertion, JudgmentAssertion, MetaAssertion, RetractionReason,
};
use crate::grammar::attribute::{self, EntityRelationType, NameText, NameType};
use crate::grammar::bookend;
use crate::grammar::citations::{
    Excerpt, ExternalReference, ExternalSource, FactualCitation, JudgmentSource, Justification,
    Language, MetaSource, Observer,
};
use crate::grammar::composites::SubimageRegion;
use crate::grammar::depiction::Perspective;
use crate::grammar::event;
use crate::grammar::existence;
use crate::grammar::geometry::ImageGeometry;
use crate::grammar::identity::{self, OrderedDistinctPair};
use crate::grammar::ids::{FactId, UserId};
use crate::grammar::image::ImageMedium;
use crate::grammar::lifecycle::{
    DamageCause, DurationalKind, DurationalRole, LifetimeEventKind, PointKind,
};
use crate::lifespan::{ExistenceState, Lifespan};
use crate::location::ConflictStatus;
use crate::projection::Claimed;
use crate::store::memory::{MemoryEntityId, MemoryError, MemoryFactStore, MemoryIds};
use crate::store::pagination::paginate;
use crate::store::schema::{FactPage, PageItem};
use crate::store::{EntityIdOf, EventIdOf, FactStore, ImageIdOf, SubmitCommitError};
use crate::submit::{
    Commit as SubmitBundle, CommitAuthor, DateRole, Decl, EntityIdx, EventIdx, ImageIdx,
    StoredFact, SubmitError, SubmitFact, commit_facts,
};
use proptest::prelude::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type MemEntId = EntityIdOf<MemoryFactStore>;
type MemEvtId = EventIdOf<MemoryFactStore>;
type MemImgId = ImageIdOf<MemoryFactStore>;
type Lin = MemberLineage<MemEntId, MemImgId>;
/// The image projection's lineage: a `SameArtifact` class's members are image
/// ids, so the source-id atom is an image id.
type ImgLin = MemberLineage<MemImgId, MemImgId>;

fn fixed_time() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc
        .with_ymd_and_hms(2024, 1, 1, 12, 0, 0)
        .single()
        .unwrap_or_default()
}

fn sample_citation() -> Result<FactualCitation, Box<dyn std::error::Error>> {
    factual_at("https://example.com/source")
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
                    qid: crate::external_ids::WikidataEntityId::new(qid),
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
            fact: bookend::ConstructionFact::Started {
                entity: EntityIdx(entity_idx),
                bound: year_date(year)?,
            },
        },
        citation: sample_citation()?,
    })
}

fn demolition_started_fact(
    entity_idx: usize,
    year: i32,
) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Demolition {
            fact: bookend::DemolitionFact::Started {
                entity: EntityIdx(entity_idx),
                bound: year_date(year)?,
            },
        },
        citation: sample_citation()?,
    })
}

fn demolition_completed_fact(
    entity_idx: usize,
    year: i32,
) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Demolition {
            fact: bookend::DemolitionFact::Completed {
                entity: EntityIdx(entity_idx),
                bound: year_date(year)?,
            },
        },
        citation: sample_citation()?,
    })
}

fn existence_fact(entity_idx: usize, year: i32) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Existence {
            fact: existence::Fact {
                entity: EntityIdx(entity_idx),
                at: year_date(year)?,
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

fn same_artifact_fact(a: usize, b: usize) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    Ok(SubmitFact::Judgment {
        assertion: JudgmentAssertion::Identity {
            fact: identity::Fact::same_artifact(ImageIdx(a), ImageIdx(b))?,
        },
        citation: judgment_source()?,
    })
}

/// Submit one bundle, mapping the error into a boxed string so `?` works.
async fn submit(
    store: &MemoryFactStore,
    entities: usize,
    facts: Vec<SubmitFact>,
) -> Result<crate::submit::SubmitResult<MemoryIds>, Box<dyn std::error::Error>> {
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
    entity: &'e Entity<MemEntId, MemEvtId, MemImgId, Lin>,
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

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let (_, entity) = project_entity::<MemoryFactStore, _, _>(&mut view, id, member_lineage)
        .await?
        .ok_or("known id should project")?;

    assert_eq!(entity.names.len(), 1);
    let (key, entry) = entity.names.iter().next().ok_or("no name")?;
    assert_eq!(key.name.as_str(), "Pantheon");
    assert_eq!(key.language.as_str(), "en");
    assert_eq!(key.name_type, NameType::Common);
    // The membership key carries the one fact backing its presence.
    assert_eq!(entry.support.atoms().count(), 1);
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

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let (_, entity) = project_entity::<MemoryFactStore, _, _>(&mut view, id, member_lineage)
        .await?
        .ok_or("known id should project")?;

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
        en.support.atoms().count(),
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

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let (_, entity) = project_entity::<MemoryFactStore, _, _>(&mut view, id, member_lineage)
        .await?
        .ok_or("known id should project")?;

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
    assert_eq!(started.consensus.support.atoms().count(), 2);
    assert_eq!(started.extent.support.atoms().count(), 2);
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

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let (_, entity) = project_entity::<MemoryFactStore, _, _>(&mut view, id, member_lineage)
        .await?
        .ok_or("known id should project")?;

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
            fact: bookend::ConstructionFact::Started {
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

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let (_, entity) = project_entity::<MemoryFactStore, _, _>(&mut view, a, member_lineage)
        .await?
        .ok_or("known id should project")?;

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
    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let page = view
        .all_facts_about_entity(&id, None, PAGE_SIZE)
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
    let mut view_now = store.now().await.map_err(|e| format!("{e:?}"))?;
    let (_, after) = project_entity::<MemoryFactStore, _, _>(&mut view_now, id, member_lineage)
        .await?
        .ok_or("known id should project")?;
    assert_eq!(after.names.len(), 1, "the retracted name is gone at now()");
    let (key, _) = after.names.iter().next().ok_or("no name")?;
    assert_eq!(key.name.as_str(), "New");

    // At the pre-retraction snapshot: both names.
    let mut view_before = store
        .no_later_than(snapshot_before)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let (_, before) = project_entity::<MemoryFactStore, _, _>(&mut view_before, id, member_lineage)
        .await?
        .ok_or("known id should project")?;
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
    // A SameEntity class {0,1} plus a third entity 2 the class points to. The
    // relationship 0→2 is the class's outgoing edge — it crosses the class
    // boundary, so it surfaces keyed by its external target rather than folding
    // onto a member. Names + a bookend + an external ref round out the fields.
    let result = submit(
        &store,
        3,
        vec![
            name_fact(0, "Hall", "en")?,
            construction_started_fact(0, 1900)?,
            external_ref_fact(0, 7)?,
            relationship_fact(0, 2)?,
            name_fact(1, "Annex", "en")?,
            same_entity_fact(0, 1)?,
        ],
    )
    .await?;
    let a = result.entities.get(&EntityIdx(0)).ok_or("missing a")?.id;
    let target = result
        .entities
        .get(&EntityIdx(2))
        .ok_or("missing target")?
        .id;

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let (_, entity) = project_entity::<MemoryFactStore, _, _>(&mut view, a, member_lineage)
        .await?
        .ok_or("known id should project")?;

    // Every populated value field carries its in-band support.
    for entry in entity.names.values() {
        assert!(
            entry.support.atoms().next().is_some(),
            "each name key cites its fact"
        );
    }
    assert!(
        entity
            .construction
            .started_at
            .consensus
            .support
            .atoms()
            .next()
            .is_some(),
        "the bookend start cites its fact"
    );
    let (_, ref_entry) = entity.refs.iter().next().ok_or("no ref")?;
    assert!(ref_entry.support.atoms().next().is_some());
    let (rel_key, rel_entry) = entity.relations.iter().next().ok_or("no relation")?;
    assert_eq!(
        *rel_key, target,
        "the outgoing relation is keyed by its external target, not a class member"
    );
    assert!(rel_entry.support.atoms().next().is_some());

    // No field's support cites a meta fact — meta facts back no value. The
    // SameEntity judgment drives grouping, not a field, so every value's
    // support is factual.
    for entry in entity.names.values() {
        assert!(entry.support.atoms().all(is_factual));
    }
    assert!(
        entity
            .construction
            .started_at
            .consensus
            .support
            .atoms()
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
    // Submit more names than a single page holds. paginate defaults to
    // PAGE_SIZE=256, so exercising the in-memory backend across pages would
    // need >256 facts; instead drive paginate with a small page limit against
    // the real backend.
    let mut facts = Vec::new();
    for i in 0..10 {
        facts.push(name_fact(0, &format!("name-{i}"), "en")?);
    }
    let result = submit(&store, 1, facts).await?;
    let id = result.entities.get(&EntityIdx(0)).ok_or("missing")?.id;

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let tiny: NonZeroUsize = NonZeroUsize::new(3).ok_or("nonzero")?;
    let drained: Vec<(FactId, StoredFact<MemoryIds>)> =
        paginate(&mut view, |v, cursor| async move {
            let page = v.all_facts_about_entity(&id, cursor, tiny).await?;
            let (rows, next) = page.into_parts();
            Ok::<_, MemoryError>((rows, next, v))
        })
        .map_ok(|item| (item.fact_id, item.fact))
        .try_collect()
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
            fact: StoredFact::Factual(crate::submit::result::StoredFactualFact {
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

    // Pages keyed by the cursor the drain passes in — `None` first, then each
    // page's `next_cursor`: None → item 0, resume at 1; 1 → EMPTY, resume at 2;
    // 2 → item 2, resume at 3; 3 → item 3, done.
    let pages: Vec<FactPage<StubFact, MemEntId, FactId>> = vec![
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

    let drained: Vec<(FactId, StubFact)> = paginate((), |(), cursor: Option<FactId>| {
        let page = pages.get(cursor.map_or(0, |c| c.get() as usize)).cloned();
        async move {
            let page = page
                .ok_or("stub page source: cursor out of range")
                .map_err(|e: &str| e.to_owned())?;
            let (rows, next) = page.into_parts();
            Ok::<_, String>((rows, next, ()))
        }
    })
    .map_ok(|item| (item.fact_id, item.fact))
    .try_collect()
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
// Existence lifespan — the per-fact fold behind the map's time slider
//
// Each case is a row of the denotational spec, committed as real facts and
// read back off the projection, so a failure names the semantics it broke.
// ------------------------------------------------------------------

/// One instant to ask the classifier about.
fn day(y: i32, m: u32, d: u32) -> Result<chrono::NaiveDate, &'static str> {
    chrono::NaiveDate::from_ymd_opt(y, m, d).ok_or("valid date")
}

/// Commit `facts` about a single entity and read back the lifespan its
/// projection accumulated.
async fn lifespan_of(facts: Vec<SubmitFact>) -> Result<Lifespan, Box<dyn std::error::Error>> {
    let store = MemoryFactStore::new();
    let result = submit(&store, 1, facts).await?;
    let id = result.entities.get(&EntityIdx(0)).ok_or("entity 0")?.id;
    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let (_, entity) = project_entity::<MemoryFactStore, _, _>(&mut view, id, member_lineage)
        .await
        .map_err(|e| format!("{e:?}"))?
        .ok_or("known id should project")?;
    Ok(entity.lifespan)
}

#[tokio::test]
async fn construction_and_demolition_bracket_a_green_span() -> TestResult {
    let lifespan = lifespan_of(vec![
        construction_started_fact(0, 1900)?,
        demolition_completed_fact(0, 1950)?,
    ])
    .await?;

    assert_eq!(
        lifespan.classify(day(1925, 6, 1)?),
        ExistenceState::Uncontested
    );
    assert_eq!(
        lifespan.classify(day(1899, 12, 31)?),
        ExistenceState::Absent
    );
    assert_eq!(lifespan.classify(day(1951, 1, 1)?), ExistenceState::Absent);
    Ok(())
}

/// Two sources disagree on when construction started. The later claim denies
/// the whole span before it while the hull still spans both, so the disputed
/// era reads as one contested band — the assertion-quantified deny channel. A
/// fold over the merged bracket would lose the rivalry and paint it green.
#[tokio::test]
async fn disputed_construction_starts_paint_a_contested_era() -> TestResult {
    let lifespan = lifespan_of(vec![
        construction_started_fact(0, 1900)?,
        construction_started_fact(0, 2000)?,
        existence_fact(0, 1950)?,
    ])
    .await?;

    assert_eq!(
        lifespan.classify(day(1899, 12, 31)?),
        ExistenceState::Absent
    );
    assert_eq!(
        lifespan.classify(day(1950, 1, 1)?),
        ExistenceState::Contested
    );
    assert_eq!(
        lifespan.classify(day(1999, 12, 31)?),
        ExistenceState::Contested
    );
    assert_eq!(
        lifespan.classify(day(2000, 6, 1)?),
        ExistenceState::Uncontested
    );
    assert_eq!(
        lifespan.classify(day(2001, 1, 1)?),
        ExistenceState::Presumed
    );
    Ok(())
}

/// A lone sighting confirms its own range and nothing before it; with no
/// demolition on record, existence persists forward.
#[tokio::test]
async fn a_lone_witness_is_green_then_presumed_forward() -> TestResult {
    let lifespan = lifespan_of(vec![existence_fact(0, 1950)?]).await?;

    assert_eq!(
        lifespan.classify(day(1949, 12, 31)?),
        ExistenceState::Unknown
    );
    assert_eq!(
        lifespan.classify(day(1950, 6, 1)?),
        ExistenceState::Uncontested
    );
    assert_eq!(
        lifespan.classify(day(2000, 1, 1)?),
        ExistenceState::Presumed
    );
    Ok(())
}

/// A demolition that started and never completed denies no instant, yet
/// removal-in-progress withdraws the forward presumption.
#[tokio::test]
async fn an_uncompleted_demolition_suppresses_the_forward_presumption() -> TestResult {
    let lifespan = lifespan_of(vec![demolition_started_fact(0, 1973)?]).await?;

    assert_eq!(
        lifespan.classify(day(1972, 12, 31)?),
        ExistenceState::Unknown
    );
    assert_eq!(
        lifespan.classify(day(1973, 6, 1)?),
        ExistenceState::Uncontested
    );
    assert_eq!(lifespan.classify(day(1974, 1, 1)?), ExistenceState::Unknown);
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
    crate::store::memory::MemoryEventId(id)
}

/// Wrap an event-cluster fact as a stored factual fact at the given id.
fn event_stored(
    fact_id: u64,
    fact: event::Fact<MemoryIds>,
) -> Result<(FactId, StoredEventFact), Box<dyn std::error::Error>> {
    Ok((
        FactId::new(fact_id),
        StoredFact::Factual(crate::submit::result::StoredFactualFact {
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
/// merge the entity entry point runs. The caller supplies the projected entity's
/// class — the same set the real entry point resolves from `SameEntity`. The
/// reacher map comes from the `HasEvent` facts in the bag, exactly as the entry
/// point derives it.
fn project(
    facts: &BTreeMap<FactId, StoredEventFact>,
    members: &BTreeSet<MemEntId>,
) -> Entity<MemEntId, MemEvtId, MemImgId, Lin> {
    let reachers = event_reachers(facts);
    project_facts(facts, members, &reachers, member_lineage)
}

/// Mint an entity and an interior event tied to it by a `HasEvent`, plus a
/// `DurationalDate`, then project the *entity*. The events map must populate —
/// through the real entry point, this is the entity→event hop the `HasEvent`
/// bridge makes reachable.
#[tokio::test]
async fn entity_projects_has_event_linked_event() -> TestResult {
    use crate::grammar::event::Fact as EventFact;

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

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let (_, entity) = project_entity::<MemoryFactStore, _, _>(&mut view, id, member_lineage)
        .await?
        .ok_or("known id should project")?;

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
    // The event happened to it, so the entity was there: its date witnesses
    // existence, reaching the lifespan through the same HasEvent bridge.
    assert_eq!(
        entity.lifespan.classify(day(1850, 6, 1)?),
        ExistenceState::Uncontested,
        "the interior event's date witnesses the entity's existence"
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

    let entity = project(&facts, &BTreeSet::from([ent(7)]));
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
    assert_eq!(record.started_at.consensus.support.atoms().count(), 1);
    assert_eq!(record.completed_at.consensus.support.atoms().count(), 1);
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

    let entity = project(&facts, &BTreeSet::from([ent(7)]));
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

    let entity = project(&facts, &BTreeSet::from([ent(7)]));
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

    let entity = project(&facts, &BTreeSet::from([ent(7)]));
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

    let entity = project(&facts, &BTreeSet::from([ent(7)]));
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

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let (_, entity) = project_entity::<MemoryFactStore, _, _>(&mut view, x, member_lineage)
        .await?
        .ok_or("known id should project")?;

    // The construction-start field was asserted by both members, so its
    // support carries both ids and the connecting judgment is load-bearing.
    let started = &entity.construction.started_at;
    let support_ids: BTreeSet<&MemEntId> =
        started.extent.support.atoms().map(|(id, _)| id).collect();
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

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let (_, entity) = project_entity::<MemoryFactStore, _, _>(&mut view, x, member_lineage)
        .await?
        .ok_or("known id should project")?;

    // The name was asserted by one member only; no SameEntity edge fits
    // inside a single-id support set, so nothing is load-bearing for it.
    let (_, name) = name_by_language(&entity, "en").ok_or("no name")?;
    let name_ids: BTreeSet<&MemEntId> = name.support.atoms().map(|(id, _)| id).collect();
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

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let (_, entity) = project_entity::<MemoryFactStore, _, _>(&mut view, x, member_lineage)
        .await?
        .ok_or("known id should project")?;

    // The derived root summary is the ⊔ of the class's edge supports. The one
    // SameEntity edge is tagged symmetrically, so the root carries both
    // endpoints' ids.
    let root = sameness_summary(&entity.sameness);
    let root_ids: BTreeSet<&MemEntId> = root.atoms().map(|(id, _)| id).collect();
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

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let (_, entity) = project_entity::<MemoryFactStore, _, _>(&mut view, x, member_lineage)
        .await?
        .ok_or("known id should project")?;

    // `Citation::Judgment` reaches projected provenance only because the merge
    // no longer drops judgments: it lands on the `sameness` edge.
    let root = sameness_summary(&entity.sameness);
    assert!(
        root.atoms()
            .any(|(_, c)| matches!(c, Citation::Judgment { .. })),
        "the merge judgment's citation surfaces on the derived root"
    );
    let pair = OrderedDistinctPair::new(x, y)?;
    let edge = entity.sameness.get(&pair).ok_or("no sameness edge")?;
    assert!(
        edge.support
            .atoms()
            .any(|(_, c)| matches!(c, Citation::Judgment { .. })),
        "the same judgment citation backs the recorded glue edge"
    );
    Ok(())
}

// ------------------------------------------------------------------
// Relations — incident-edge projection across the class boundary
//
// `relations` holds the class's *outgoing* edges, keyed by the target. A
// relationship Y→X mentions X, so draining X's backlinks pulls it in; keying it
// unconditionally by its target would fold Y→X onto `relations[X]`, a self-loop
// on X's own projection. The fold instead keeps an edge only when its source is
// a class member, so the edge lands on Y's projection (keyed X), not X's.
// ------------------------------------------------------------------

#[tokio::test]
async fn incoming_relationship_does_not_self_loop_the_target() -> TestResult {
    let store = MemoryFactStore::new();
    // Class {0,1} via SameEntity; entity 2 sits outside it. The edge 2→0 points
    // into the class from outside.
    let result = submit(
        &store,
        3,
        vec![same_entity_fact(0, 1)?, relationship_fact(2, 0)?],
    )
    .await?;
    let target = result.entities.get(&EntityIdx(0)).ok_or("missing 0")?.id;
    let source = result.entities.get(&EntityIdx(2)).ok_or("missing 2")?.id;

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;

    // Projecting the class the edge points *into*: nothing, no self-loop.
    let (class, projected_target) =
        project_entity::<MemoryFactStore, _, _>(&mut view, target, member_lineage)
            .await?
            .ok_or("known id should project")?;
    assert!(
        projected_target.relations.is_empty(),
        "an edge pointing into the class is the source's outgoing relation, not the target's"
    );
    assert!(
        projected_target
            .relations
            .keys()
            .all(|k| !class.members.contains(k)),
        "no relation key is a member of the projected class"
    );

    // Projecting the source end: the outgoing relation surfaces, keyed by its
    // target endpoint.
    let (_, projected_source) =
        project_entity::<MemoryFactStore, _, _>(&mut view, source, member_lineage)
            .await?
            .ok_or("known id should project")?;
    assert_eq!(
        projected_source.relations.len(),
        1,
        "the source carries its one outgoing relation"
    );
    let (key, _) = projected_source
        .relations
        .iter()
        .next()
        .ok_or("no relation")?;
    assert_eq!(*key, target, "the relation is keyed by its target endpoint");
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Edge-field projection invariants over multi-member classes and edges
    /// crossing the class boundary — the gap the per-field tests left open. For
    /// every entity touched by an edge or a merge, its projected `relations` must
    /// hold *exactly* the class's boundary-crossing outgoing targets: keyed by a
    /// non-member target, sourced from a member.
    ///
    /// The single set-equality pins three invariants at once — every key is the
    /// far endpoint of an edge whose near endpoint is in the class (far-keyed
    /// correctness), no key is itself a member (no self-loop), and an edge with
    /// neither endpoint in the class contributes nothing (no non-incident leak).
    /// Pre-fix, an edge drained through its in-class target folded onto
    /// `relations[target]` with `target` a member, breaking the no-self-loop and
    /// the equality together.
    #[test]
    fn relations_project_as_outgoing_incident_edges(
        n in 2usize..=5,
        rel_seed in prop::collection::vec((0usize..5, 0usize..5), 0..=8),
        same_seed in prop::collection::vec((0usize..5, 0usize..5), 0..=4),
    ) {
        let rels_seed: BTreeSet<(usize, usize)> = rel_seed
            .into_iter()
            .map(|(a, b)| (a % n, b % n))
            .filter(|(a, b)| a != b)
            .collect();
        let sames_seed: BTreeSet<(usize, usize)> = same_seed
            .into_iter()
            .map(|(a, b)| (a % n, b % n))
            .filter(|(a, b)| a != b)
            .map(|(a, b)| if a < b { (a, b) } else { (b, a) })
            .collect();

        // Declare only the entities an edge or a merge references — the submit
        // layer rejects an unused declaration — relabeled to a contiguous range.
        let mut touched: BTreeSet<usize> = BTreeSet::new();
        for &(a, b) in rels_seed.iter().chain(sames_seed.iter()) {
            touched.insert(a);
            touched.insert(b);
        }
        prop_assume!(!touched.is_empty());
        let remap: BTreeMap<usize, usize> = touched
            .iter()
            .enumerate()
            .map(|(slot, &old)| (old, slot))
            .collect();
        let relabel = |&(a, b): &(usize, usize)| (remap[&a], remap[&b]);
        let rels: BTreeSet<(usize, usize)> = rels_seed.iter().map(relabel).collect();
        let sames: BTreeSet<(usize, usize)> = sames_seed.iter().map(relabel).collect();
        let entity_count = touched.len();

        let mut facts: Vec<SubmitFact> = Vec::new();
        for &(f, t) in &rels {
            facts.push(relationship_fact(f, t).map_err(|e| TestCaseError::fail(format!("{e}")))?);
        }
        for &(a, b) in &sames {
            facts.push(same_entity_fact(a, b).map_err(|e| TestCaseError::fail(format!("{e}")))?);
        }

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| TestCaseError::fail(format!("runtime: {e}")))?;
        rt.block_on(async move {
            let store = MemoryFactStore::new();
            let result = submit(&store, entity_count, facts)
                .await
                .map_err(|e| TestCaseError::fail(format!("submit: {e}")))?;
            let id_of = |i: usize| result.entities.get(&EntityIdx(i)).map(|e| e.id);
            let mut view = store
                .now()
                .await
                .map_err(|e| TestCaseError::fail(format!("view: {e:?}")))?;

            for idx in 0..entity_count {
                let id = id_of(idx).ok_or_else(|| TestCaseError::fail("missing entity id"))?;
                let (class, entity) =
                    project_entity::<MemoryFactStore, _, _>(&mut view, id, member_lineage)
                        .await
                        .map_err(|e| TestCaseError::fail(format!("project: {e:?}")))?
                        .ok_or_else(|| TestCaseError::fail("known id should project"))?;
                let members = &class.members;

                let mut expected: BTreeSet<MemEntId> = BTreeSet::new();
                for &(f, t) in &rels {
                    let fid = id_of(f).ok_or_else(|| TestCaseError::fail("missing source"))?;
                    let tid = id_of(t).ok_or_else(|| TestCaseError::fail("missing target"))?;
                    if members.contains(&fid) && !members.contains(&tid) {
                        expected.insert(tid);
                    }
                }
                let actual: BTreeSet<MemEntId> = entity.relations.keys().copied().collect();

                prop_assert_eq!(
                    actual,
                    expected,
                    "relations are exactly the class's boundary-crossing outgoing targets"
                );
            }
            Ok(())
        })?;
    }
}

// ------------------------------------------------------------------
// Image projection — the SameArtifact fold over image-level facts,
// depictions, and composite edges.
//
// `project_image_facts` is the pure fold over a class's facts; the routing
// tests drive it directly on a hand-built fact set with an explicit member set,
// the same shape the image drain hands it.
// ------------------------------------------------------------------

fn img(id: u64) -> MemImgId {
    crate::store::memory::MemoryImageId(id)
}

/// A factual citation distinguished by source url, so two image facts on one
/// image keep distinct lineage atoms.
fn factual_at(url: &str) -> Result<FactualCitation, Box<dyn std::error::Error>> {
    Ok(FactualCitation::new(
        ExternalSource::Url {
            url: Url::parse(url)?,
            published: None,
        },
        vec![Excerpt::new("source-text")?],
    )?)
}

/// A judgment source for a stored depiction / composite fact.
fn stored_judgment_src() -> Result<JudgmentSource<MemImgId>, Box<dyn std::error::Error>> {
    Ok(JudgmentSource::PersonalKnowledge {
        user: UserId::new("alice"),
        justification: Justification::new("an image judgment")?,
    })
}

fn source_stored(
    fact_id: u64,
    image: MemImgId,
    url: &str,
) -> Result<(FactId, StoredEventFact), Box<dyn std::error::Error>> {
    Ok((
        FactId::new(fact_id),
        StoredFact::Factual(crate::submit::result::StoredFactualFact {
            assertion: FactualAssertion::Image {
                fact: crate::grammar::image::Fact::Source {
                    image,
                    url: Url::parse(url)?,
                },
            },
            citation: factual_at(url)?,
        }),
    ))
}

fn medium_stored(
    fact_id: u64,
    image: MemImgId,
    medium: ImageMedium,
    url: &str,
) -> Result<(FactId, StoredEventFact), Box<dyn std::error::Error>> {
    Ok((
        FactId::new(fact_id),
        StoredFact::Factual(crate::submit::result::StoredFactualFact {
            assertion: FactualAssertion::Image {
                fact: crate::grammar::image::Fact::Medium { image, medium },
            },
            citation: factual_at(url)?,
        }),
    ))
}

/// Wrap a judgment assertion as a stored judgment fact at the given id,
/// mirroring [`event_stored`] for the factual side.
fn judgment_stored(
    fact_id: u64,
    assertion: JudgmentAssertion<MemoryIds>,
) -> Result<(FactId, StoredEventFact), Box<dyn std::error::Error>> {
    Ok((
        FactId::new(fact_id),
        StoredFact::Judgment(crate::submit::result::StoredJudgmentFact {
            assertion,
            source: stored_judgment_src()?,
        }),
    ))
}

fn depiction_stored(
    fact_id: u64,
    entity: MemEntId,
    image: MemImgId,
    localization: Option<ImageGeometry>,
    perspective: Option<Perspective>,
) -> Result<(FactId, StoredEventFact), Box<dyn std::error::Error>> {
    judgment_stored(
        fact_id,
        JudgmentAssertion::Depiction {
            fact: crate::grammar::depiction::Fact {
                entity,
                image,
                localization,
                perspective,
            },
        },
    )
}

fn subimage_stored(
    fact_id: u64,
    subimage: MemImgId,
    parent: MemImgId,
    region: SubimageRegion,
) -> Result<(FactId, StoredEventFact), Box<dyn std::error::Error>> {
    judgment_stored(
        fact_id,
        JudgmentAssertion::Composite {
            fact: crate::grammar::composites::Fact::IsSubimageOf {
                subimage,
                parent,
                region,
            },
        },
    )
}

/// Fold a hand-built fact map over the member-aware lineage, the same merge the
/// image entry point runs.
fn project_img(
    facts: &BTreeMap<FactId, StoredEventFact>,
    members: &BTreeSet<MemImgId>,
) -> Image<MemEntId, MemImgId, ImgLin> {
    project_image_facts(facts, members, member_lineage)
}

/// An image with a source, a medium, and a localized depiction projects each
/// onto its slot: the url joins `urls`, the medium settles, and the depiction
/// lands in `depicts` keyed by the entity with both axes pinned.
#[test]
fn image_projects_medium_urls_and_depiction() -> TestResult {
    let geometry = ImageGeometry::bbox(0.1, 0.2, 0.4, 0.5)?;
    let facts: BTreeMap<FactId, _> = [
        source_stored(0, img(1), "https://example.com/photo.jpg")?,
        medium_stored(
            1,
            img(1),
            ImageMedium::Picture,
            "https://example.com/catalog",
        )?,
        depiction_stored(
            2,
            ent(7),
            img(1),
            Some(geometry.clone()),
            Some(Perspective::Exterior),
        )?,
    ]
    .into_iter()
    .collect();

    let image = project_img(&facts, &BTreeSet::from([img(1)]));

    assert_eq!(image.urls.len(), 1, "the source url joins urls");
    assert_eq!(
        image.medium.consensus.value,
        Claimed::Of {
            values: BTreeSet::from([ImageMedium::Picture])
        },
        "the medium settles to Picture"
    );

    assert_eq!(image.depicts.len(), 1, "the depiction keys by entity");
    let (entity_key, entry) = image.depicts.iter().next().ok_or("no depiction")?;
    assert_eq!(entity_key, &ent(7));
    assert_eq!(
        entry.value.localization.consensus.value,
        Claimed::Of {
            values: BTreeSet::from([geometry])
        },
        "the localization geometry pins the bracket"
    );
    assert_eq!(
        entry.value.perspective.consensus.value,
        Claimed::Of {
            values: BTreeSet::from([Perspective::Exterior])
        },
        "the perspective pins the bracket"
    );
    Ok(())
}

/// One `IsSubimageOf` edge routes by which end the projected class holds: the
/// parent's class records the subimage under `subimages`; the subimage's class
/// records the parent under `parent`.
#[test]
fn subimage_edge_routes_by_class_membership() -> TestResult {
    let region = SubimageRegion::rect(0.0, 0.0, 0.5, 1.0)?;
    let facts: BTreeMap<FactId, _> = [subimage_stored(0, img(2), img(1), region)?]
        .into_iter()
        .collect();

    // Projecting the parent (image 1) records image 2 as a held subimage.
    let parent_view = project_img(&facts, &BTreeSet::from([img(1)]));
    assert!(
        parent_view.parent.is_empty(),
        "the parent has no parent of its own"
    );
    assert_eq!(
        parent_view.subimages.len(),
        1,
        "the parent holds one subimage"
    );
    let (sub_key, sub_entry) = parent_view.subimages.iter().next().ok_or("no subimage")?;
    assert_eq!(sub_key, &img(2));
    assert_eq!(
        sub_entry.value.region.consensus.value,
        Claimed::Of {
            values: BTreeSet::from([region])
        }
    );

    // Projecting the subimage (image 2) records image 1 as its parent.
    let sub_view = project_img(&facts, &BTreeSet::from([img(2)]));
    assert!(
        sub_view.subimages.is_empty(),
        "the subimage holds no panels"
    );
    assert_eq!(sub_view.parent.len(), 1, "the subimage names one parent");
    let (parent_key, parent_entry) = sub_view.parent.iter().next().ok_or("no parent")?;
    assert_eq!(parent_key, &img(1));
    assert_eq!(
        parent_entry.value.region.consensus.value,
        Claimed::Of {
            values: BTreeSet::from([region])
        }
    );
    Ok(())
}

/// Two sources disagree on the medium. The whole value is the atom, so the meet
/// of `{Picture}` and `{Map}` empties — an over-determined conflict — while the
/// extent keeps both.
#[test]
fn image_medium_disagreement_is_a_value_mode_conflict() -> TestResult {
    let facts: BTreeMap<FactId, _> = [
        medium_stored(0, img(1), ImageMedium::Picture, "https://a.example/src")?,
        medium_stored(1, img(1), ImageMedium::Map, "https://b.example/src")?,
    ]
    .into_iter()
    .collect();

    let image = project_img(&facts, &BTreeSet::from([img(1)]));
    assert_eq!(
        image.medium.conflict(),
        ConflictStatus::Conflict,
        "disjoint media over-determine the consensus"
    );
    assert_eq!(
        image.medium.consensus.value,
        Claimed::Of {
            values: BTreeSet::new()
        },
        "the consensus meet of disjoint singletons is empty"
    );
    assert_eq!(
        image.medium.extent.value,
        Claimed::Of {
            values: BTreeSet::from([ImageMedium::Picture, ImageMedium::Map])
        },
        "the extent keeps both claimed media"
    );
    Ok(())
}

/// A bare depiction — no localization, no perspective — still records the
/// entity↔image membership, with both annotation axes left at the untouched
/// bracket.
#[test]
fn bare_depiction_records_membership_with_untouched_axes() -> TestResult {
    let facts: BTreeMap<FactId, _> = [depiction_stored(0, ent(7), img(1), None, None)?]
        .into_iter()
        .collect();

    let image = project_img(&facts, &BTreeSet::from([img(1)]));
    assert_eq!(
        image.depicts.len(),
        1,
        "the depiction membership is recorded"
    );
    let (_, entry) = image.depicts.iter().next().ok_or("no depiction")?;
    assert_eq!(
        entry.support.atoms().count(),
        1,
        "the membership cites the depiction fact"
    );
    assert_eq!(
        entry.value.localization.extent.support.atoms().count(),
        0,
        "an absent localization leaves its bracket untouched"
    );
    assert_eq!(
        entry.value.perspective.extent.support.atoms().count(),
        0,
        "an absent perspective leaves its bracket untouched"
    );
    Ok(())
}

/// A depiction fact mentions the entity, so the entity projection drains it and
/// folds it into `depictions`, keyed by the depicted image.
#[test]
fn entity_projection_records_depiction() -> TestResult {
    let geometry = ImageGeometry::bbox(0.1, 0.1, 0.2, 0.2)?;
    let facts: BTreeMap<FactId, _> = [depiction_stored(
        0,
        ent(7),
        img(1),
        Some(geometry.clone()),
        Some(Perspective::Interior),
    )?]
    .into_iter()
    .collect();

    let entity = project(&facts, &BTreeSet::from([ent(7)]));
    assert_eq!(
        entity.depictions.len(),
        1,
        "the depiction surfaces on the depicted entity, keyed by image"
    );
    let (image_key, entry) = entity.depictions.iter().next().ok_or("no depiction")?;
    assert_eq!(image_key, &img(1));
    assert_eq!(
        entry.value.localization.consensus.value,
        Claimed::Of {
            values: BTreeSet::from([geometry])
        }
    );
    assert_eq!(
        entry.value.perspective.consensus.value,
        Claimed::Of {
            values: BTreeSet::from([Perspective::Interior])
        }
    );
    Ok(())
}

/// End-to-end through the store: `project_image` resolves the `SameArtifact`
/// class, drains its members' backlinks, and folds the image-level facts.
#[tokio::test]
async fn project_image_drains_class_and_folds_facts() -> TestResult {
    let store = MemoryFactStore::new();
    let url = "https://example.com/sheet.jpg";
    let bundle: SubmitBundle<MemoryIds> = SubmitBundle {
        author: CommitAuthor::User(UserId::new("alice")),
        recorded_at: fixed_time(),
        entities: Vec::new(),
        events: Vec::new(),
        images: vec![Decl::Local],
        facts: [
            SubmitFact::Factual {
                assertion: FactualAssertion::Image {
                    fact: crate::grammar::image::Fact::Source {
                        image: ImageIdx(0),
                        url: Url::parse(url)?,
                    },
                },
                citation: sample_citation()?,
            },
            SubmitFact::Factual {
                assertion: FactualAssertion::Image {
                    fact: crate::grammar::image::Fact::Medium {
                        image: ImageIdx(0),
                        medium: ImageMedium::Map,
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
    let id = result.images.get(&ImageIdx(0)).ok_or("missing image")?.id;

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let (class, image) = project_image::<MemoryFactStore, _, _>(&mut view, id, member_lineage)
        .await?
        .ok_or("known id should project")?;

    assert!(
        class.members.contains(&id),
        "the class holds its own subject"
    );
    assert_eq!(image.urls.len(), 1, "the source url is drained and folded");
    assert_eq!(
        image.medium.consensus.value,
        Claimed::Of {
            values: BTreeSet::from([ImageMedium::Map])
        },
        "the medium settles to Map"
    );
    Ok(())
}

/// A `SameArtifact` judgment surfaces as a `sameness` glue edge through the real
/// `project_image` drain, mirroring the entity side's `SameEntity` glue.
#[tokio::test]
async fn project_image_records_same_artifact_glue() -> TestResult {
    let store = MemoryFactStore::new();
    let bundle: SubmitBundle<MemoryIds> = SubmitBundle {
        author: CommitAuthor::User(UserId::new("alice")),
        recorded_at: fixed_time(),
        entities: Vec::new(),
        events: Vec::new(),
        images: vec![Decl::Local, Decl::Local],
        facts: [same_artifact_fact(0, 1)?].into_iter().collect(),
    };
    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let img0 = result.images.get(&ImageIdx(0)).ok_or("missing image 0")?.id;
    let img1 = result.images.get(&ImageIdx(1)).ok_or("missing image 1")?.id;

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let (class, image) = project_image::<MemoryFactStore, _, _>(&mut view, img0, member_lineage)
        .await?
        .ok_or("known id should project")?;

    // The judgment unions both realizations into one SameArtifact class.
    assert!(
        class.members.contains(&img0) && class.members.contains(&img1),
        "the judgment merges both images into one SameArtifact class"
    );

    // The glue surfaces as exactly one sameness edge, keyed by the canonical
    // endpoint pair and carrying the judgment's provenance.
    assert_eq!(
        image.sameness.len(),
        1,
        "the single merge judgment lands exactly one glue edge"
    );
    let (edge_key, entry) = image.sameness.iter().next().ok_or("no sameness edge")?;
    assert_eq!(
        edge_key,
        &OrderedDistinctPair::new(img0, img1)?,
        "the glue edge is keyed by the canonical-ordered endpoint pair"
    );
    assert!(
        entry.support.atoms().next().is_some(),
        "the recorded glue edge carries the judgment's support"
    );
    Ok(())
}

/// A `SubjectDate` folds into the image's restrictive `subject_date` slot
/// through the real store drain. Nothing consumes the slot yet; the projection
/// is the surface later PRs read.
#[tokio::test]
async fn project_image_folds_subject_date() -> TestResult {
    let store = MemoryFactStore::new();
    let subject = year_date(1850)?;
    let bundle: SubmitBundle<MemoryIds> = SubmitBundle {
        author: CommitAuthor::User(UserId::new("alice")),
        recorded_at: fixed_time(),
        entities: Vec::new(),
        events: Vec::new(),
        images: vec![Decl::Local],
        facts: [SubmitFact::Factual {
            assertion: FactualAssertion::Image {
                fact: crate::grammar::image::Fact::SubjectDate {
                    image: ImageIdx(0),
                    bound: subject.clone(),
                },
            },
            citation: sample_citation()?,
        }]
        .into_iter()
        .collect(),
    };
    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let id = result.images.get(&ImageIdx(0)).ok_or("missing image")?.id;

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let (_, image) = project_image::<MemoryFactStore, _, _>(&mut view, id, member_lineage)
        .await?
        .ok_or("known id should project")?;

    assert_eq!(
        image.subject_date.consensus.value, subject,
        "the subject-date folds into its restrictive slot"
    );
    assert_eq!(
        image.subject_date.consensus.support.atoms().count(),
        1,
        "the folded subject-date cites its one fact"
    );
    Ok(())
}

/// A `SubjectDate` is artifact-level: asserted on one scan and glued to another
/// by `SameArtifact`, it surfaces when the class is read through the *other*
/// scan — the scan that carries no subject-date of its own.
#[tokio::test]
async fn subject_date_propagates_across_same_artifact() -> TestResult {
    let store = MemoryFactStore::new();
    let subject = year_date(1850)?;
    let bundle: SubmitBundle<MemoryIds> = SubmitBundle {
        author: CommitAuthor::User(UserId::new("alice")),
        recorded_at: fixed_time(),
        entities: Vec::new(),
        events: Vec::new(),
        images: vec![Decl::Local, Decl::Local],
        facts: [
            SubmitFact::Factual {
                assertion: FactualAssertion::Image {
                    fact: crate::grammar::image::Fact::SubjectDate {
                        image: ImageIdx(0),
                        bound: subject.clone(),
                    },
                },
                citation: sample_citation()?,
            },
            same_artifact_fact(0, 1)?,
        ]
        .into_iter()
        .collect(),
    };
    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let scan_a = result.images.get(&ImageIdx(0)).ok_or("missing scan a")?.id;
    let scan_b = result.images.get(&ImageIdx(1)).ok_or("missing scan b")?.id;

    // Read the class through scan B, which carries no SubjectDate of its own.
    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let (class, image) = project_image::<MemoryFactStore, _, _>(&mut view, scan_b, member_lineage)
        .await?
        .ok_or("known id should project")?;
    assert!(
        class.members.contains(&scan_a) && class.members.contains(&scan_b),
        "the judgment unions both scans into one artifact class"
    );
    assert_eq!(
        image.subject_date.consensus.value, subject,
        "scan A's subject-date surfaces when the class is read through scan B"
    );
    Ok(())
}

/// A `SubjectDate` carrying a disjunction (a multi-interval date) is rejected at
/// submit as `NonSingleIntervalDate`, tagged with the `ImageSubject` role — the
/// `#[date_role]` walk reaches the new payload for free.
#[tokio::test]
async fn multi_interval_subject_date_is_rejected() -> TestResult {
    let store = MemoryFactStore::new();
    // "1850 or 1870": a genuine two-interval disjunction, no storable claim.
    let disjunction = year_date(1850)?.join(&year_date(1870)?);
    assert_eq!(
        disjunction.intervals().len(),
        2,
        "the test needs a real disjunction"
    );
    let bundle: SubmitBundle<MemoryIds> = SubmitBundle {
        author: CommitAuthor::User(UserId::new("alice")),
        recorded_at: fixed_time(),
        entities: Vec::new(),
        events: Vec::new(),
        images: vec![Decl::Local],
        facts: [SubmitFact::Factual {
            assertion: FactualAssertion::Image {
                fact: crate::grammar::image::Fact::SubjectDate {
                    image: ImageIdx(0),
                    bound: disjunction,
                },
            },
            citation: sample_citation()?,
        }]
        .into_iter()
        .collect(),
    };
    let err = match commit_facts(&store, bundle).await {
        Ok(_) => return Err("a multi-interval subject-date must be rejected".into()),
        Err(e) => e,
    };
    let SubmitCommitError::Submit(batch) = err else {
        return Err(format!("expected a submit rejection, got {err:?}").into());
    };
    assert!(
        batch.iter().any(|e| matches!(
            e,
            SubmitError::NonSingleIntervalDate {
                role: DateRole::ImageSubject
            }
        )),
        "got {batch:?}"
    );
    Ok(())
}

/// End-to-end through the store: a submitted `Depiction` and an `IsSubimageOf`
/// composite both surface through the real drains. `project_image` on the parent
/// records the depicted entity under `depicts` and the panel under `subimages`;
/// `project_image` on the subimage records the parent; `project_entity` on the
/// depicted entity records the image under `depictions`.
#[tokio::test]
async fn project_drains_depiction_and_subimage_edges() -> TestResult {
    let store = MemoryFactStore::new();
    let geometry = ImageGeometry::bbox(0.1, 0.2, 0.4, 0.5)?;
    let region = SubimageRegion::rect(0.0, 0.0, 0.5, 1.0)?;
    let bundle: SubmitBundle<MemoryIds> = SubmitBundle {
        author: CommitAuthor::User(UserId::new("alice")),
        recorded_at: fixed_time(),
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: vec![Decl::Local, Decl::Local],
        facts: [
            SubmitFact::Judgment {
                assertion: JudgmentAssertion::Depiction {
                    fact: crate::grammar::depiction::Fact {
                        entity: EntityIdx(0),
                        image: ImageIdx(0),
                        localization: Some(geometry.clone()),
                        perspective: Some(Perspective::Exterior),
                    },
                },
                citation: judgment_source()?,
            },
            SubmitFact::Judgment {
                assertion: JudgmentAssertion::Composite {
                    fact: crate::grammar::composites::Fact::IsSubimageOf {
                        subimage: ImageIdx(1),
                        parent: ImageIdx(0),
                        region,
                    },
                },
                citation: judgment_source()?,
            },
        ]
        .into_iter()
        .collect(),
    };
    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let entity_id = result
        .entities
        .get(&EntityIdx(0))
        .ok_or("missing entity")?
        .id;
    let parent_id = result.images.get(&ImageIdx(0)).ok_or("missing parent")?.id;
    let subimage_id = result
        .images
        .get(&ImageIdx(1))
        .ok_or("missing subimage")?
        .id;

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;

    // The parent image: the depicted entity under `depicts`, the panel under
    // `subimages`, and no enclosing image of its own.
    let (_, parent) = project_image::<MemoryFactStore, _, _>(&mut view, parent_id, member_lineage)
        .await?
        .ok_or("known id should project")?;
    assert_eq!(
        parent.depicts.len(),
        1,
        "the depiction drains onto the image"
    );
    let (depicted, depiction) = parent.depicts.iter().next().ok_or("no depiction")?;
    assert_eq!(depicted, &entity_id);
    assert_eq!(
        depiction.value.localization.consensus.value,
        Claimed::Of {
            values: BTreeSet::from([geometry.clone()])
        },
        "the depiction's localization survives the drain"
    );
    assert_eq!(parent.subimages.len(), 1, "the composite panel drains in");
    let (sub_key, _) = parent.subimages.iter().next().ok_or("no subimage")?;
    assert_eq!(sub_key, &subimage_id);
    assert!(
        parent.parent.is_empty(),
        "the parent has no enclosing image"
    );

    // The subimage end: the parent under `parent`, no panels of its own.
    let (_, sub) = project_image::<MemoryFactStore, _, _>(&mut view, subimage_id, member_lineage)
        .await?
        .ok_or("known id should project")?;
    assert_eq!(
        sub.parent.len(),
        1,
        "the subimage names its enclosing image"
    );
    let (parent_key, _) = sub.parent.iter().next().ok_or("no parent")?;
    assert_eq!(parent_key, &parent_id);
    assert!(sub.subimages.is_empty(), "the subimage holds no panels");

    // The entity side through its own drain: the depiction keyed by image.
    let (_, entity) = project_entity::<MemoryFactStore, _, _>(&mut view, entity_id, member_lineage)
        .await?
        .ok_or("known id should project")?;
    assert_eq!(
        entity.depictions.len(),
        1,
        "the depiction drains onto the depicted entity, keyed by image"
    );
    let (image_key, _) = entity.depictions.iter().next().ok_or("no depiction")?;
    assert_eq!(image_key, &parent_id);
    Ok(())
}

/// A `Depiction` whose source observed THIS image while its `fact.image` is a
/// DIFFERENT image must not surface in this image's `depicts`. The observed image
/// rides the citation into this image's backlinks, so the drain hands the fact to
/// the fold; the gate keys on `fact.image`, not on mere reachability, so the
/// cross-image depiction is declined. The observed-image indexing this leans on
/// is itself pinned by `for_each_id_visits_observed_image_of_judgment_citation`.
#[tokio::test]
async fn observed_image_does_not_leak_a_foreign_depiction() -> TestResult {
    let store = MemoryFactStore::new();
    let bundle: SubmitBundle<MemoryIds> = SubmitBundle {
        author: CommitAuthor::User(UserId::new("alice")),
        recorded_at: fixed_time(),
        entities: vec![Decl::Local, Decl::Local],
        events: Vec::new(),
        images: vec![Decl::Local, Decl::Local],
        facts: [
            // Entity 0 depicted on image A (ImageIdx 0) — the one this image holds.
            SubmitFact::Judgment {
                assertion: JudgmentAssertion::Depiction {
                    fact: crate::grammar::depiction::Fact {
                        entity: EntityIdx(0),
                        image: ImageIdx(0),
                        localization: None,
                        perspective: None,
                    },
                },
                citation: judgment_source()?,
            },
            // Entity 1 depicted on image B (ImageIdx 1), but the observation
            // looked at image A. The observed image pulls this fact into image
            // A's backlinks even though its `fact.image` is image B.
            SubmitFact::Judgment {
                assertion: JudgmentAssertion::Depiction {
                    fact: crate::grammar::depiction::Fact {
                        entity: EntityIdx(1),
                        image: ImageIdx(1),
                        localization: None,
                        perspective: None,
                    },
                },
                citation: JudgmentSource::ImageObservation {
                    image: ImageIdx(0),
                    region: None,
                    observer: Observer::User {
                        user: UserId::new("alice"),
                        justification: None,
                    },
                },
            },
        ]
        .into_iter()
        .collect(),
    };
    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let entity_a = result
        .entities
        .get(&EntityIdx(0))
        .ok_or("missing entity 0")?
        .id;
    let entity_b = result
        .entities
        .get(&EntityIdx(1))
        .ok_or("missing entity 1")?
        .id;
    let image_a = result.images.get(&ImageIdx(0)).ok_or("missing image A")?.id;

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let (_, image) = project_image::<MemoryFactStore, _, _>(&mut view, image_a, member_lineage)
        .await?
        .ok_or("known id should project")?;

    let depicted: BTreeSet<MemEntId> = image.depicts.keys().copied().collect();
    assert_eq!(
        depicted,
        BTreeSet::from([entity_a]),
        "only the depiction whose fact.image is this image surfaces"
    );
    assert!(
        !image.depicts.contains_key(&entity_b),
        "a cross-image depiction does not leak in via the citation's observed image"
    );
    Ok(())
}

// ------------------------------------------------------------------
// project_entity_images — the paged depiction assembler
//
// Folds one walk page's raw depiction facts into typed `Depiction`s, keyed by
// image rep, without projecting the whole entity. Mirrors the depictions the
// full `project_entity` fold would surface, one page at a time.
// ------------------------------------------------------------------

/// A `Depiction` judgment naming a bundle entity/image, with an optional
/// perspective — the submit-side counterpart to `depiction_stored`.
fn depiction_submit(
    entity: EntityIdx,
    image: ImageIdx,
    perspective: Option<Perspective>,
) -> Result<SubmitFact, Box<dyn std::error::Error>> {
    Ok(SubmitFact::Judgment {
        assertion: JudgmentAssertion::Depiction {
            fact: crate::grammar::depiction::Fact {
                entity,
                image,
                localization: None,
                perspective,
            },
        },
        citation: judgment_source()?,
    })
}

#[tokio::test]
async fn project_entity_images_assembles_a_tile_per_depicted_image() -> TestResult {
    let store = MemoryFactStore::new();
    // Entity 0 is depicted by two images (distinct perspectives); entity 1 is
    // depicted by a third. The walk is entity-scoped, so entity 0's page carries
    // its two images only.
    let bundle: SubmitBundle<MemoryIds> = SubmitBundle {
        author: CommitAuthor::User(UserId::new("alice")),
        recorded_at: fixed_time(),
        entities: vec![Decl::Local, Decl::Local],
        events: Vec::new(),
        images: vec![Decl::Local, Decl::Local, Decl::Local],
        facts: [
            depiction_submit(EntityIdx(0), ImageIdx(0), Some(Perspective::Exterior))?,
            depiction_submit(EntityIdx(0), ImageIdx(1), Some(Perspective::Interior))?,
            depiction_submit(EntityIdx(1), ImageIdx(2), Some(Perspective::Exterior))?,
        ]
        .into_iter()
        .collect(),
    };
    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let entity0 = result.entities.get(&EntityIdx(0)).ok_or("missing e0")?.id;
    let img0 = result.images.get(&ImageIdx(0)).ok_or("missing i0")?.id;
    let img1 = result.images.get(&ImageIdx(1)).ok_or("missing i1")?.id;

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let (depictions, next) = project_entity_images::<MemoryFactStore, _>(
        &mut view,
        entity0,
        None,
        NonZeroUsize::new(10).ok_or("nonzero")?,
    )
    .await
    .map_err(|e| format!("{e:?}"))?;

    assert!(
        next.is_none(),
        "a page larger than the image count exhausts the walk"
    );
    let settled: BTreeMap<MemImgId, Perspective> = depictions
        .iter()
        .filter_map(|d| d.perspective.settled().map(|p| (d.other, *p)))
        .collect();
    assert_eq!(
        settled,
        BTreeMap::from([(img0, Perspective::Exterior), (img1, Perspective::Interior),]),
        "exactly the two images depicting entity 0 surface, each keyed by its \
         image rep with its settled perspective"
    );
    Ok(())
}

#[tokio::test]
async fn project_entity_images_walks_every_image_once_across_pages() -> TestResult {
    let store = MemoryFactStore::new();
    // Two images depict one entity; limit=1 forces one image per page, so the
    // resume cursor must thread each image in exactly once and then terminate.
    let bundle: SubmitBundle<MemoryIds> = SubmitBundle {
        author: CommitAuthor::User(UserId::new("alice")),
        recorded_at: fixed_time(),
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: vec![Decl::Local, Decl::Local],
        facts: [
            depiction_submit(EntityIdx(0), ImageIdx(0), Some(Perspective::Exterior))?,
            depiction_submit(EntityIdx(0), ImageIdx(1), Some(Perspective::Interior))?,
        ]
        .into_iter()
        .collect(),
    };
    let result = commit_facts(&store, bundle)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let entity0 = result.entities.get(&EntityIdx(0)).ok_or("missing e0")?.id;
    let img0 = result.images.get(&ImageIdx(0)).ok_or("missing i0")?.id;
    let img1 = result.images.get(&ImageIdx(1)).ok_or("missing i1")?.id;

    let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
    let one = NonZeroUsize::new(1).ok_or("nonzero")?;
    let mut seen: BTreeSet<MemImgId> = BTreeSet::new();
    let mut after: Option<(MemImgId, FactId)> = None;
    let mut pages = 0;
    loop {
        let (depictions, next) =
            project_entity_images::<MemoryFactStore, _>(&mut view, entity0, after, one)
                .await
                .map_err(|e| format!("{e:?}"))?;
        assert!(
            depictions.len() <= 1,
            "limit=1 caps each page at a single image, got {}",
            depictions.len()
        );
        for d in &depictions {
            assert!(
                seen.insert(d.other),
                "image {:?} surfaced on two pages",
                d.other
            );
        }
        pages += 1;
        assert!(pages <= 5, "cursor pagination failed to terminate");
        match next {
            Some(c) => after = Some(c),
            None => break,
        }
    }
    assert_eq!(
        seen,
        BTreeSet::from([img0, img1]),
        "the cursor walk covers every depicting image exactly once"
    );
    Ok(())
}
