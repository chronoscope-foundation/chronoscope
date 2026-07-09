//! Tests for the fact-store-backed entity read endpoints.
//!
//! Facts are committed directly against `ctx.app_state.facts` (there's no
//! write endpoint yet) and then read back over HTTP, exercising the real
//! Dropshot path/query extraction and JSON wire shapes — including the
//! `MemoryEntityId` path-param round trip (numeric wire form vs. the
//! `entity-{n}` `Display` form).

use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, TimeZone, Utc};

use chronoscope_api_client::{
    ClickAction, Cursor, EntityListPage, MarkersResponse, client::ApiError,
};
use chronoscope_core::conflicts::{AnyConflictReport, BookendEndpoint, ConflictPath};
use chronoscope_core::date::{DatePrecision, UncertainDate};
use chronoscope_core::geo::{GeoPoint, Meters};
use chronoscope_core::grammar::assertions::{FactualAssertion, JudgmentAssertion};
use chronoscope_core::grammar::attribute::{self, NameText, NameType};
use chronoscope_core::grammar::bookend::ConstructionFact;
use chronoscope_core::grammar::citations::{
    Excerpt, ExternalSource, FactualCitation, JudgmentSource, Language,
};
use chronoscope_core::grammar::depiction::{self, Perspective};
use chronoscope_core::grammar::ids::{FactId, UserId};
use chronoscope_core::grammar::image::{self, ImageMedium};
use chronoscope_core::location::{Location, UnresolvedLocation};
use chronoscope_core::store::memory::{MemoryEntityId, MemoryFactStore, MemoryIds, MemoryImageId};
use chronoscope_core::submit::{
    Commit, CommitAuthor, Decl, EntityIdx, ImageIdx, SubmitFact, commit_facts,
};

use super::TestContext;
use crate::cdn::tests::TEST_CDN_BASE_URL;
use crate::state::ResolvedImageMedia;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

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
        Meters(10.0),
    )?))
}

