//! Tests for the fact-store-backed entity read endpoints.
//!
//! Facts are committed directly against `ctx.app_state.facts` (there's no
//! write endpoint yet) and then read back over HTTP, exercising the real
//! Dropshot path/query extraction and JSON wire shapes — including the
//! `ServerEntityId` path-param round trip (decimal-string wire form vs. the
//! `entity-{n}` `Display` form).

use std::collections::{BTreeSet, HashMap};
use std::num::NonZeroU32;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};

use chronoscope_api_client::{
    ClickAction, Cursor, EntityId, EntityListPage, TileResponse, client::ApiError,
};
use chronoscope_core::date::{DatePrecision, UncertainDate};
use chronoscope_core::geo::{GeoPoint, Meters, mercator_x, mercator_y};
use chronoscope_core::grammar::assertions::{FactualAssertion, JudgmentAssertion};
use chronoscope_core::grammar::attribute::{self, NameText, NameType};
use chronoscope_core::grammar::bookend::{ConstructionFact, DemolitionFact};
use chronoscope_core::grammar::citations::{
    Excerpt, ExternalSource, FactualCitation, JudgmentSource, Language,
};
use chronoscope_core::grammar::depiction::{self, Perspective};
use chronoscope_core::grammar::event;
use chronoscope_core::grammar::existence;
use chronoscope_core::grammar::ids::UserId;
use chronoscope_core::grammar::image::{self, ImageMedium};
use chronoscope_core::grammar::lifecycle::{LifetimeEventKind, PointKind};
use chronoscope_core::lifespan::ExistenceState;
use chronoscope_core::location::{Location, UnresolvedLocation};
use chronoscope_core::projection::DerivationRule;
use chronoscope_core::solvers::TemporalConflictKind;
use chronoscope_core::submit::{
    Commit, CommitAuthor, Decl, EntityIdx, EventIdx, ImageIdx, SubmitFact, commit_facts,
};
use chronoscope_core::typed::{Consensus, Derivation, EventDetail, InteriorEvent, distinct_rivals};

use super::TestContext;
use crate::cdn::tests::TEST_CDN_BASE_URL;
use crate::state::{
    ResolvedImageMedia, ServerEntityId, ServerFactStore, ServerIds, ServerImageId,
    placeholder_storage_key, placeholder_thumbnail_key,
};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

/// The client-side wire id the server serializes a `ServerEntityId` as: the
/// backend id crosses the wire as its decimal string, which the typed client
/// reads back as an opaque [`EntityId`]. Bridges a committed backend id to the
/// id the typed client surfaces for the same entity.
fn wire_entity_id(id: ServerEntityId) -> EntityId {
    EntityId::new(id.0.to_string())
}

fn fixed_time() -> Result<DateTime<Utc>, &'static str> {
    Utc.with_ymd_and_hms(2024, 1, 1, 12, 0, 0)
        .single()
        .ok_or("fixed timestamp is unambiguous")
}

fn citation(url: &str) -> Result<FactualCitation, Box<dyn std::error::Error + Send + Sync>> {
    let source = ExternalSource::Url {
        url: url::Url::parse(url)?,
        published: None,
    };
    Ok(FactualCitation::new(
        source,
        vec![Excerpt::new("source-text")?],
    )?)
}

fn resolved_point(
    lat: f64,
    lon: f64,
) -> Result<UnresolvedLocation, Box<dyn std::error::Error + Send + Sync>> {
    Ok(UnresolvedLocation::Resolved(Location::circle(
        GeoPoint::new(lat, lon)?,
        Meters::try_new(10.0)?,
    )?))
}

/// Commit `commit` and return the single entity id it resolves.
async fn commit_single_entity(
    facts: &ServerFactStore,
    commit: Commit<ServerIds>,
) -> Result<ServerEntityId, Box<dyn std::error::Error + Send + Sync>> {
    let result = commit_facts(facts, commit)
        .await
        .map_err(|e| format!("{e:?}"))?;
    Ok(result
        .entities
        .get(&EntityIdx(0))
        .ok_or("entity 0 resolved")?
        .id)
}

/// Commit a fresh single-entity, single-fact-set bundle: one name and one
/// construction location. Enough to make the entity placeable (for
/// `/tiles` and `/entities`) and nameable (for `get_entity`).
async fn commit_named_entity_at(
    facts: &ServerFactStore,
    name: &str,
    lat: f64,
    lon: f64,
) -> Result<ServerEntityId, Box<dyn std::error::Error + Send + Sync>> {
    let commit = Commit::<ServerIds> {
        author: CommitAuthor::User(UserId::new("test")?),
        recorded_at: fixed_time()?,
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [
            SubmitFact::Factual {
                assertion: FactualAssertion::Attribute {
                    fact: attribute::Fact::Name {
                        entity: EntityIdx(0),
                        name: NameText::new(name)?,
                        language: Language::new("en")?,
                        name_type: NameType::Common,
                        valid_from: None,
                        valid_to: None,
                    },
                },
                citation: citation("https://example.com/name")?,
            },
            SubmitFact::Factual {
                assertion: FactualAssertion::Construction {
                    fact: ConstructionFact::Location {
                        entity: EntityIdx(0),
                        location: resolved_point(lat, lon)?,
                    },
                },
                citation: citation("https://example.com/location")?,
            },
        ]
        .into_iter()
        .collect(),
    };

    commit_single_entity(facts, commit).await
}

/// Commit a placeable entity that was built and later demolished, so a read can
/// ask whether it existed at a given instant.
async fn commit_demolished_entity_at(
    facts: &ServerFactStore,
    name: &str,
    lat: f64,
    lon: f64,
    built: i32,
    demolished: i32,
) -> Result<ServerEntityId, Box<dyn std::error::Error + Send + Sync>> {
    let commit = Commit::<ServerIds> {
        author: CommitAuthor::User(UserId::new("test")?),
        recorded_at: fixed_time()?,
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [
            SubmitFact::Factual {
                assertion: FactualAssertion::Attribute {
                    fact: attribute::Fact::Name {
                        entity: EntityIdx(0),
                        name: NameText::new(name)?,
                        language: Language::new("en")?,
                        name_type: NameType::Common,
                        valid_from: None,
                        valid_to: None,
                    },
                },
                citation: citation("https://example.com/name")?,
            },
            SubmitFact::Factual {
                assertion: FactualAssertion::Construction {
                    fact: ConstructionFact::Location {
                        entity: EntityIdx(0),
                        location: resolved_point(lat, lon)?,
                    },
                },
                citation: citation("https://example.com/location")?,
            },
            SubmitFact::Factual {
                assertion: FactualAssertion::Construction {
                    fact: ConstructionFact::Started {
                        entity: EntityIdx(0),
                        bound: year(built)?,
                    },
                },
                citation: citation("https://example.com/built")?,
            },
            SubmitFact::Factual {
                assertion: FactualAssertion::Demolition {
                    fact: DemolitionFact::Completed {
                        entity: EntityIdx(0),
                        bound: year(demolished)?,
                    },
                },
                citation: citation("https://example.com/demolished")?,
            },
        ]
        .into_iter()
        .collect(),
    };

    commit_single_entity(facts, commit).await
}

/// Commit a placeable entity built, later claimed demolished, and depicted by
/// one image whose `SubjectDate` portrays it standing in `portrayed`.
///
/// The sighting is asserted about the *image*, so a served verdict reaches it
/// only by walking the entity's depiction fan-in. Portraying the entity past the
/// claimed removal refutes the claim and reads contested there.
async fn commit_photographed_demolished_entity_at(
    facts: &ServerFactStore,
    name: &str,
    lat: f64,
    lon: f64,
    built: i32,
    demolished: i32,
    portrayed: i32,
) -> Result<ServerEntityId, Box<dyn std::error::Error + Send + Sync>> {
    let commit = Commit::<ServerIds> {
        author: CommitAuthor::User(UserId::new("test")?),
        recorded_at: fixed_time()?,
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: vec![Decl::Local],
        facts: [
            SubmitFact::Factual {
                assertion: FactualAssertion::Attribute {
                    fact: attribute::Fact::Name {
                        entity: EntityIdx(0),
                        name: NameText::new(name)?,
                        language: Language::new("en")?,
                        name_type: NameType::Common,
                        valid_from: None,
                        valid_to: None,
                    },
                },
                citation: citation("https://example.com/name")?,
            },
            SubmitFact::Factual {
                assertion: FactualAssertion::Construction {
                    fact: ConstructionFact::Location {
                        entity: EntityIdx(0),
                        location: resolved_point(lat, lon)?,
                    },
                },
                citation: citation("https://example.com/location")?,
            },
            SubmitFact::Factual {
                assertion: FactualAssertion::Construction {
                    fact: ConstructionFact::Started {
                        entity: EntityIdx(0),
                        bound: year(built)?,
                    },
                },
                citation: citation("https://example.com/built")?,
            },
            SubmitFact::Factual {
                assertion: FactualAssertion::Demolition {
                    fact: DemolitionFact::Completed {
                        entity: EntityIdx(0),
                        bound: year(demolished)?,
                    },
                },
                citation: citation("https://example.com/demolished")?,
            },
            SubmitFact::Factual {
                assertion: FactualAssertion::Image {
                    fact: image::Fact::SubjectDate {
                        image: ImageIdx(0),
                        bound: year(portrayed)?,
                    },
                },
                citation: citation("https://example.com/portrayed")?,
            },
            SubmitFact::Judgment {
                assertion: JudgmentAssertion::Depiction {
                    fact: depiction::Fact {
                        entity: EntityIdx(0),
                        image: ImageIdx(0),
                        localization: None,
                        perspective: Some(Perspective::Exterior),
                    },
                },
                citation: JudgmentSource::External {
                    source: ExternalSource::Url {
                        url: url::Url::parse("https://example.com/depiction")?,
                        published: None,
                    },
                },
            },
        ]
        .into_iter()
        .collect(),
    };

    commit_single_entity(facts, commit).await
}

/// Commit a placeable entity carrying one name per `(language, text)` pair, so a
/// read can exercise `Accept-Language` negotiation between them.
async fn commit_entity_with_names_at(
    facts: &ServerFactStore,
    names: &[(&str, &str)],
    lat: f64,
    lon: f64,
) -> Result<ServerEntityId, Box<dyn std::error::Error + Send + Sync>> {
    let named = |language: &str,
                 name: &str|
     -> Result<SubmitFact, Box<dyn std::error::Error + Send + Sync>> {
        Ok(SubmitFact::Factual {
            assertion: FactualAssertion::Attribute {
                fact: attribute::Fact::Name {
                    entity: EntityIdx(0),
                    name: NameText::new(name)?,
                    language: Language::new(language)?,
                    name_type: NameType::Common,
                    valid_from: None,
                    valid_to: None,
                },
            },
            citation: citation("https://example.com/name")?,
        })
    };
    let mut facts_vec = names
        .iter()
        .map(|(language, name)| named(language, name))
        .collect::<Result<Vec<_>, _>>()?;
    facts_vec.push(SubmitFact::Factual {
        assertion: FactualAssertion::Construction {
            fact: ConstructionFact::Location {
                entity: EntityIdx(0),
                location: resolved_point(lat, lon)?,
            },
        },
        citation: citation("https://example.com/location")?,
    });
    let commit = Commit::<ServerIds> {
        author: CommitAuthor::User(UserId::new("test")?),
        recorded_at: fixed_time()?,
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: facts_vec.into_iter().collect(),
    };

    commit_single_entity(facts, commit).await
}