/// Commit a fresh single-entity, single-fact-set bundle: one name and one
/// construction location. Enough to make the entity placeable (for
/// `/markers` and `/entities`) and nameable (for `get_entity`).
async fn commit_named_entity_at(
    facts: &MemoryFactStore,
    name: &str,
    lat: f64,
    lon: f64,
) -> Result<MemoryEntityId, Box<dyn std::error::Error + Send + Sync>> {
    let commit = Commit::<MemoryIds> {
        author: CommitAuthor::User(UserId::new("test")),
        recorded_at: fixed_time()?,
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [
            SubmitFact::Factual {
                assertion: FactualAssertion::Attribute {
                    fact: attribute::Fact::Name {
                        entity: EntityIdx(0),
                        name: NameText::new(name),
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

/// Commit a placeable entity carrying one name per `(language, text)` pair, so a
/// read can exercise `Accept-Language` negotiation between them.
async fn commit_entity_with_names_at(
    facts: &MemoryFactStore,
    names: &[(&str, &str)],
    lat: f64,
    lon: f64,
) -> Result<MemoryEntityId, Box<dyn std::error::Error + Send + Sync>> {
    let named = |language: &str,
                 name: &str|
     -> Result<SubmitFact, Box<dyn std::error::Error + Send + Sync>> {
        Ok(SubmitFact::Factual {
            assertion: FactualAssertion::Attribute {
                fact: attribute::Fact::Name {
                    entity: EntityIdx(0),
                    name: NameText::new(name),
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
    let commit = Commit::<MemoryIds> {
        author: CommitAuthor::User(UserId::new("test")),
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

/// Commit a named, placeable entity depicted by one image: an exterior-picture
/// depiction whose image carries a Commons `Source` URL and a `Picture` medium.
/// Exercises the image read path (`get_entity` grid, `/markers` thumbnail).
async fn commit_entity_with_depicted_image(
    facts: &MemoryFactStore,
    name: &str,
    lat: f64,
    lon: f64,
    image_source_url: &str,
) -> Result<(MemoryEntityId, MemoryImageId), Box<dyn std::error::Error + Send + Sync>> {
    let commit = Commit::<MemoryIds> {
        author: CommitAuthor::User(UserId::new("test")),
        recorded_at: fixed_time()?,
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: vec![Decl::Local],
        facts: [
            SubmitFact::Factual {
                assertion: FactualAssertion::Attribute {
                    fact: attribute::Fact::Name {
                        entity: EntityIdx(0),
                        name: NameText::new(name),
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

    let detail = ctx.client.get_entity(&id).await?;

    assert_eq!(detail.entity.id, id);
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
    assert!(
        detail.images.is_empty(),
        "an entity with no depiction carries no detail images"
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

    let detail = ctx.client.get_entity(&id).await?;

    assert_eq!(
        detail.display_name.as_deref(),
        Some("Florence"),
        "a header-less request falls back to the English name, not the first-listed Italian one"
    );
    Ok(())
}

/// Media keys a fact-store image resolves to, mirroring the `dev` resolver's
/// placeholder layout: single path segments under `media/` so `/media/{key}`
/// serves them, distinct per image.
fn resolved_media(image_id: MemoryImageId) -> ResolvedImageMedia {
    ResolvedImageMedia {
        storage_key: format!("media/factimg-{}.jpg", image_id.0),
        thumbnail_key: format!("media/factimg-{}-thumb.jpg", image_id.0),
    }
}

#[tokio::test]
async fn get_entity_resolves_a_depicted_image_into_the_detail_grid() -> TestResult {
    let facts = MemoryFactStore::new();
    let src = "https://upload.wikimedia.org/wikipedia/commons/a/a1/Pantheon.jpg";
    let (id, image_id) =
        commit_entity_with_depicted_image(&facts, "Pantheon", 41.8986, 12.4769, src).await?;

    // The depicted image is resolved into media; the read path serves its
    // original from our own /media/{key}, not from upstream Commons.
    let media = resolved_media(image_id);
    let expected_display = format!("{TEST_CDN_BASE_URL}/{}", media.storage_key);
    let ctx =
        TestContext::with_facts_and_image_media(facts, HashMap::from([(image_id, media)])).await?;

    let detail = ctx.client.get_entity(&id).await?;

    assert_eq!(detail.entity.id, id);
    let image = detail.images.first().ok_or("expected one detail image")?;
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
    Ok(())
}

#[tokio::test]
async fn get_entity_skips_a_depiction_whose_image_is_unresolved() -> TestResult {
    // The image has a Source fact but no entry in the media map — a tile that
    // can't load is worse than an absent one, so the grid drops it.
    let facts = MemoryFactStore::new();
    let src = "https://upload.wikimedia.org/wikipedia/commons/c/c3/Unresolved.jpg";
    let (id, _image_id) =
        commit_entity_with_depicted_image(&facts, "Unresolved", 41.9, 12.5, src).await?;

    let ctx = TestContext::with_facts_and_image_media(facts, HashMap::new()).await?;

    let detail = ctx.client.get_entity(&id).await?;
    assert!(
        detail.images.is_empty(),
        "an unresolved depiction contributes no grid tile, got {:?}",
        detail.images
    );
    Ok(())
}

#[tokio::test]
async fn get_entity_404s_for_an_id_no_fact_ever_named() -> TestResult {
    let ctx = TestContext::new().await?;
    // A fresh store mints entity ids from 0; this id was never declared by
    // any commit, so no fact anywhere mentions it.
    let unknown = MemoryEntityId(999_999);

    match ctx.client.get_entity(&unknown).await {
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
    }
    Ok(())
}

#[tokio::test]
async fn get_entity_404_carries_cors_headers() -> TestResult {
    // The web detail panel calls this cross-origin; a 404 without CORS headers
    // is blocked by the browser, so the panel sees an opaque transport error
    // instead of a clean "not found".
    let ctx = TestContext::new().await?;
    let resp = ctx.get("/entities/999999").await?;
    assert_eq!(resp.status(), 404, "an unnamed id must 404");
    assert_eq!(
        resp.headers()
            .get("access-control-allow-origin")
            .and_then(|v| v.to_str().ok()),
        Some("*"),
        "the 404 must carry CORS so a cross-origin client can read it"
    );
    Ok(())
}

#[tokio::test]
async fn get_entity_path_param_round_trips_the_numeric_wire_form() -> TestResult {
    // `MemoryEntityId`'s `Display` renders the debug form `entity-{n}`, not
    // the wire form the path deserializer parses. Hitting the raw HTTP path
    // with the bare integer (what `Client::get_entity` sends) pins that the
    // server-side path extraction actually accepts it.
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

/// Commit one entity with two competing `Construction::Started` dates in
/// disjoint years, over-determining its construction-started slot. Returns the
/// entity id and the two minted fact ids — the fighting set the detector must
/// attribute.
async fn commit_competing_construction_dates(
    facts: &MemoryFactStore,
) -> Result<(MemoryEntityId, BTreeSet<FactId>), Box<dyn std::error::Error + Send + Sync>> {
    let started =
        |year: i32, url: &str| -> Result<SubmitFact, Box<dyn std::error::Error + Send + Sync>> {
            let bound = UncertainDate::with_precision(
                chrono::NaiveDate::from_ymd_opt(year, 1, 1).ok_or("valid date")?,
                DatePrecision::Year,
            )?;
            Ok(SubmitFact::Factual {
                assertion: FactualAssertion::Construction {
                    fact: ConstructionFact::Started {
                        entity: EntityIdx(0),
                        bound,
                    },
                },
                citation: citation(url)?,
            })
        };
    let commit = Commit::<MemoryIds> {
        author: CommitAuthor::User(UserId::new("test")),
        recorded_at: fixed_time()?,
        entities: vec![Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts: [
            started(1887, "https://a.example/src")?,
            started(1889, "https://b.example/src")?,
        ]
        .into_iter()
        .collect(),
    };

    let result = commit_facts(facts, commit)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let id = result
        .entities
        .get(&EntityIdx(0))
        .ok_or("entity 0 resolved")?
        .id;
    let fact_ids = result.fact_ids.iter().copied().collect();
    Ok((id, fact_ids))
}

#[tokio::test]
async fn get_entity_surfaces_a_competing_construction_date_conflict() -> TestResult {
    let ctx = TestContext::new().await?;
    let (id, expected) = commit_competing_construction_dates(&ctx.app_state.facts).await?;

    let detail = ctx.client.get_entity(&id).await?;

    assert_eq!(
        detail.conflicts.len(),
        1,
        "one over-determined slot yields one conflict report, got {:?}",
        detail.conflicts
    );
    let AnyConflictReport::Date(report) = detail.conflicts.first().ok_or("no conflict report")?;
    assert_eq!(
        report.location.entity, id,
        "the conflict is anchored to the entity that was read"
    );
    assert_eq!(
        report.location.path,
        ConflictPath::Construction {
            endpoint: BookendEndpoint::Started,
        },
        "the conflict anchors at the construction-started slot"
    );
    let contributing: BTreeSet<FactId> = report.data.contributing.iter().copied().collect();
    assert_eq!(
        contributing, expected,
        "both competing start claims are the fighting set"
    );
    Ok(())
}

// ==================== list_markers ====================

#[tokio::test]
async fn list_markers_selects_a_lone_entity() -> TestResult {
    let ctx = TestContext::new().await?;
    let id = commit_named_entity_at(&ctx.app_state.facts, "Colosseum", 41.8902, 12.4922).await?;

    let bbox = chronoscope_core::geo::Bbox::from_coords(41.8, 42.0, 12.4, 12.6)?;
    let response = ctx.client.list_markers(&bbox).await?;

    assert_eq!(response.markers.len(), 1, "expected exactly one marker");
    let marker = &response.markers[0];
    assert_eq!(marker.id, id);
    assert_eq!(
        marker.name.as_deref(),
        Some("Colosseum"),
        "with no Accept-Language the negotiated marker name falls back to the only name"
    );
    assert!(
        marker.thumbnail_url.is_none(),
        "an entity with no depiction has no marker thumbnail"
    );
    match &marker.click_action {
        ClickAction::Select { entity_id } => assert_eq!(*entity_id, id),
        other => return Err(format!("expected Select, got {other:?}").into()),
    }
    assert!(!response.truncated);
    Ok(())
}

#[tokio::test]
async fn list_markers_carries_a_thumbnail_for_a_depicted_entity() -> TestResult {
    let facts = MemoryFactStore::new();
    let src = "https://upload.wikimedia.org/wikipedia/commons/b/b2/Colosseum.jpg";
    let (id, image_id) =
        commit_entity_with_depicted_image(&facts, "Colosseum", 41.8902, 12.4922, src).await?;

    let media = resolved_media(image_id);
    let expected_thumb = format!("{TEST_CDN_BASE_URL}/{}", media.thumbnail_key);
    let ctx =
        TestContext::with_facts_and_image_media(facts, HashMap::from([(image_id, media)])).await?;

    let bbox = chronoscope_core::geo::Bbox::from_coords(41.8, 42.0, 12.4, 12.6)?;
    let response = ctx.client.list_markers(&bbox).await?;

    let marker = response
        .markers
        .iter()
        .find(|m| m.id == id)
        .ok_or("expected a marker for the depicted entity")?;
    assert_eq!(
        marker.thumbnail_url.as_ref().map(url::Url::as_str),
        Some(expected_thumb.as_str()),
        "the marker thumbnail serves the resolved image's thumbnail from our media host"
    );
    Ok(())
}

#[tokio::test]
async fn list_markers_disambiguates_colocated_entities() -> TestResult {
    let ctx = TestContext::new().await?;
    // Two entities declared at the exact same point.
    let a = commit_named_entity_at(&ctx.app_state.facts, "Old Chapel", 45.2, 12.27).await?;
    let b = commit_named_entity_at(&ctx.app_state.facts, "New Chapel", 45.2, 12.27).await?;

    let bbox = chronoscope_core::geo::Bbox::from_coords(45.0, 45.4, 12.0, 12.5)?;
    let response = ctx.client.list_markers(&bbox).await?;

    assert_eq!(
        response.markers.len(),
        1,
        "co-located entities collapse into one marker"
    );
    match &response.markers[0].click_action {
        ClickAction::Disambiguate { entries } => {
            let ids: std::collections::BTreeSet<_> = entries.iter().map(|e| e.id).collect();
            assert_eq!(ids, [a, b].into_iter().collect());
        }
        other => return Err(format!("expected Disambiguate, got {other:?}").into()),
    }
    Ok(())
}

#[tokio::test]
async fn list_markers_omits_entities_outside_the_bbox() -> TestResult {
    let ctx = TestContext::new().await?;
    commit_named_entity_at(&ctx.app_state.facts, "Eiffel Tower", 48.8584, 2.2945).await?;

    // A box nowhere near Paris.
    let bbox = chronoscope_core::geo::Bbox::from_coords(41.8, 42.0, 12.4, 12.6)?;
    let response = ctx.client.list_markers(&bbox).await?;

    assert!(response.markers.is_empty());
    Ok(())
}

#[tokio::test]
async fn list_markers_accepts_an_antimeridian_bbox() -> TestResult {
    let ctx = TestContext::new().await?;
    // An entity just west of the antimeridian.
    let id = commit_named_entity_at(&ctx.app_state.facts, "Dateline Light", 0.0, 179.5).await?;

    // A box that wraps across the antimeridian: min_lon (170) > max_lon (-170).
    // The whole path — `Bbox::from_coords`, the server's `request_bbox`, and the
    // core spatial walk — must accept the wrap rather than 400, and surface the
    // entity inside it.
    let bbox = chronoscope_core::geo::Bbox::from_coords(-1.0, 1.0, 170.0, -170.0)?;
    let response = ctx.client.list_markers(&bbox).await?;

    let marker = response
        .markers
        .iter()
        .find(|m| m.id == id)
        .ok_or("expected the antimeridian entity inside the wrapping box")?;
    assert_eq!(
        marker.name.as_deref(),
        Some("Dateline Light"),
        "the wrapping-box marker still carries the entity's name"
    );
    Ok(())
}

#[tokio::test]
async fn list_markers_negotiates_marker_name_by_accept_language() -> TestResult {
    let ctx = TestContext::new().await?;
    commit_entity_with_names_at(
        &ctx.app_state.facts,
        &[("en", "Florence"), ("it", "Firenze")],
        43.7731,
        11.2560,
    )
    .await?;

    let query = "/markers?min_lat=43.7&max_lat=43.8&min_lon=11.2&max_lon=11.3";

    // `it` preference: the Italian name wins.
    let italian_resp = ctx
        .client
        .reqwest_client()
        .get(ctx.url(query))
        .header(reqwest::header::ACCEPT_LANGUAGE, "it")
        .send()
        .await?;
    assert_eq!(
        italian_resp
            .headers()
            .get(reqwest::header::VARY)
            .and_then(|v| v.to_str().ok()),
        Some("Accept-Language"),
        "a language-negotiated response advertises Vary: Accept-Language so caches don't cross-serve locales"
    );
    let italian: MarkersResponse = italian_resp.json().await?;
    assert_eq!(
        italian.markers.first().and_then(|m| m.name.as_deref()),
        Some("Firenze"),
        "an `it` preference selects the Italian name"
    );

    // `en-US,en;q=0.9`: both entries reduce to the primary subtag `en`, so the
    // English name wins regardless of the q-weight.
    let english: MarkersResponse = ctx
        .client
        .reqwest_client()
        .get(ctx.url(query))
        .header(reqwest::header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(
        english.markers.first().and_then(|m| m.name.as_deref()),
        Some("Florence"),
        "an `en` preference selects the English name"
    );
    Ok(())
}

#[tokio::test]
async fn list_markers_matches_accept_language_case_insensitively() -> TestResult {
    let ctx = TestContext::new().await?;
    commit_entity_with_names_at(
        &ctx.app_state.facts,
        &[("en", "Florence"), ("it", "Firenze")],
        43.7731,
        11.2560,
    )
    .await?;

    let query = "/markers?min_lat=43.7&max_lat=43.8&min_lon=11.2&max_lon=11.3";

    // An uppercase `IT` must match the lowercase-canonical stored `it` tag.
    let response: MarkersResponse = ctx
        .client
        .reqwest_client()
        .get(ctx.url(query))
        .header(reqwest::header::ACCEPT_LANGUAGE, "IT")
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(
        response.markers.first().and_then(|m| m.name.as_deref()),
        Some("Firenze"),
        "an uppercase `IT` header matches the lowercase-canonical `it` tag"
    );
    Ok(())
}

#[tokio::test]
async fn list_markers_orders_accept_language_by_q_weight() -> TestResult {
    let ctx = TestContext::new().await?;
    commit_entity_with_names_at(
        &ctx.app_state.facts,
        &[("en", "Munich"), ("de", "München")],
        48.1372,
        11.5756,
    )
    .await?;

    let query = "/markers?min_lat=48.1&max_lat=48.2&min_lon=11.5&max_lon=11.6";

    // `de;q=0.5, en`: `en` carries an implicit q=1.0, outranking `de;q=0.5`
    // despite coming later in the header, so the English name wins.
    let response: MarkersResponse = ctx
        .client
        .reqwest_client()
        .get(ctx.url(query))
        .header(reqwest::header::ACCEPT_LANGUAGE, "de;q=0.5, en")
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(
        response.markers.first().and_then(|m| m.name.as_deref()),
        Some("Munich"),
        "the higher q-weight (`en`, implicit 1.0) wins over `de;q=0.5`"
    );
    Ok(())
}

// ==================== list_entities ====================

#[tokio::test]
async fn list_entities_lists_placeable_entities_in_the_bbox() -> TestResult {
    let ctx = TestContext::new().await?;
    let id = commit_named_entity_at(&ctx.app_state.facts, "Duomo", 43.7731, 11.2560).await?;

    let resp = ctx
        .get("/entities?min_lat=43.7&max_lat=43.8&min_lon=11.2&max_lon=11.3")
        .await?;
    assert_eq!(resp.status(), 200);
    let page: chronoscope_api_client::EntityListPage = resp.json().await?;

    assert!(
        page.summaries.iter().any(|s| s.id == id),
        "expected the committed entity in the page, got {:?}",
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
    // Three placeable entities inside one small bbox; limit=1 forces the walk
    // across cursor-linked pages, exercising the encode/decode round trip.
    let expected: std::collections::BTreeSet<MemoryEntityId> = [
        commit_named_entity_at(&ctx.app_state.facts, "Alpha", 43.771, 11.251).await?,
        commit_named_entity_at(&ctx.app_state.facts, "Beta", 43.772, 11.252).await?,
        commit_named_entity_at(&ctx.app_state.facts, "Gamma", 43.773, 11.253).await?,
    ]
    .into_iter()
    .collect();

    let bbox = "min_lat=43.7&max_lat=43.8&min_lon=11.2&max_lon=11.3";

    // Page 1 caps at the limit and, with entities still to come, hands back a cursor.
    let resp = ctx.get(&format!("/entities?{bbox}&limit=1")).await?;
    assert_eq!(resp.status(), 200);
    let page1: EntityListPage = resp.json().await?;
    assert_eq!(
        page1.summaries.len(),
        1,
        "limit=1 caps the first page at a single summary"
    );
    let mut seen: std::collections::BTreeSet<MemoryEntityId> =
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
            .get(&format!("/entities?{bbox}&limit=1&cursor={}", c.as_str()))
            .await?;
        assert_eq!(resp.status(), 200, "a minted cursor round-trips as a 200");
        let page: EntityListPage = resp.json().await?;
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