fn year(y: i32) -> Result<UncertainDate, Box<dyn std::error::Error + Send + Sync>> {
    Ok(UncertainDate::with_precision(
        NaiveDate::from_ymd_opt(y, 1, 1).ok_or("valid year")?,
        DatePrecision::Year,
    )?)
}

/// Commit a named entity carrying two construction-*start* facts a few years
/// apart — the Notre-Dame pattern, where sources disagree on when building
/// began. Their disjoint intervals over-determine the start slot, so the typed
/// projection surfaces a `Conflict` with fighting rivals.
async fn commit_entity_with_conflicting_start_dates(
    facts: &ServerFactStore,
    name: &str,
    early: i32,
    late: i32,
) -> Result<ServerEntityId, Box<dyn std::error::Error + Send + Sync>> {
    let started =
        |y: i32, url: &str| -> Result<SubmitFact, Box<dyn std::error::Error + Send + Sync>> {
            Ok(SubmitFact::Factual {
                assertion: FactualAssertion::Construction {
                    fact: ConstructionFact::Started {
                        entity: EntityIdx(0),
                        bound: year(y)?,
                    },
                },
                citation: citation(url)?,
            })
        };
    let commit = Commit::<ServerIds> {
        author: CommitAuthor::User(UserId::new("test")?),
        recorded_at: fixed_time()?,
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [
            SubmitFact::Factual {
                assertion: FactualAssertion::Attribute {
                    fact: attribute::Fact::Name {
                        entity: EntityIdx(0),
                        name: NameText::new(name)?,
                        language: Language::new("en")?,
                        name_type: NameType::Common,
                        valid_from: None,
                        valid_to: None,
                    },
                },
                citation: citation("https://example.com/name")?,
            },
            started(early, "https://example.com/start-early")?,
            started(late, "https://example.com/start-late")?,
        ]
        .into_iter()
        .collect(),
    };

    commit_single_entity(facts, commit).await
}

/// Commit a named entity carrying one interior point event — a `Designated`
/// landmark — whose date sources disagree: two disjoint `PointDate` claims a few
/// years apart over-determine the event's `occurred_at` slot. The conflict rides
/// the `HasEvent` reacher/backlink projection path onto the event, so the typed
/// event's date surfaces a `Conflict` with fighting rivals.
async fn commit_entity_with_conflicting_event_dates(
    facts: &ServerFactStore,
    name: &str,
    early: i32,
    late: i32,
) -> Result<ServerEntityId, Box<dyn std::error::Error + Send + Sync>> {
    let point_date =
        |y: i32, url: &str| -> Result<SubmitFact, Box<dyn std::error::Error + Send + Sync>> {
            Ok(SubmitFact::Factual {
                assertion: FactualAssertion::Event {
                    fact: event::Fact::PointDate {
                        event: EventIdx(0),
                        bound: year(y)?,
                    },
                },
                citation: citation(url)?,
            })
        };
    let commit = Commit::<ServerIds> {
        author: CommitAuthor::User(UserId::new("test")?),
        recorded_at: fixed_time()?,
        entities: vec![Decl::Local],
        events: vec![Decl::Local],
        images: Vec::new(),
        facts: [
            SubmitFact::Factual {
                assertion: FactualAssertion::Attribute {
                    fact: attribute::Fact::Name {
                        entity: EntityIdx(0),
                        name: NameText::new(name)?,
                        language: Language::new("en")?,
                        name_type: NameType::Common,
                        valid_from: None,
                        valid_to: None,
                    },
                },
                citation: citation("https://example.com/name")?,
            },
            SubmitFact::Factual {
                assertion: FactualAssertion::Event {
                    fact: event::Fact::HasEvent {
                        entity: EntityIdx(0),
                        event: EventIdx(0),
                        kind: LifetimeEventKind::Point {
                            kind: PointKind::Designated,
                        },
                    },
                },
                citation: citation("https://example.com/designation")?,
            },
            point_date(early, "https://example.com/date-early")?,
            point_date(late, "https://example.com/date-late")?,
        ]
        .into_iter()
        .collect(),
    };

    commit_single_entity(facts, commit).await
}

/// Commit a named entity whose interior point event predates its construction:
/// a `UsageChanged` opening dated `event`, but construction starts in `started >
/// event`. Each fact holds alone; jointly they can't, so the entity-level
/// temporal solver reports one conflict — the Colosseum/Mole shape.
async fn commit_entity_with_event_before_construction(
    facts: &ServerFactStore,
    name: &str,
    started: i32,
    event: i32,
) -> Result<ServerEntityId, Box<dyn std::error::Error + Send + Sync>> {
    let commit = Commit::<ServerIds> {
        author: CommitAuthor::User(UserId::new("test")?),
        recorded_at: fixed_time()?,
        entities: vec![Decl::Local],
        events: vec![Decl::Local],
        images: Vec::new(),
        facts: [
            SubmitFact::Factual {
                assertion: FactualAssertion::Attribute {
                    fact: attribute::Fact::Name {
                        entity: EntityIdx(0),
                        name: NameText::new(name)?,
                        language: Language::new("en")?,
                        name_type: NameType::Common,
                        valid_from: None,
                        valid_to: None,
                    },
                },
                citation: citation("https://example.com/name")?,
            },
            SubmitFact::Factual {
                assertion: FactualAssertion::Construction {
                    fact: ConstructionFact::Started {
                        entity: EntityIdx(0),
                        bound: year(started)?,
                    },
                },
                citation: citation("https://example.com/inception")?,
            },
            SubmitFact::Factual {
                assertion: FactualAssertion::Event {
                    fact: event::Fact::HasEvent {
                        entity: EntityIdx(0),
                        event: EventIdx(0),
                        kind: LifetimeEventKind::Point {
                            kind: PointKind::UsageChanged,
                        },
                    },
                },
                citation: citation("https://example.com/opening")?,
            },
            SubmitFact::Factual {
                assertion: FactualAssertion::Event {
                    fact: event::Fact::PointDate {
                        event: EventIdx(0),
                        bound: year(event)?,
                    },
                },
                citation: citation("https://example.com/opening-date")?,
            },
        ]
        .into_iter()
        .collect(),
    };

    commit_single_entity(facts, commit).await
}

/// Commit a named entity attested to exist at `witness`, with no construction
/// start. The shape the derived-bound producer fills: the read infers
/// `construction ≤ witness`.
async fn commit_entity_with_existence_witness(
    facts: &ServerFactStore,
    name: &str,
    witness: i32,
) -> Result<ServerEntityId, Box<dyn std::error::Error + Send + Sync>> {
    let commit = Commit::<ServerIds> {
        author: CommitAuthor::User(UserId::new("test")?),
        recorded_at: fixed_time()?,
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [
            SubmitFact::Factual {
                assertion: FactualAssertion::Attribute {
                    fact: attribute::Fact::Name {
                        entity: EntityIdx(0),
                        name: NameText::new(name)?,
                        language: Language::new("en")?,
                        name_type: NameType::Common,
                        valid_from: None,
                        valid_to: None,
                    },
                },
                citation: citation("https://example.com/name")?,
            },
            SubmitFact::Factual {
                assertion: FactualAssertion::Existence {
                    fact: existence::Fact {
                        entity: EntityIdx(0),
                        at: year(witness)?,
                    },
                },
                citation: citation("https://example.com/witness")?,
            },
        ]
        .into_iter()
        .collect(),
    };

    commit_single_entity(facts, commit).await
}

/// Commit a named, placeable entity depicted by one image: an exterior-picture
/// depiction whose image carries a Commons `Source` URL and a `Picture` medium.
/// Exercises the image read path (`get_entity` grid, `/tiles` thumbnail).
async fn commit_entity_with_depicted_image(
    facts: &ServerFactStore,
    name: &str,
    lat: f64,
    lon: f64,
    image_source_url: &str,
) -> Result<(ServerEntityId, ServerImageId), Box<dyn std::error::Error + Send + Sync>> {
    let commit = Commit::<ServerIds> {
        author: CommitAuthor::User(UserId::new("test")?),
        recorded_at: fixed_time()?,
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: vec![Decl::Local],
        facts: [
            SubmitFact::Factual {
                assertion: FactualAssertion::Attribute {
                    fact: attribute::Fact::Name {
                        entity: EntityIdx(0),
                        name: NameText::new(name)?,
                        language: Language::new("en")?,
                        name_type: NameType::Common,
                        valid_from: None,
                        valid_to: None,
                    },
                },
                citation: citation("https://example.com/name")?,
            },
            SubmitFact::Factual {
                assertion: FactualAssertion::Construction {
                    fact: ConstructionFact::Location {
                        entity: EntityIdx(0),
                        location: resolved_point(lat, lon)?,
                    },
                },
                citation: citation("https://example.com/location")?,
            },
            SubmitFact::Factual {
                assertion: FactualAssertion::Image {
                    fact: image::Fact::Source {
                        image: ImageIdx(0),
                        url: url::Url::parse(image_source_url)?,
                    },
                },
                citation: citation("https://commons.wikimedia.org/source")?,
            },
            SubmitFact::Factual {
                assertion: FactualAssertion::Image {
                    fact: image::Fact::Medium {
                        image: ImageIdx(0),
                        medium: ImageMedium::Picture,
                    },
                },
                citation: citation("https://commons.wikimedia.org/medium")?,
            },
            SubmitFact::Judgment {
                assertion: JudgmentAssertion::Depiction {
                    fact: depiction::Fact {
                        entity: EntityIdx(0),
                        image: ImageIdx(0),
                        localization: None,
                        perspective: Some(Perspective::Exterior),
                    },
                },
                citation: JudgmentSource::External {
                    source: ExternalSource::Url {
                        url: url::Url::parse("https://commons.wikimedia.org/depiction")?,
                        published: None,
                    },
                },
            },
        ]
        .into_iter()
        .collect(),
    };

    let result = commit_facts(facts, commit)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let entity_id = result
        .entities
        .get(&EntityIdx(0))
        .ok_or("entity 0 resolved")?
        .id;
    let image_id = result
        .images
        .get(&ImageIdx(0))
        .ok_or("image 0 resolved")?
        .id;
    Ok((entity_id, image_id))
}

// ==================== get_entity ====================

#[tokio::test]
async fn get_entity_returns_the_typed_projection_for_a_known_id() -> TestResult {
    let ctx = TestContext::new().await?;
    let id = commit_named_entity_at(&ctx.app_state.facts, "Pantheon", 41.8986, 12.4769).await?;

    let detail = ctx.client.get_entity(&wire_entity_id(id)).await?;

    assert_eq!(detail.entity.id, wire_entity_id(id));
    assert!(
        detail.entity.names.iter().any(|n| n.text == "Pantheon"),
        "expected the committed name to round-trip, got {:?}",
        detail.entity.names
    );
    assert_eq!(
        detail.display_name.as_deref(),
        Some("Pantheon"),
        "with no Accept-Language the negotiated display name falls back to the only name"
    );
    Ok(())
}

#[tokio::test]
async fn get_entity_surfaces_conflicting_construction_start_as_fighting_rivals() -> TestResult {
    let ctx = TestContext::new().await?;
    // Two sources place the start of construction three years apart. The
    // over-determined start slot must carry both as fighting rivals with their
    // dates and citations — the whole point of a conflict projection.
    let id =
        commit_entity_with_conflicting_start_dates(&ctx.app_state.facts, "Notre-Dame", 1160, 1163)
            .await?;

    let detail = ctx.client.get_entity(&wire_entity_id(id)).await?;

    let construction = detail
        .entity
        .timeline
        .events()
        .iter()
        .find_map(|event| match &event.detail {
            EventDetail::Constructed { period, .. } => Some(period),
            _ => None,
        })
        .ok_or("expected a construction entry in the timeline")?;

    let Consensus::Conflict { fighting } = &construction.started.consensus else {
        return Err(format!(
            "the construction start must be a Conflict, got {:?}",
            construction.started.consensus
        )
        .into());
    };
    assert_eq!(
        fighting.len(),
        1,
        "the two disjoint start dates form one minimal fighting set"
    );
    let rivals = fighting.first().ok_or("no fighting set")?;
    assert_eq!(rivals.len().get(), 2, "the set names both rival facts");
    let rival_dates: std::collections::BTreeSet<UncertainDate> =
        rivals.iter().map(|r| r.value.clone()).collect();
    assert_eq!(
        rival_dates,
        [year(1160)?, year(1163)?].into_iter().collect(),
        "the rivals carry the two submitted start dates, deserialized from the wire"
    );
    for rival in rivals {
        assert!(
            !rival.sources.is_empty(),
            "each rival surfaces the citation behind its date"
        );
    }
    Ok(())
}

#[tokio::test]
async fn get_entity_surfaces_conflicting_event_date_as_fighting_rivals() -> TestResult {
    let ctx = TestContext::new().await?;
    // An interior event's date, unlike a bookend's, is populated through the
    // HasEvent reacher/backlink path. Two disjoint designation dates must still
    // over-determine the event's `occurred_at` slot and surface both as rivals.
    let id = commit_entity_with_conflicting_event_dates(
        &ctx.app_state.facts,
        "Designated Landmark",
        1900,
        1905,
    )
    .await?;

    let detail = ctx.client.get_entity(&wire_entity_id(id)).await?;

    let at = detail
        .entity
        .timeline
        .events()
        .iter()
        .find_map(|event| match &event.detail {
            EventDetail::Interior {
                kind: InteriorEvent::Designated { at, .. },
                ..
            } => Some(at),
            _ => None,
        })
        .ok_or("expected a designated interior event in the timeline")?;

    let Consensus::Conflict { fighting } = &at.consensus else {
        return Err(format!(
            "the interior event's date must be a Conflict, got {:?}",
            at.consensus
        )
        .into());
    };
    assert_eq!(
        distinct_rivals(fighting).len(),
        2,
        "both disjoint designation dates surface as fighting rivals"
    );
    Ok(())
}

#[tokio::test]
async fn get_entity_surfaces_a_temporal_conflict_for_an_event_before_construction() -> TestResult {
    let ctx = TestContext::new().await?;
    // Construction starts in 82; an opening dated 81 can't precede it. Each date
    // is fine on its own field — the contradiction lives across them, so it rides
    // the entity-level `temporal_conflicts` channel, not a per-slot dispute.
    let id =
        commit_entity_with_event_before_construction(&ctx.app_state.facts, "Colosseum", 82, 81)
            .await?;

    let detail = ctx.client.get_entity(&wire_entity_id(id)).await?;

    assert_eq!(
        detail.temporal_conflicts.len(),
        1,
        "the event-before-construction pair is one entity-level conflict, got {:?}",
        detail.temporal_conflicts
    );
    let conflict = detail.temporal_conflicts.first().ok_or("one conflict")?;
    assert_eq!(
        conflict.facts.len().get(),
        2,
        "the conflict names the event's date fact and the construction-start fact"
    );
    // The structured kind survives the serde round-trip through the client,
    // carrying the 82 construction floor the 81 opening predates.
    match &conflict.kind {
        TemporalConflictKind::ExistedBeforeConstruction {
            construction_started,
            ..
        } => assert_eq!(
            *construction_started,
            NaiveDate::from_ymd_opt(82, 1, 1).ok_or("valid date")?,
            "the kind carries the 82 construction floor"
        ),
        other => return Err(format!("expected a before-construction kind, got {other:?}").into()),
    }
    Ok(())
}

#[tokio::test]
async fn get_entity_infers_a_built_by_bound_from_an_existence_witness() -> TestResult {
    let ctx = TestContext::new().await?;
    // The Colosseum shape: attested existing in 81, no construction date. The read
    // fills the empty construction row with an inferred "before 81" bound, marked
    // derived and sourced to the witness — not a conflict.
    let id = commit_entity_with_existence_witness(&ctx.app_state.facts, "Colosseum", 81).await?;

    let detail = ctx.client.get_entity(&wire_entity_id(id)).await?;

    let construction = detail
        .entity
        .timeline
        .events()
        .iter()
        .find_map(|event| match &event.detail {
            EventDetail::Constructed { period, .. } => Some(period),
            _ => None,
        })
        .ok_or("expected an inferred construction entry in the timeline")?;

    match &construction.started.derivation {
        Derivation::Inferred { rules, evidence } => {
            assert_eq!(
                rules.iter().copied().collect::<Vec<_>>(),
                vec![DerivationRule::ExistenceWitness],
                "the inferred start is marked derived"
            );
            assert_eq!(
                evidence.len(),
                1,
                "and cites the witness it read, over the wire"
            );
        }
        other => return Err(format!("expected an inferred start, got {other:?}").into()),
    }
    // "before 81": open below, topping out at the end of the witnessed year, so
    // the one-sided bound round-trips through the client.
    assert_eq!(
        construction.started.possible.earliest(),
        None,
        "the built-by bound is open below"
    );
    assert_eq!(
        construction.started.possible.latest(),
        NaiveDate::from_ymd_opt(81, 12, 31),
        "the built-by bound tops out at the end of the witnessed year"
    );
    assert!(
        construction.started.facts.is_empty(),
        "and names no fact of its own — no source claimed a construction start"
    );
    assert!(
        detail.temporal_conflicts.is_empty(),
        "a witness with no bookend infers a bound, not a conflict, got {:?}",
        detail.temporal_conflicts
    );
    Ok(())
}

#[tokio::test]
async fn get_entity_defaults_display_name_to_english_without_accept_language() -> TestResult {
    let ctx = TestContext::new().await?;
    // The names sort `Firenze` (it) before `Florence` (en), so the first-listed
    // name is Italian. A header-less client must still get the English default.
    let id = commit_entity_with_names_at(
        &ctx.app_state.facts,
        &[("en", "Florence"), ("it", "Firenze")],
        43.7731,
        11.2560,
    )
    .await?;

    let detail = ctx.client.get_entity(&wire_entity_id(id)).await?;

    assert_eq!(
        detail.display_name.as_deref(),
        Some("Florence"),
        "a header-less request falls back to the English name, not the first-listed Italian one"
    );
    Ok(())
}

/// Media keys a fact-store image resolves to — the `dev` resolver's actual
/// placeholder layout, through the shared key functions, so the fixture
/// cannot drift from what the resolver writes.
fn resolved_media(image_id: ServerImageId) -> ResolvedImageMedia {
    ResolvedImageMedia {
        storage_key: placeholder_storage_key(image_id),
        thumbnail_key: placeholder_thumbnail_key(image_id),
    }
}

// ==================== get_entity_images ====================

/// A default page size for image-grid reads: well above the per-test image
/// counts, so one page exhausts the walk unless a test pages deliberately.
fn image_page_size() -> Result<NonZeroU32, &'static str> {
    NonZeroU32::new(50).ok_or("nonzero image page size")
}

#[tokio::test]
async fn get_entity_images_is_empty_for_an_entity_with_no_depiction() -> TestResult {
    let ctx = TestContext::new().await?;
    let id = commit_named_entity_at(&ctx.app_state.facts, "Pantheon", 41.8986, 12.4769).await?;

    let page = ctx
        .client
        .get_entity_images(&wire_entity_id(id), image_page_size()?, None, None)
        .await?;
    assert!(
        page.images.is_empty(),
        "an entity with no depiction carries no image tiles, got {:?}",
        page.images
    );
    assert!(page.next.is_none(), "an empty grid has no next page");
    Ok(())
}

#[tokio::test]
async fn get_entity_images_resolves_a_depicted_image_into_a_tile() -> TestResult {
    let (facts, facts_dir) = super::fresh_fact_store().await?;
    let src = "https://upload.wikimedia.org/wikipedia/commons/a/a1/Pantheon.jpg";
    let (id, image_id) =
        commit_entity_with_depicted_image(&facts, "Pantheon", 41.8986, 12.4769, src).await?;

    // The depicted image is resolved into media; the read path serves its
    // original from our own /media/{key}, not from upstream Commons.
    let media = resolved_media(image_id);
    let expected_display = format!("{TEST_CDN_BASE_URL}/{}", media.storage_key);
    let ctx = TestContext::with_facts_and_image_media(
        facts,
        facts_dir,
        HashMap::from([(image_id, media)]),
    )
    .await?;

    let page = ctx
        .client
        .get_entity_images(&wire_entity_id(id), image_page_size()?, None, None)
        .await?;

    let image = page.images.first().ok_or("expected one detail image")?;
    assert_eq!(
        image.source_url.as_str(),
        src,
        "source_url preserves the real Commons URL for the lightbox link"
    );
    assert_eq!(
        image.display_url.as_str(),
        expected_display.as_str(),
        "display_url serves the resolved original from our own media host"
    );
    assert_eq!(
        image.perspective,
        Some(Perspective::Exterior),
        "the settled depiction perspective rides the wire structured, not as prose"
    );
    assert_eq!(
        image.medium,
        Some(ImageMedium::Picture),
        "the settled image medium rides the wire structured, not as prose"
    );
    assert!(page.next.is_none(), "one image exhausts the walk");
    Ok(())
}

#[tokio::test]
async fn get_entity_images_skips_a_depiction_whose_image_is_unresolved() -> TestResult {
    // The image has a Source fact but no entry in the media map — a tile that
    // can't load is worse than an absent one, so the grid drops it.
    let (facts, facts_dir) = super::fresh_fact_store().await?;
    let src = "https://upload.wikimedia.org/wikipedia/commons/c/c3/Unresolved.jpg";
    let (id, _image_id) =
        commit_entity_with_depicted_image(&facts, "Unresolved", 41.9, 12.5, src).await?;

    let ctx = TestContext::with_facts_and_image_media(facts, facts_dir, HashMap::new()).await?;

    let page = ctx
        .client
        .get_entity_images(&wire_entity_id(id), image_page_size()?, None, None)
        .await?;
    assert!(
        page.images.is_empty(),
        "an unresolved depiction contributes no grid tile, got {:?}",
        page.images
    );
    Ok(())
}

/// Commit a named, placeable entity depicted by `count` distinct images, each
/// carrying a Commons `Source` URL and a `Picture` medium. Returns the entity id
/// and its image ids in declaration order. Drives the paginated image grid.
async fn commit_entity_with_depicted_images(
    facts: &ServerFactStore,
    name: &str,
    lat: f64,
    lon: f64,
    count: usize,
) -> Result<(ServerEntityId, Vec<ServerImageId>), Box<dyn std::error::Error + Send + Sync>> {
    let mut submit_facts = vec![
        SubmitFact::Factual {
            assertion: FactualAssertion::Attribute {
                fact: attribute::Fact::Name {
                    entity: EntityIdx(0),
                    name: NameText::new(name)?,
                    language: Language::new("en")?,
                    name_type: NameType::Common,
                    valid_from: None,
                    valid_to: None,
                },
            },
            citation: citation("https://example.com/name")?,
        },
        SubmitFact::Factual {
            assertion: FactualAssertion::Construction {
                fact: ConstructionFact::Location {
                    entity: EntityIdx(0),
                    location: resolved_point(lat, lon)?,
                },
            },
            citation: citation("https://example.com/location")?,
        },
    ];
    for image in 0..count {
        submit_facts.push(SubmitFact::Factual {
            assertion: FactualAssertion::Image {
                fact: image::Fact::Source {
                    image: ImageIdx(image),
                    url: url::Url::parse(&format!(
                        "https://commons.wikimedia.org/img-{image}.jpg"
                    ))?,
                },
            },
            citation: citation("https://commons.wikimedia.org/source")?,
        });
        submit_facts.push(SubmitFact::Judgment {
            assertion: JudgmentAssertion::Depiction {
                fact: depiction::Fact {
                    entity: EntityIdx(0),
                    image: ImageIdx(image),
                    localization: None,
                    perspective: Some(Perspective::Exterior),
                },
            },
            citation: JudgmentSource::External {
                source: ExternalSource::Url {
                    url: url::Url::parse(&format!("https://commons.wikimedia.org/dep-{image}"))?,
                    published: None,
                },
            },
        });
    }
    let commit = Commit::<ServerIds> {
        author: CommitAuthor::User(UserId::new("test")?),
        recorded_at: fixed_time()?,
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: (0..count).map(|_| Decl::Local).collect(),
        facts: submit_facts.into_iter().collect(),
    };

    let result = commit_facts(facts, commit)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let entity_id = result
        .entities
        .get(&EntityIdx(0))
        .ok_or("entity 0 resolved")?
        .id;
    let image_ids = (0..count)
        .map(|image| {
            result
                .images
                .get(&ImageIdx(image))
                .map(|r| r.id)
                .ok_or("image resolved")
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((entity_id, image_ids))
}

/// Commit one more exterior depiction onto an existing entity, minting a fresh
/// image that carries a Commons `Source` URL. Lands an intervening write after a
/// page-1 read so a resume can prove it reads the pinned snapshot, not `now()`.
async fn commit_extra_depiction_on(
    facts: &ServerFactStore,
    entity_id: ServerEntityId,
    image_source_url: &str,
) -> Result<ServerImageId, Box<dyn std::error::Error + Send + Sync>> {
    let commit = Commit::<ServerIds> {
        author: CommitAuthor::User(UserId::new("test")?),
        recorded_at: fixed_time()?,
        entities: vec![Decl::Existing { id: entity_id }],
        events: Vec::new(),
        images: vec![Decl::Local],
        facts: [
            SubmitFact::Factual {
                assertion: FactualAssertion::Image {
                    fact: image::Fact::Source {
                        image: ImageIdx(0),
                        url: url::Url::parse(image_source_url)?,
                    },
                },
                citation: citation("https://commons.wikimedia.org/source")?,
            },
            SubmitFact::Judgment {
                assertion: JudgmentAssertion::Depiction {
                    fact: depiction::Fact {
                        entity: EntityIdx(0),
                        image: ImageIdx(0),
                        localization: None,
                        perspective: Some(Perspective::Exterior),
                    },
                },
                citation: JudgmentSource::External {
                    source: ExternalSource::Url {
                        url: url::Url::parse("https://commons.wikimedia.org/depiction-extra")?,
                        published: None,
                    },
                },
            },
        ]
        .into_iter()
        .collect(),
    };

    let result = commit_facts(facts, commit)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let image_id = result
        .images
        .get(&ImageIdx(0))
        .ok_or("extra image resolved")?
        .id;
    Ok(image_id)
}

#[tokio::test]
async fn get_entity_images_cursor_walks_every_image_exactly_once() -> TestResult {
    let (facts, facts_dir) = super::fresh_fact_store().await?;
    // Three depicted images inside one entity; limit=1 forces the walk across
    // cursor-linked pages, exercising the images cursor encode/decode round trip.
    let (id, image_ids) =
        commit_entity_with_depicted_images(&facts, "Colosseum", 41.8902, 12.4922, 3).await?;
    let media: HashMap<ServerImageId, ResolvedImageMedia> = image_ids
        .iter()
        .map(|&image_id| (image_id, resolved_media(image_id)))
        .collect();
    // The wire form of a backend image id is its decimal string, the same shape
    // the tiles carry back; compare on that rather than re-parsing.
    let expected: BTreeSet<String> = image_ids.iter().map(|id| id.0.to_string()).collect();
    let ctx = TestContext::with_facts_and_image_media(facts, facts_dir, media).await?;

    let one = NonZeroU32::new(1).ok_or("nonzero")?;

    // Page 1 caps at one image and hands back a cursor while images remain.
    let page1 = ctx
        .client
        .get_entity_images(&wire_entity_id(id), one, None, None)
        .await?;
    assert_eq!(
        page1.images.len(),
        1,
        "limit=1 caps the first page at a single image tile"
    );
    let mut seen: BTreeSet<String> = page1
        .images
        .iter()
        .map(|tile| tile.id.as_str().to_string())
        .collect();
    let mut cursor: Option<Cursor> = Some(
        page1
            .next
            .ok_or("page 1 must carry a next cursor while images remain")?,
    );

    let mut pages = 1;
    while let Some(c) = cursor.take() {
        let page = ctx
            .client
            .get_entity_images(&wire_entity_id(id), one, Some(&c), None)
            .await?;
        for tile in &page.images {
            assert!(
                seen.insert(tile.id.as_str().to_string()),
                "image {} surfaced on two different pages",
                tile.id.as_str()
            );
        }
        cursor = page.next;
        pages += 1;
        assert!(pages <= 8, "cursor pagination failed to terminate");
    }

    assert_eq!(
        seen, expected,
        "the cursor walk covers every depicting image exactly once"
    );
    Ok(())
}

#[tokio::test]
async fn get_entity_images_resume_reads_the_pinned_snapshot_despite_writes() -> TestResult {
    let (facts, facts_dir) = super::fresh_fact_store().await?;
    // Three depicted images make ≥2 pages at limit=1. Media is registered for a
    // range past those three, so a later image would resolve into a tile *if* the
    // resume erroneously read `now()` rather than the cursor's pinned snapshot.
    let (id, image_ids) =
        commit_entity_with_depicted_images(&facts, "Colosseum", 41.8902, 12.4922, 3).await?;
    let media: HashMap<ServerImageId, ResolvedImageMedia> = (0..8i64)
        .map(|i| {
            let image_id = chronoscope_db::SqlImageId(i);
            (image_id, resolved_media(image_id))
        })
        .collect();
    let expected: BTreeSet<String> = image_ids.iter().map(|id| id.0.to_string()).collect();
    let ctx = TestContext::with_facts_and_image_media(facts, facts_dir, media).await?;

    let one = NonZeroU32::new(1).ok_or("nonzero")?;

    // Page 1 pins its snapshot into the cursor it hands back.
    let page1 = ctx
        .client
        .get_entity_images(&wire_entity_id(id), one, None, None)
        .await?;
    let mut seen: BTreeSet<String> = page1
        .images
        .iter()
        .map(|tile| tile.id.as_str().to_string())
        .collect();
    let mut cursor: Option<Cursor> = Some(
        page1
            .next
            .ok_or("page 1 must carry a next cursor while images remain")?,
    );

    // A fourth depiction lands on the same entity after page 1. Its facts postdate
    // the cursor's snapshot, so the resumed walk must never surface it — a `now()`
    // read would, since its media is registered and it carries a source URL.
    let intruder = commit_extra_depiction_on(
        &ctx.app_state.facts,
        id,
        "https://commons.wikimedia.org/intruder.jpg",
    )
    .await?;

    let mut pages = 1;
    while let Some(c) = cursor.take() {
        // A snapshot-pinned cursor resumes cleanly; the typed client turns any
        // 400 into an `Err` the `?` would surface.
        let page = ctx
            .client
            .get_entity_images(&wire_entity_id(id), one, Some(&c), None)
            .await?;
        for tile in &page.images {
            assert!(
                seen.insert(tile.id.as_str().to_string()),
                "image {} surfaced on two different pages",
                tile.id.as_str()
            );
        }
        cursor = page.next;
        pages += 1;
        assert!(pages <= 8, "cursor pagination failed to terminate");
    }

    assert_eq!(
        seen, expected,
        "the resumed walk covers exactly the three images the pinned snapshot held"
    );
    assert!(
        !seen.contains(&intruder.0.to_string()),
        "the image committed after the cursor's snapshot must not surface mid-walk"
    );
    Ok(())
}

#[tokio::test]
async fn get_entity_images_404s_for_an_id_no_fact_ever_named() -> TestResult {
    // The images sub-resource mirrors `get_entity`'s "not found": an id no
    // committed fact named 404s rather than returning an empty grid.
    let ctx = TestContext::new().await?;
    // A fresh store mints entity ids from 0; this id was never declared.
    let resp = ctx.get("/entities/999999/images?limit=50").await?;
    assert_eq!(
        resp.status(),
        404,
        "an id no fact named must 404, not return an empty grid"
    );
    Ok(())
}

#[tokio::test]
async fn get_entity_images_requires_a_limit() -> TestResult {
    // `limit` is required, but declared `Option` at the deserialize layer so a
    // missing value reaches the handler and 400s there. A required `NonZeroU32`
    // would make Dropshot reject a missing limit before the handler ever runs.
    let ctx = TestContext::new().await?;
    let id = commit_named_entity_at(&ctx.app_state.facts, "Pantheon", 41.8986, 12.4769).await?;

    let resp = ctx.get(&format!("/entities/{}/images", id.0)).await?;
    assert_eq!(resp.status(), 400, "a missing limit is a bad request");
    Ok(())
}

#[tokio::test]
async fn get_entity_404s_for_an_id_no_fact_ever_named() -> TestResult {
    let ctx = TestContext::new().await?;
    // A fresh store mints entity ids from 0; this id was never declared by
    // any commit, so no fact anywhere mentions it.
    // Constructed concretely: a raw id mints only from the backend type.
    let unknown = chronoscope_db::SqlEntityId(999_999);

    match ctx.client.get_entity(&wire_entity_id(unknown)).await {
        Ok(entity) => {
            return Err(format!(
                "an unnamed id must 404, not project as an empty entity: {entity:?}"
            )
            .into());
        }
        Err(ApiError::Api { status, .. }) => assert_eq!(status, 404),
        Err(ApiError::Request(e)) => {
            return Err(format!("expected an API error, got a transport error: {e}").into());
        }
        Err(ApiError::Client(e)) => {
            return Err(format!("expected an API error, got a client error: {e}").into());
        }
    }
    Ok(())
}

#[tokio::test]
async fn get_entity_path_param_round_trips_the_numeric_wire_form() -> TestResult {
    // `ServerEntityId`'s `Display` renders the debug form `entity-{n}`, not the
    // wire form the path deserializer parses. Hitting the raw HTTP path with the
    // backend id's decimal-string form (what `Client::get_entity` sends) pins
    // that the server-side path extraction accepts it.
    let ctx = TestContext::new().await?;
    let id = commit_named_entity_at(&ctx.app_state.facts, "Bare Numeric Path", 10.0, 10.0).await?;

    let resp = ctx.get(&format!("/entities/{}", id.0)).await?;
    assert_eq!(
        resp.status(),
        200,
        "a bare numeric path segment must resolve"
    );
    Ok(())
}

// ==================== get_tile ====================

/// The container tile `(x, y)` at `z` a lat/lon falls in — the client-side
/// analogue of the server's tile projection, for asking `/tiles` about a seeded
/// point.
fn container_tile(lat: f64, lon: f64, z: u8) -> (u32, u32) {
    let n = f64::from(1u32 << z);
    let x = (mercator_x(lon) * n).floor() as u32;
    let y = (mercator_y(lat) * n).floor() as u32;
    (x, y)
}

#[tokio::test]
async fn get_tile_selects_a_lone_entity_in_its_container() -> TestResult {
    let ctx = TestContext::new().await?;
    let (lat, lon) = (41.8902, 12.4922);
    let id = commit_named_entity_at(&ctx.app_state.facts, "Colosseum", lat, lon).await?;

    let (x, y) = container_tile(lat, lon, 10);
    let response: TileResponse<EntityId> = ctx.client.fetch_tile(10, x, y, None, None).await?;

    assert_eq!(response.markers.len(), 1, "expected exactly one marker");
    let marker = &response.markers[0];
    assert_eq!(marker.id, wire_entity_id(id));
    assert_eq!(
        marker.name.as_deref(),
        Some("Colosseum"),
        "a lone entity's Select marker carries its negotiated display name"
    );
    assert!(
        marker.thumbnail_url.is_none(),
        "an undepicted entity's marker carries no thumbnail"
    );
    match &marker.click_action {
        ClickAction::Select { entity_id } => assert_eq!(*entity_id, wire_entity_id(id)),
        other => return Err(format!("expected Select, got {other:?}").into()),
    }
    Ok(())
}

#[tokio::test]
async fn get_tile_carries_a_thumbnail_for_a_depicted_entity() -> TestResult {
    let (facts, facts_dir) = super::fresh_fact_store().await?;
    let src = "https://upload.wikimedia.org/wikipedia/commons/a/a1/Pantheon.jpg";
    let (lat, lon) = (41.8986, 12.4769);
    let (id, image_id) =
        commit_entity_with_depicted_image(&facts, "Pantheon", lat, lon, src).await?;
    // The marker thumbnail serves the resolved image's *thumbnail* key from our
    // own media host, not the upstream Commons original.
    let expected_thumb = format!(
        "{TEST_CDN_BASE_URL}/{}",
        placeholder_thumbnail_key(image_id)
    );
    let ctx = TestContext::with_facts_and_image_media(
        facts,
        facts_dir,
        HashMap::from([(image_id, resolved_media(image_id))]),
    )
    .await?;

    let (x, y) = container_tile(lat, lon, 14);
    let response: TileResponse<EntityId> = ctx.client.fetch_tile(14, x, y, None, None).await?;

    assert_eq!(response.markers.len(), 1, "expected exactly one marker");
    let marker = &response.markers[0];
    assert_eq!(marker.id, wire_entity_id(id));
    assert_eq!(
        marker.thumbnail_url.as_ref().map(url::Url::as_str),
        Some(expected_thumb.as_str()),
        "a depicted lone entity's Select marker carries its representative thumbnail"
    );
    match &marker.click_action {
        ClickAction::Select { entity_id } => assert_eq!(*entity_id, wire_entity_id(id)),
        other => return Err(format!("expected Select, got {other:?}").into()),
    }
    Ok(())
}

#[tokio::test]
async fn get_tile_accepts_level_zero() -> TestResult {
    let ctx = TestContext::new().await?;
    // A tile names a bounded region at any level, so the whole world at z=0 must
    // fold its sole entity into a marker rather than reject the read.
    let id = commit_named_entity_at(&ctx.app_state.facts, "Colosseum", 41.8902, 12.4922).await?;

    let response: TileResponse<EntityId> = ctx.client.fetch_tile(0, 0, 0, None, None).await?;
    assert!(
        response.markers.iter().any(|m| m.id == wire_entity_id(id)),
        "the world tile at z=0 must surface the sole entity"
    );
    Ok(())
}

#[tokio::test]
async fn get_tile_rejects_an_off_grid_coordinate() -> TestResult {
    let ctx = TestContext::new().await?;
    // At z=2 the grid is 4×4, so x=4 names no tile — a browser-readable 400, not
    // a 500 out of the store.
    match ctx.client.fetch_tile(2, 4, 0, None, None).await {
        Ok(_) => return Err("an off-grid tile coordinate must 400".into()),
        Err(ApiError::Api { status, .. }) => assert_eq!(status, 400),
        Err(other) => return Err(format!("expected an API 400, got {other}").into()),
    }
    Ok(())
}

// ==================== get_tile: co-location & language ====================
//
// These flow one commit through the real `cells_to_markers` projection over
// `GET /tiles/{z}/{x}/{y}`: a co-located sub-tile folds to a `Disambiguate`
// whose picker orders chronologically, and every name — pin or picker entry — is
// negotiated against `Accept-Language`. Container level 14 folds sub-tiles at
// `z + CELL_DEPTH = 17`, so entities at one point always share a sub-tile.

/// Fetch the container tile a point falls in at `z` as a raw HTTP response,
/// optionally sending an `Accept-Language` header. Handing back the response
/// (not the parsed body) lets a caller read the negotiated names *and* the
/// `Vary: Accept-Language` header the tile endpoint advertises.
async fn get_tile_at_point(
    ctx: &TestContext,
    lat: f64,
    lon: f64,
    z: u8,
    accept_language: Option<&str>,
) -> reqwest::Result<reqwest::Response> {
    let (x, y) = container_tile(lat, lon, z);
    let mut req = ctx
        .client
        .reqwest_client()
        .get(ctx.url(&format!("/tiles/{z}/{x}/{y}")));
    if let Some(lang) = accept_language {
        req = req.header(reqwest::header::ACCEPT_LANGUAGE, lang);
    }
    req.send().await
}

/// The disambiguation-picker name a tile carries for `target`, read off a raw
/// `TileResponse`. `None` when no picker entry names it.
fn picker_name_for(
    response: &TileResponse<ServerEntityId>,
    target: ServerEntityId,
) -> Option<String> {
    response
        .markers
        .iter()
        .filter_map(|m| match &m.click_action {
            ClickAction::Disambiguate { entries } => Some(entries),
            _ => None,
        })
        .flatten()
        .find(|e| e.id == target)
        .and_then(|e| e.name.clone())
}

/// The top-level pin name a tile carries for `target`'s marker. `None` when no
/// marker names it.
fn marker_name_for(
    response: &TileResponse<ServerEntityId>,
    target: ServerEntityId,
) -> Option<String> {
    response
        .markers
        .iter()
        .find(|m| m.id == target)
        .and_then(|m| m.name.clone())
}

/// Commit a multi-named entity at a point plus a second entity at the identical
/// point, so the sub-tile folds to a co-located cell whose picker carries the
/// multi-named entity's negotiated name. Returns the multi-named entity's id.
async fn commit_colocated_named_entity(
    facts: &ServerFactStore,
    names: &[(&str, &str)],
    lat: f64,
    lon: f64,
) -> Result<ServerEntityId, Box<dyn std::error::Error + Send + Sync>> {
    let target = commit_entity_with_names_at(facts, names, lat, lon).await?;
    commit_named_entity_at(facts, "Colocated Neighbor", lat, lon).await?;
    Ok(target)
}

/// Commit a named, placeable entity, optionally dated with a construction-started
/// year. The date is what the co-located picker orders by; `None` leaves the
/// entity undated, so it sorts after every dated member.
async fn commit_dated_entity_at(
    facts: &ServerFactStore,
    name: &str,
    started_year: Option<i32>,
    lat: f64,
    lon: f64,
) -> Result<ServerEntityId, Box<dyn std::error::Error + Send + Sync>> {
    let mut facts_vec = vec![
        SubmitFact::Factual {
            assertion: FactualAssertion::Attribute {
                fact: attribute::Fact::Name {
                    entity: EntityIdx(0),
                    name: NameText::new(name)?,
                    language: Language::new("en")?,
                    name_type: NameType::Common,
                    valid_from: None,
                    valid_to: None,
                },
            },
            citation: citation("https://example.com/name")?,
        },
        SubmitFact::Factual {
            assertion: FactualAssertion::Construction {
                fact: ConstructionFact::Location {
                    entity: EntityIdx(0),
                    location: resolved_point(lat, lon)?,
                },
            },
            citation: citation("https://example.com/location")?,
        },
    ];
    if let Some(year) = started_year {
        let bound = UncertainDate::with_precision(
            chrono::NaiveDate::from_ymd_opt(year, 1, 1).ok_or("valid date")?,
            DatePrecision::Year,
        )?;
        facts_vec.push(SubmitFact::Factual {
            assertion: FactualAssertion::Construction {
                fact: ConstructionFact::Started {
                    entity: EntityIdx(0),
                    bound,
                },
            },
            citation: citation("https://example.com/started")?,
        });
    }
    let commit = Commit::<ServerIds> {
        author: CommitAuthor::User(UserId::new("test")?),
        recorded_at: fixed_time()?,
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: facts_vec.into_iter().collect(),
    };
    let result = commit_facts(facts, commit)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let id = result
        .entities
        .get(&EntityIdx(0))
        .ok_or("entity 0 resolved")?
        .id;
    Ok(id)
}

#[tokio::test]
async fn get_tile_disambiguates_colocated_entities() -> TestResult {
    let ctx = TestContext::new().await?;
    // Two entities declared at the exact same point share the finest tile, so the
    // cell is co-located — the picker carries both, each with its name.
    let (lat, lon) = (45.2, 12.27);
    let a = commit_named_entity_at(&ctx.app_state.facts, "Old Chapel", lat, lon).await?;
    let b = commit_named_entity_at(&ctx.app_state.facts, "New Chapel", lat, lon).await?;

    let (x, y) = container_tile(lat, lon, 14);
    let response: TileResponse<EntityId> = ctx.client.fetch_tile(14, x, y, None, None).await?;

    assert_eq!(
        response.markers.len(),
        1,
        "co-located entities collapse into one marker"
    );
    match &response.markers[0].click_action {
        ClickAction::Disambiguate { entries } => {
            let ids: BTreeSet<_> = entries.iter().map(|e| e.id.clone()).collect();
            assert_eq!(
                ids,
                [a, b].into_iter().map(wire_entity_id).collect(),
                "the picker lists both co-located members"
            );
            let names: BTreeSet<_> = entries.iter().filter_map(|e| e.name.as_deref()).collect();
            assert_eq!(
                names,
                ["New Chapel", "Old Chapel"].into_iter().collect(),
                "each picker entry carries its entity's negotiated name"
            );
        }
        other => return Err(format!("expected Disambiguate, got {other:?}").into()),
    }
    Ok(())
}

#[tokio::test]
async fn get_tile_expands_a_spread_cluster_to_its_split_level() -> TestResult {
    use chronoscope_core::geo::{quadkey, split_level};

    let ctx = TestContext::new().await?;
    // Two entities at distinct points that share one sub-tile of the container
    // (container z=10, cells fold at z + CELL_DEPTH = 13) but fall in different
    // finest (level-24) tiles. That sub-tile folds to a Cluster — a spread across
    // more than one finest tile, splittable by zoom — not a Colocated group, so
    // the marker's click action is Expand carrying the split level.
    let (lat, lon_a, lon_b) = (0.0, 12.49, 12.50);
    commit_named_entity_at(&ctx.app_state.facts, "Spread West", lat, lon_a).await?;
    commit_named_entity_at(&ctx.app_state.facts, "Spread East", lat, lon_b).await?;

    // The split level the fold derives from the two survivors' quadkey spread —
    // computed here through the same `split_level` the server folds through.
    let qa = quadkey(&GeoPoint::new(lat, lon_a)?);
    let qb = quadkey(&GeoPoint::new(lat, lon_b)?);
    let expected = split_level(qa.min(qb), qa.max(qb));

    let (x, y) = container_tile(lat, lon_a, 10);
    assert_eq!(
        container_tile(lat, lon_b, 10),
        (x, y),
        "both points must share one container tile so the read returns a single cell"
    );
    let response: TileResponse<EntityId> = ctx.client.fetch_tile(10, x, y, None, None).await?;

    assert_eq!(
        response.markers.len(),
        1,
        "the spread pair folds to one cluster marker"
    );
    match &response.markers[0].click_action {
        ClickAction::Expand { split_level } => assert_eq!(
            *split_level,
            expected.get(),
            "the cluster's Expand carries the split level of the survivors' quadkey spread"
        ),
        other => return Err(format!("expected Expand, got {other:?}").into()),
    }
    Ok(())
}

#[tokio::test]
async fn get_tile_orders_the_colocated_picker_chronologically() -> TestResult {
    let ctx = TestContext::new().await?;
    // Four entities at one point → one co-located cell. Committed out of
    // chronological order, so a bare insertion order can't pass for the sort; the
    // picker must reorder by earliest date, oldest first and the undated last.
    let (lat, lon) = (45.2, 12.27);
    let middle =
        commit_dated_entity_at(&ctx.app_state.facts, "Middle", Some(1850), lat, lon).await?;
    let undated = commit_dated_entity_at(&ctx.app_state.facts, "Undated", None, lat, lon).await?;
    let oldest =
        commit_dated_entity_at(&ctx.app_state.facts, "Oldest", Some(1700), lat, lon).await?;
    let newest =
        commit_dated_entity_at(&ctx.app_state.facts, "Newest", Some(1950), lat, lon).await?;

    let (x, y) = container_tile(lat, lon, 14);
    let response: TileResponse<EntityId> = ctx.client.fetch_tile(14, x, y, None, None).await?;

    assert_eq!(
        response.markers.len(),
        1,
        "co-located entities collapse into one marker"
    );
    match &response.markers[0].click_action {
        ClickAction::Disambiguate { entries } => {
            let order: Vec<_> = entries.iter().map(|e| e.id.clone()).collect();
            let expected: Vec<_> = [oldest, middle, newest, undated]
                .into_iter()
                .map(wire_entity_id)
                .collect();
            assert_eq!(
                order, expected,
                "the picker lists dated members oldest-first, the undated member last"
            );
        }
        other => return Err(format!("expected Disambiguate, got {other:?}").into()),
    }
    Ok(())
}

#[tokio::test]
async fn get_tile_negotiates_picker_name_by_accept_language() -> TestResult {
    let ctx = TestContext::new().await?;
    // The co-located picker exercises per-member name negotiation: the multi-named
    // entity shares its point with a neighbor, so the cell is a disambiguation
    // group whose entry for the target carries the negotiated name.
    let (lat, lon) = (43.7731, 11.2560);
    let target = commit_colocated_named_entity(
        &ctx.app_state.facts,
        &[("en", "Florence"), ("it", "Firenze")],
        lat,
        lon,
    )
    .await?;

    // `it` preference: the Italian name wins, and the response advertises Vary.
    let italian_resp = get_tile_at_point(&ctx, lat, lon, 14, Some("it")).await?;
    assert_eq!(
        italian_resp
            .headers()
            .get(reqwest::header::VARY)
            .and_then(|v| v.to_str().ok()),
        Some("Accept-Language"),
        "a language-negotiated tile advertises Vary: Accept-Language so caches don't cross-serve locales"
    );
    let italian: TileResponse<ServerEntityId> = italian_resp.json().await?;
    assert_eq!(
        picker_name_for(&italian, target).as_deref(),
        Some("Firenze"),
        "an `it` preference selects the Italian name"
    );

    // `en-US,en;q=0.9`: both entries reduce to the primary subtag `en`, so the
    // English name wins regardless of the q-weight.
    let english: TileResponse<ServerEntityId> =
        get_tile_at_point(&ctx, lat, lon, 14, Some("en-US,en;q=0.9"))
            .await?
            .json()
            .await?;
    assert_eq!(
        picker_name_for(&english, target).as_deref(),
        Some("Florence"),
        "an `en` preference selects the English name"
    );
    Ok(())
}

#[tokio::test]
async fn get_tile_matches_accept_language_case_insensitively() -> TestResult {
    let ctx = TestContext::new().await?;
    let (lat, lon) = (43.7731, 11.2560);
    let target = commit_colocated_named_entity(
        &ctx.app_state.facts,
        &[("en", "Florence"), ("it", "Firenze")],
        lat,
        lon,
    )
    .await?;

    // An uppercase `IT` must match the lowercase-canonical stored `it` tag.
    let response: TileResponse<ServerEntityId> = get_tile_at_point(&ctx, lat, lon, 14, Some("IT"))
        .await?
        .json()
        .await?;
    assert_eq!(
        picker_name_for(&response, target).as_deref(),
        Some("Firenze"),
        "an uppercase `IT` header matches the lowercase-canonical `it` tag"
    );
    Ok(())
}

#[tokio::test]
async fn get_tile_orders_accept_language_by_q_weight() -> TestResult {
    let ctx = TestContext::new().await?;
    let (lat, lon) = (48.1372, 11.5756);
    let target = commit_colocated_named_entity(
        &ctx.app_state.facts,
        &[("en", "Munich"), ("de", "München")],
        lat,
        lon,
    )
    .await?;

    // `de;q=0.5, en`: `en` carries an implicit q=1.0, outranking `de;q=0.5`
    // despite coming later in the header, so the English name wins.
    let response: TileResponse<ServerEntityId> =
        get_tile_at_point(&ctx, lat, lon, 14, Some("de;q=0.5, en"))
            .await?
            .json()
            .await?;
    assert_eq!(
        picker_name_for(&response, target).as_deref(),
        Some("Munich"),
        "the higher q-weight (`en`, implicit 1.0) wins over `de;q=0.5`"
    );
    Ok(())
}

#[tokio::test]
async fn get_tile_negotiates_singleton_name_by_accept_language() -> TestResult {
    let ctx = TestContext::new().await?;
    // A lone entity with two names — its Select marker's top-level name (from
    // marker_display) must negotiate against the header, not just the co-located
    // picker.
    let (lat, lon) = (41.8902, 12.4922);
    let target = commit_entity_with_names_at(
        &ctx.app_state.facts,
        &[("en", "Colosseum"), ("it", "Colosseo")],
        lat,
        lon,
    )
    .await?;

    // `it` preference: the Italian name wins on the Select marker itself.
    let italian: TileResponse<ServerEntityId> = get_tile_at_point(&ctx, lat, lon, 14, Some("it"))
        .await?
        .json()
        .await?;
    assert_eq!(
        marker_name_for(&italian, target).as_deref(),
        Some("Colosseo"),
        "an `it` preference selects the Italian name on the singleton Select marker"
    );

    // No header: the negotiation falls back to the English name.
    let default: TileResponse<ServerEntityId> = get_tile_at_point(&ctx, lat, lon, 14, None)
        .await?
        .json()
        .await?;
    assert_eq!(
        marker_name_for(&default, target).as_deref(),
        Some("Colosseum"),
        "with no Accept-Language the singleton marker falls back to the English name"
    );
    Ok(())
}

// ==================== get_tile: existence at an instant ====================

#[tokio::test]
async fn get_tile_reports_existence_at_the_requested_instant() -> TestResult {
    let ctx = TestContext::new().await?;
    let (lat, lon) = (51.5285, -0.1339);
    commit_demolished_entity_at(&ctx.app_state.facts, "Euston Arch", lat, lon, 1837, 1962).await?;
    let (x, y) = container_tile(lat, lon, 10);

    let during: TileResponse<EntityId> = ctx
        .client
        .fetch_tile(
            10,
            x,
            y,
            None,
            Some(NaiveDate::from_ymd_opt(1900, 6, 1).ok_or("date")?),
        )
        .await?;
    assert_eq!(
        during.markers.first().and_then(|m| m.existence),
        Some(ExistenceState::Uncontested),
        "the arch stood in 1900, and no source denies it"
    );

    let after: TileResponse<EntityId> = ctx
        .client
        .fetch_tile(
            10,
            x,
            y,
            None,
            Some(NaiveDate::from_ymd_opt(2020, 1, 1).ok_or("date")?),
        )
        .await?;
    assert_eq!(
        after.markers.first().and_then(|m| m.existence),
        Some(ExistenceState::Absent),
        "the demolition denies existence in 2020"
    );

    // No instant defaults to today, and the arch came down in 1962.
    let untimed: TileResponse<EntityId> = ctx.client.fetch_tile(10, x, y, None, None).await?;
    assert_eq!(
        untimed.markers.first().and_then(|m| m.existence),
        Some(ExistenceState::Absent),
        "an as_of-less read defaults to today, where the arch is long gone"
    );
    Ok(())
}

/// `NaiveDate`'s `Display` writes a negative year as `-0400-07-01`, and the
/// time slider reaches back to 2000 BCE, so its whole ancient band depends on
/// that form surviving the client's query string and the server's serde parse.
///
/// Two probes either side of the demolition. A param that never arrived, whether
/// dropped or rejected and defaulted, reads as today, which is also absent, so
/// only the pair pins the instant that actually landed.
#[tokio::test]
async fn get_tile_reports_existence_at_a_bce_instant() -> TestResult {
    let ctx = TestContext::new().await?;
    let (lat, lon) = (37.9715, 23.7257);
    commit_demolished_entity_at(&ctx.app_state.facts, "Old Temple", lat, lon, -500, -200).await?;
    let (x, y) = container_tile(lat, lon, 10);

    let during: TileResponse<EntityId> = ctx
        .client
        .fetch_tile(
            10,
            x,
            y,
            None,
            Some(NaiveDate::from_ymd_opt(-400, 7, 1).ok_or("date")?),
        )
        .await?;
    assert_eq!(
        during.markers.first().and_then(|m| m.existence),
        Some(ExistenceState::Uncontested),
        "the temple stood in 400 BCE, and no source denies it"
    );

    let after: TileResponse<EntityId> = ctx
        .client
        .fetch_tile(
            10,
            x,
            y,
            None,
            Some(NaiveDate::from_ymd_opt(-100, 7, 1).ok_or("date")?),
        )
        .await?;
    assert_eq!(
        after.markers.first().and_then(|m| m.existence),
        Some(ExistenceState::Absent),
        "the 200 BCE demolition denies existence in 100 BCE"
    );
    Ok(())
}

/// A shared pin shows while any co-located member may have stood, so it reports
/// the most present member's verdict.
#[tokio::test]
async fn get_tile_reports_a_colocated_pin_by_its_most_present_member() -> TestResult {
    let ctx = TestContext::new().await?;
    let (lat, lon) = (45.2, 12.27);
    commit_demolished_entity_at(&ctx.app_state.facts, "Old Hall", lat, lon, 1700, 1850).await?;
    commit_demolished_entity_at(&ctx.app_state.facts, "New Hall", lat, lon, 1860, 1990).await?;
    let (x, y) = container_tile(lat, lon, 14);

    let response: TileResponse<EntityId> = ctx
        .client
        .fetch_tile(
            14,
            x,
            y,
            None,
            Some(NaiveDate::from_ymd_opt(1900, 1, 1).ok_or("date")?),
        )
        .await?;

    assert_eq!(response.markers.len(), 1, "the two share a point");
    assert_eq!(
        response.markers[0].existence,
        Some(ExistenceState::Uncontested),
        "the successor stood in 1900, so the shared pin reads uncontested \
         even though its predecessor was gone by then"
    );
    Ok(())
}

/// A singleton pin reports the whole evidence set: a photograph portraying the
/// hall in 1960 refutes the 1950 removal, so 1955 reads contested.
///
/// The sighting lives on the depicting image, so the pin only sees it by walking
/// the depiction fan-in. A pin folding the entity's own facts alone puts 1955
/// past the removal and reads absent.
#[tokio::test]
async fn get_tile_contests_a_pin_whose_photograph_outlives_its_demolition() -> TestResult {
    let ctx = TestContext::new().await?;
    let (lat, lon) = (48.8566, 2.3522);
    commit_photographed_demolished_entity_at(
        &ctx.app_state.facts,
        "Vanished Hall",
        lat,
        lon,
        1900,
        1950,
        1960,
    )
    .await?;
    let (x, y) = container_tile(lat, lon, 14);

    let response: TileResponse<EntityId> = ctx
        .client
        .fetch_tile(
            14,
            x,
            y,
            None,
            Some(NaiveDate::from_ymd_opt(1955, 6, 1).ok_or("date")?),
        )
        .await?;

    assert_eq!(
        response.markers.first().and_then(|m| m.existence),
        Some(ExistenceState::Contested),
        "the 1960 photograph and the 1950 removal disagree over 1955"
    );
    Ok(())
}

/// A shared pin takes its co-located members' most present verdict, and each
/// member's verdict carries its own photographs: the twin portrayed in 1960
/// refutes its 1950 removal and reads contested, which outranks its neighbour's
/// absent, so the shared pin reads contested in 1955.
///
/// Both members are demolished in 1950, so the two `max` inputs differ only by
/// what their depictions witness — a member fold reading its own facts alone
/// leaves the pin absent.
#[tokio::test]
async fn get_tile_contests_a_colocated_pin_from_the_photographed_member() -> TestResult {
    let ctx = TestContext::new().await?;
    let (lat, lon) = (52.52, 13.405);
    commit_photographed_demolished_entity_at(
        &ctx.app_state.facts,
        "Seen After",
        lat,
        lon,
        1900,
        1950,
        1960,
    )
    .await?;
    commit_photographed_demolished_entity_at(
        &ctx.app_state.facts,
        "Seen Before",
        lat,
        lon,
        1900,
        1950,
        1940,
    )
    .await?;
    let (x, y) = container_tile(lat, lon, 14);

    let response: TileResponse<EntityId> = ctx
        .client
        .fetch_tile(
            14,
            x,
            y,
            None,
            Some(NaiveDate::from_ymd_opt(1955, 6, 1).ok_or("date")?),
        )
        .await?;

    assert_eq!(response.markers.len(), 1, "the two share a point");
    match &response.markers[0].click_action {
        ClickAction::Disambiguate { .. } => {}
        other => return Err(format!("expected Disambiguate, got {other:?}").into()),
    }
    assert_eq!(
        response.markers[0].existence,
        Some(ExistenceState::Contested),
        "the member photographed in 1960 contests 1955, and the shared pin takes it"
    );
    Ok(())
}

/// A cluster pin stands for many entities across a sub-tile, so it reports no
/// verdict — resolving one would mean projecting every member, the work
/// clustering exists to avoid.
#[tokio::test]
async fn get_tile_reports_no_existence_for_a_cluster_pin() -> TestResult {
    let ctx = TestContext::new().await?;
    let (lat, lon_a, lon_b) = (0.0, 12.49, 12.50);
    commit_demolished_entity_at(&ctx.app_state.facts, "Spread West", lat, lon_a, 1700, 1850)
        .await?;
    commit_demolished_entity_at(&ctx.app_state.facts, "Spread East", lat, lon_b, 1860, 1990)
        .await?;

    let (x, y) = container_tile(lat, lon_a, 10);
    let response: TileResponse<EntityId> = ctx
        .client
        .fetch_tile(
            10,
            x,
            y,
            None,
            Some(NaiveDate::from_ymd_opt(1900, 1, 1).ok_or("date")?),
        )
        .await?;

    assert_eq!(
        response.markers.len(),
        1,
        "the spread pair folds to a cluster"
    );
    let marker = &response.markers[0];
    match &marker.click_action {
        ClickAction::Expand { .. } => {}
        other => return Err(format!("expected Expand, got {other:?}").into()),
    }
    assert_eq!(
        marker.existence, None,
        "a cluster stands for many entities and carries no single verdict"
    );
    Ok(())
}

// ==================== list_entities ====================

#[tokio::test]
async fn list_entities_lists_placeable_entities_in_the_viewport() -> TestResult {
    let ctx = TestContext::new().await?;
    let id = commit_named_entity_at(&ctx.app_state.facts, "Duomo", 43.7731, 11.2560).await?;

    let resp = ctx
        .get("/entities?min_lat=43.7&max_lat=43.8&min_lon=11.2&max_lon=11.3")
        .await?;
    assert_eq!(resp.status(), 200);
    let page: chronoscope_api_client::EntityListPage<ServerEntityId, ServerImageId> =
        resp.json().await?;

    assert!(
        page.summaries.iter().any(|s| s.id == id),
        "expected the committed entity in the page, got {:?}",
        page.summaries.iter().map(|s| s.id).collect::<Vec<_>>()
    );
    Ok(())
}

#[tokio::test]
async fn list_entities_accepts_an_antimeridian_viewport() -> TestResult {
    let ctx = TestContext::new().await?;
    // An entity just west of the antimeridian.
    let id = commit_named_entity_at(&ctx.app_state.facts, "Dateline Light", 0.0, 179.5).await?;

    // A box that wraps across the antimeridian: min_lon (170) > max_lon (-170).
    // `request_viewport` admits the wrap, and the geodesic walk surfaces the
    // entity inside it.
    let resp = ctx
        .get("/entities?min_lat=-1&max_lat=1&min_lon=170&max_lon=-170")
        .await?;
    assert_eq!(resp.status(), 200);
    let page: EntityListPage<ServerEntityId, ServerImageId> = resp.json().await?;

    assert!(
        page.summaries.iter().any(|s| s.id == id),
        "expected the antimeridian entity inside the wrapping box, got {:?}",
        page.summaries.iter().map(|s| s.id).collect::<Vec<_>>()
    );
    Ok(())
}

#[tokio::test]
async fn list_entities_rejects_a_page_size_over_the_max() -> TestResult {
    let ctx = TestContext::new().await?;

    let resp = ctx
        .get("/entities?min_lat=0&max_lat=1&min_lon=0&max_lon=1&limit=100000")
        .await?;
    assert_eq!(resp.status(), 400);
    Ok(())
}

#[tokio::test]
async fn list_entities_cursor_walks_every_entity_exactly_once() -> TestResult {
    let ctx = TestContext::new().await?;
    // Three placeable entities inside one small viewport; limit=1 forces the walk
    // across cursor-linked pages, exercising the encode/decode round trip.
    let expected: std::collections::BTreeSet<ServerEntityId> = [
        commit_named_entity_at(&ctx.app_state.facts, "Alpha", 43.771, 11.251).await?,
        commit_named_entity_at(&ctx.app_state.facts, "Beta", 43.772, 11.252).await?,
        commit_named_entity_at(&ctx.app_state.facts, "Gamma", 43.773, 11.253).await?,
    ]
    .into_iter()
    .collect();

    let viewport = "min_lat=43.7&max_lat=43.8&min_lon=11.2&max_lon=11.3";

    // Page 1 caps at the limit and, with entities still to come, hands back a cursor.
    let resp = ctx.get(&format!("/entities?{viewport}&limit=1")).await?;
    assert_eq!(resp.status(), 200);
    let page1: EntityListPage<ServerEntityId, ServerImageId> = resp.json().await?;
    assert_eq!(
        page1.summaries.len(),
        1,
        "limit=1 caps the first page at a single summary"
    );
    let mut seen: std::collections::BTreeSet<ServerEntityId> =
        page1.summaries.iter().map(|s| s.id).collect();
    let mut cursor: Option<Cursor> = Some(
        page1
            .next
            .ok_or("page 1 must carry a next cursor while entities remain")?,
    );

    // Threading each page's cursor back visits the rest with no repeats and
    // terminates with next=None.
    let mut pages = 1;
    while let Some(c) = cursor.take() {
        let resp = ctx
            .get(&format!(
                "/entities?{viewport}&limit=1&cursor={}",
                c.as_str()
            ))
            .await?;
        assert_eq!(resp.status(), 200, "a minted cursor round-trips as a 200");
        let page: EntityListPage<ServerEntityId, ServerImageId> = resp.json().await?;
        for summary in &page.summaries {
            assert!(
                seen.insert(summary.id),
                "entity {:?} surfaced on two different pages",
                summary.id
            );
        }
        cursor = page.next;
        pages += 1;
        assert!(pages <= 8, "cursor pagination failed to terminate");
    }

    assert_eq!(
        seen, expected,
        "the cursor walk covers every committed entity exactly once"
    );
    Ok(())
}

#[tokio::test]
async fn list_entities_resume_reads_the_pinned_snapshot_despite_writes() -> TestResult {
    let ctx = TestContext::new().await?;
    // Three placeable entities in one small viewport make ≥2 pages at limit=1.
    let expected: std::collections::BTreeSet<ServerEntityId> = [
        commit_named_entity_at(&ctx.app_state.facts, "Alpha", 43.771, 11.251).await?,
        commit_named_entity_at(&ctx.app_state.facts, "Beta", 43.772, 11.252).await?,
        commit_named_entity_at(&ctx.app_state.facts, "Gamma", 43.773, 11.253).await?,
    ]
    .into_iter()
    .collect();

    let viewport = "min_lat=43.7&max_lat=43.8&min_lon=11.2&max_lon=11.3";

    // Page 1 pins its snapshot into the cursor it hands back.
    let resp = ctx.get(&format!("/entities?{viewport}&limit=1")).await?;
    assert_eq!(resp.status(), 200);
    let page1: EntityListPage<ServerEntityId, ServerImageId> = resp.json().await?;
    let mut seen: std::collections::BTreeSet<ServerEntityId> =
        page1.summaries.iter().map(|s| s.id).collect();
    let mut cursor: Option<Cursor> = Some(
        page1
            .next
            .ok_or("page 1 must carry a next cursor while entities remain")?,
    );

    // Three more in-box entities land after page 1. Their facts postdate the
    // cursor's snapshot, so the resumed walk must never surface them — a `now()`
    // read would, since all six sit in the same viewport.
    let intruders: std::collections::BTreeSet<ServerEntityId> = [
        commit_named_entity_at(&ctx.app_state.facts, "Delta", 43.774, 11.254).await?,
        commit_named_entity_at(&ctx.app_state.facts, "Epsilon", 43.775, 11.255).await?,
        commit_named_entity_at(&ctx.app_state.facts, "Zeta", 43.776, 11.256).await?,
    ]
    .into_iter()
    .collect();

    let mut pages = 1;
    while let Some(c) = cursor.take() {
        let resp = ctx
            .get(&format!(
                "/entities?{viewport}&limit=1&cursor={}",
                c.as_str()
            ))
            .await?;
        assert_eq!(
            resp.status(),
            200,
            "a snapshot-pinned cursor resumes as a 200, never a stale-snapshot 400"
        );
        let page: EntityListPage<ServerEntityId, ServerImageId> = resp.json().await?;
        for summary in &page.summaries {
            assert!(
                seen.insert(summary.id),
                "entity {:?} surfaced on two different pages",
                summary.id
            );
        }
        cursor = page.next;
        pages += 1;
        assert!(pages <= 8, "cursor pagination failed to terminate");
    }

    assert_eq!(
        seen, expected,
        "the resumed walk covers exactly the entities the pinned snapshot held"
    );
    assert!(
        seen.is_disjoint(&intruders),
        "entities committed after the cursor's snapshot must not surface mid-walk"
    );
    Ok(())
}

// ==================== snapshot pinning ====================

#[tokio::test]
async fn list_entities_reads_only_the_pinned_snapshots_entities() -> TestResult {
    let ctx = TestContext::new().await?;
    let bbox = "min_lat=43.7&max_lat=43.8&min_lon=11.2&max_lon=11.3";

    // One entity in the box, then capture the point the listing was served at.
    let alpha = commit_named_entity_at(&ctx.app_state.facts, "Alpha", 43.771, 11.251).await?;
    let page: EntityListPage<ServerEntityId, ServerImageId> =
        ctx.get(&format!("/entities?{bbox}")).await?.json().await?;
    let s1 = page.snapshot;

    // A second in-box entity lands after the snapshot.
    let beta = commit_named_entity_at(&ctx.app_state.facts, "Beta", 43.772, 11.252).await?;

    // Re-reading pinned at S1 sees only Alpha.
    let pinned: EntityListPage<ServerEntityId, ServerImageId> = ctx
        .get(&format!("/entities?{bbox}&snapshot={}", s1.as_str()))
        .await?
        .json()
        .await?;
    let pinned_ids: BTreeSet<ServerEntityId> = pinned.summaries.iter().map(|s| s.id).collect();
    assert_eq!(
        pinned_ids,
        BTreeSet::from([alpha]),
        "a read pinned at S1 sees only the entity that existed then, not Beta"
    );

    // The live read sees both — so the snapshot, not the bbox, excludes Beta.
    let live: EntityListPage<ServerEntityId, ServerImageId> =
        ctx.get(&format!("/entities?{bbox}")).await?.json().await?;
    let live_ids: BTreeSet<ServerEntityId> = live.summaries.iter().map(|s| s.id).collect();
    assert_eq!(
        live_ids,
        BTreeSet::from([alpha, beta]),
        "the live read sees both, proving S1 is what held Beta back"
    );
    Ok(())
}

#[tokio::test]
async fn entity_images_at_the_detail_snapshot_exclude_later_writes() -> TestResult {
    // The web panel reads entity detail, then its images as a second request.
    // Pinning the images fetch to the detail's snapshot keeps the grid from
    // showing depictions committed between the two requests.
    let (facts, facts_dir) = super::fresh_fact_store().await?;
    let src = "https://upload.wikimedia.org/wikipedia/commons/a/a1/Original.jpg";
    let (id, original) =
        commit_entity_with_depicted_image(&facts, "Pantheon", 41.8986, 12.4769, src).await?;
    // Media for a range past the first image, so a later intruder *would* resolve
    // into a tile if the read weren't pinned — making the pin the sole reason it
    // doesn't.
    let media: HashMap<ServerImageId, ResolvedImageMedia> = (0..4i64)
        .map(|i| {
            let image_id = chronoscope_db::SqlImageId(i);
            (image_id, resolved_media(image_id))
        })
        .collect();
    let ctx = TestContext::with_facts_and_image_media(facts, facts_dir, media).await?;

    // Detail read pins the snapshot the panel threads into the grid.
    let detail = ctx.client.get_entity(&wire_entity_id(id)).await?;
    let snapshot = detail.snapshot;

    // A second depiction lands after the detail read.
    let intruder = commit_extra_depiction_on(
        &ctx.app_state.facts,
        id,
        "https://commons.wikimedia.org/intruder.jpg",
    )
    .await?;

    // Grid pinned to the detail's snapshot: only the original tile.
    let pinned = ctx
        .client
        .get_entity_images(
            &wire_entity_id(id),
            image_page_size()?,
            None,
            Some(&snapshot),
        )
        .await?;
    let pinned_ids: BTreeSet<String> = pinned
        .images
        .iter()
        .map(|t| t.id.as_str().to_string())
        .collect();
    assert_eq!(
        pinned_ids,
        BTreeSet::from([original.0.to_string()]),
        "the grid pinned to the detail's snapshot shows only the depiction that existed then"
    );

    // The live grid shows both, proving the pin is what excludes the intruder.
    let live = ctx
        .client
        .get_entity_images(&wire_entity_id(id), image_page_size()?, None, None)
        .await?;
    let live_ids: BTreeSet<String> = live
        .images
        .iter()
        .map(|t| t.id.as_str().to_string())
        .collect();
    assert_eq!(
        live_ids,
        BTreeSet::from([original.0.to_string(), intruder.0.to_string()]),
        "the live grid shows the intruder too, so the pin is what held it back"
    );
    Ok(())
}

#[tokio::test]
async fn get_entity_images_cursor_and_snapshot_must_agree() -> TestResult {
    let (facts, facts_dir) = super::fresh_fact_store().await?;
    // Two depictions so limit=1 hands back a resume cursor pinned at S1.
    let (id, _image_ids) =
        commit_entity_with_depicted_images(&facts, "Colosseum", 41.8902, 12.4922, 2).await?;
    let media: HashMap<ServerImageId, ResolvedImageMedia> = (0..8i64)
        .map(|i| {
            let image_id = chronoscope_db::SqlImageId(i);
            (image_id, resolved_media(image_id))
        })
        .collect();
    let ctx = TestContext::with_facts_and_image_media(facts, facts_dir, media).await?;
    let one = NonZeroU32::new(1).ok_or("nonzero")?;

    // Page 1 mints a cursor pinned at S1 and echoes S1.
    let page1 = ctx
        .client
        .get_entity_images(&wire_entity_id(id), one, None, None)
        .await?;
    let s1 = page1.snapshot;
    let cursor = page1
        .next
        .ok_or("page 1 carries a cursor while images remain")?;

    // Advance the store, then read fresh to mint a *different* snapshot S2.
    commit_extra_depiction_on(
        &ctx.app_state.facts,
        id,
        "https://commons.wikimedia.org/later.jpg",
    )
    .await?;
    let s2 = ctx
        .client
        .get_entity_images(&wire_entity_id(id), one, None, None)
        .await?
        .snapshot;
    assert_ne!(
        s1, s2,
        "the store advanced, so the fresh read pins a later snapshot"
    );

    // Cursor + its own snapshot: accepted (the two agree).
    ctx.client
        .get_entity_images(&wire_entity_id(id), one, Some(&cursor), Some(&s1))
        .await?;

    // Cursor + a mismatched snapshot: a loud 400.
    match ctx
        .client
        .get_entity_images(&wire_entity_id(id), one, Some(&cursor), Some(&s2))
        .await
    {
        Ok(_) => return Err("a snapshot mismatched with the cursor must 400".into()),
        Err(ApiError::Api { status, .. }) => assert_eq!(status, 400),
        Err(ApiError::Request(e)) => {
            return Err(format!("expected an API 400, got a transport error: {e}").into());
        }
        Err(ApiError::Client(e)) => {
            return Err(format!("expected an API 400, got a client error: {e}").into());
        }
    }
    Ok(())
}
