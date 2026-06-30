//! Wire-stability golden tests.
//!
//! Each test pins the JCS-encoded form of a representative value from a
//! grammar cluster. Changing the serialize shape of any type reachable from
//! `submit::Commit` changes the `CommitId` of every existing commit; these
//! tests catch that by asserting against exact byte strings.
//!
//! The goldens also guard a class JCS can't catch, via a streaming
//! `serde_json::to_string` round-trip. JCS tolerates broken tagged-enum
//! shapes (a newtype variant whose payload isn't a map; a tagged enum nested
//! inside another sharing the same tag key) — it walks the value into a tree
//! and re-emits, so the bug never surfaces. The streaming serializer errors
//! at emit time on those shapes. [`assert_golden_roundtrip`] runs both,
//! byte-pinning the content address and proving the value survives a
//! streaming serialize→deserialize.
//!
//! To evolve a wire shape: update the type, run the affected golden, copy the
//! new bytes from the failure message, and update the expected string in the
//! same commit. See the project `CLAUDE.md` "Wire stability conventions"
//! section for the broader rules.

use std::collections::BTreeSet;
use std::fmt::Debug;

use chrono::TimeZone;
use serde::Serialize;
use serde::de::DeserializeOwned;
use url::Url;

use crate::date::{DatePrecision, UncertainDate};
use crate::facts::assertions::{FactualAssertion, JudgmentAssertion, MetaAssertion};
use crate::facts::attribute::{self, EntityRelationType, NameText, NameType};
use crate::facts::bookend;
use crate::facts::citations::{
    Excerpt, ExternalReference, ExternalSource, FactualCitation, JudgmentSource, Language,
    MetaSource, Observer,
};
use crate::facts::composites::{self, SubimageRegion};
use crate::facts::depiction::{self, Perspective};
use crate::facts::event;
use crate::facts::features::Feature;
use crate::facts::geometry::{ImageGeometry, ProportionalPolyline};
use crate::facts::identity;
use crate::facts::ids::{AnalyzerProcess, AnalyzerVersion, FactId, UserId};
use crate::facts::image::{self, ImageMedium};
use crate::facts::lifecycle::{DurationalKind, DurationalRole, LifetimeEventKind, MoveMethod};
use crate::facts::memory::{MemoryEntityId, MemoryEventId, MemoryIds, MemoryImageId};
use crate::facts::observation;
use crate::facts::spatial::TopologicalRel;
use crate::facts::submit::{Commit, CommitAuthor, Decl, EntityIdx, ImageIdx, SubmitFact};
use crate::geo::{GeoPoint, Meters};
use crate::location::Location;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn ent(n: u64) -> std::result::Result<MemoryEntityId, std::convert::Infallible> {
    Ok(MemoryEntityId(n))
}
fn evt(n: u64) -> std::result::Result<MemoryEventId, std::convert::Infallible> {
    Ok(MemoryEventId(n))
}
fn img(n: u64) -> std::result::Result<MemoryImageId, std::convert::Infallible> {
    Ok(MemoryImageId(n))
}

fn en() -> std::result::Result<Language, Box<dyn std::error::Error>> {
    Ok(Language::new("en")?)
}

fn sample_date() -> std::result::Result<UncertainDate, Box<dyn std::error::Error>> {
    let d = chrono::NaiveDate::from_ymd_opt(1700, 1, 1).ok_or("date")?;
    Ok(UncertainDate::with_precision(d, DatePrecision::Year)?)
}

fn sample_factual_citation() -> Result<FactualCitation> {
    let url = Url::parse("https://example.com/source")?;
    let source = ExternalSource::Url {
        url,
        published: None,
    };
    let excerpts = vec![Excerpt::new("source-text")?];
    Ok(FactualCitation::new(source, excerpts)?)
}

fn sample_judgment_source() -> Result<JudgmentSource<ImageIdx>> {
    Ok(JudgmentSource::ImageObservation {
        image: ImageIdx(0),
        region: None,
        observer: Observer::User {
            user: UserId::new("alice"),
            justification: None,
        },
    })
}

/// A machine-derivation judgment source with fixed literals, so the pinned
/// bytes are independent of the build environment.
fn sample_derivation_source() -> Result<JudgmentSource<ImageIdx>> {
    Ok(JudgmentSource::Derivation {
        process: AnalyzerProcess::new("matcher"),
        version: AnalyzerVersion::new("test-version"),
        basis: [FactId::new(3), FactId::new(5)].into_iter().collect(),
        snapshot: FactId::new(7),
    })
}

/// Assert `actual == expected`, with both strings in the panic message so a
/// regenerated golden is easy to copy in.
///
/// For one-way projections: a commit's canonical JCS is the
/// [`CommitHashView`](crate::facts::submit) hash input, which flattens the
/// author to a lossy canonical string and quantizes `recorded_at`, so it can't
/// round-trip back to the value. Pinning the JCS still fully determines the
/// resulting [`CommitId`](crate::facts::ids::CommitId). Types that round-trip
/// use [`assert_golden_roundtrip`].
#[track_caller]
fn assert_golden(actual: &str, expected: &str) {
    assert_eq!(
        actual, expected,
        "wire-shape regression: replace the expected string with the actual one if the change is intentional"
    );
}

/// Pin a value's wire shape two ways.
///
/// 1. `serde_jcs::to_string(value)` must equal `expected_jcs` — the same
///    bytes that flow into the [`CommitId`](crate::facts::ids::CommitId) hash.
/// 2. `serde_json::to_string(value)` then `serde_json::from_str::<T>(..)` must
///    reproduce `value`.
///
/// `serde_json` rejects a tagged enum whose payload isn't a map (an integer, a
/// fieldless inner enum, a string), or a tagged enum nested inside another
/// sharing the tag key; JCS tolerates those by re-emitting from a tree. The
/// streaming round-trip catches the broken-tag class, on the path real wire
/// input takes.
#[track_caller]
fn assert_golden_roundtrip<T>(value: &T, expected_jcs: &str) -> Result<()>
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let canonical = serde_jcs::to_string(value)?;
    assert_eq!(
        canonical, expected_jcs,
        "wire-shape regression: replace the expected string with the actual one if the change is intentional"
    );

    // serde_json errors on the broken-tag class JCS tolerates; the parse-back
    // proves the shape round-trips.
    let streamed = serde_json::to_string(value)?;
    let parsed: T = serde_json::from_str(&streamed)?;
    assert_eq!(
        &parsed, value,
        "value did not survive a streaming serialize -> deserialize round-trip"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Per-cluster Fact goldens
// ---------------------------------------------------------------------------

#[test]
fn golden_attribute_fact_name() -> Result<()> {
    let f: attribute::Fact<MemoryEntityId> = attribute::Fact::Name {
        entity: ent(1)?,
        name: NameText::new("Pantheon"),
        language: en()?,
        name_type: NameType::Common,
        valid_from: None,
        valid_to: None,
    };
    assert_golden_roundtrip(
        &f,
        r#"{"entity":1,"language":"en","name":"Pantheon","name_type":"common","type":"name","valid_from":null,"valid_to":null}"#,
    )
}

#[test]
fn golden_attribute_fact_relationship() -> Result<()> {
    let f = attribute::Fact::relationship(ent(1)?, ent(2)?, EntityRelationType::Contains)?;
    assert_golden_roundtrip(
        &f,
        r#"{"pair":{"from":1,"to":2},"relation":"contains","type":"relationship"}"#,
    )
}

#[test]
fn golden_bookend_fact_started() -> Result<()> {
    let f: bookend::Fact<MemoryEntityId> = bookend::Fact::Started {
        entity: ent(1)?,
        bound: sample_date()?,
    };
    assert_golden_roundtrip(
        &f,
        r#"{"bound":{"earliest":{"date":"1700-01-01","precision":"year"},"latest":{"date":"1700-01-01","precision":"year"}},"entity":1,"type":"started"}"#,
    )
}

#[test]
fn golden_event_fact_durational_date() -> Result<()> {
    let f: event::Fact<MemoryEntityId, MemoryEventId> = event::Fact::DurationalDate {
        event: evt(1)?,
        role: DurationalRole::Started,
        bound: sample_date()?,
    };
    assert_golden_roundtrip(
        &f,
        r#"{"bound":{"earliest":{"date":"1700-01-01","precision":"year"},"latest":{"date":"1700-01-01","precision":"year"}},"event":1,"role":"started","type":"durational_date"}"#,
    )
}

#[test]
fn golden_event_fact_has_event() -> Result<()> {
    let f: event::Fact<MemoryEntityId, MemoryEventId> = event::Fact::HasEvent {
        entity: ent(1)?,
        event: evt(1)?,
        kind: LifetimeEventKind::Durational {
            kind: DurationalKind::Damaged,
        },
    };
    assert_golden_roundtrip(
        &f,
        r#"{"entity":1,"event":1,"kind":{"kind":"damaged","type":"durational"},"type":"has_event"}"#,
    )
}

#[test]
fn golden_event_fact_move_method() -> Result<()> {
    let f: event::Fact<MemoryEntityId, MemoryEventId> = event::Fact::MoveMethod {
        event: evt(1)?,
        method: MoveMethod::Whole,
    };
    assert_golden_roundtrip(&f, r#"{"event":1,"method":"whole","type":"move_method"}"#)
}

#[test]
fn golden_image_fact_source() -> Result<()> {
    let f: image::Fact<MemoryImageId> = image::Fact::Source {
        image: img(1)?,
        url: Url::parse("https://example.com/img")?,
    };
    assert_golden_roundtrip(
        &f,
        r#"{"image":1,"type":"source","url":"https://example.com/img"}"#,
    )
}

#[test]
fn golden_image_fact_medium() -> Result<()> {
    let f: image::Fact<MemoryImageId> = image::Fact::Medium {
        image: img(1)?,
        medium: ImageMedium::PictorialMap,
    };
    assert_golden_roundtrip(
        &f,
        r#"{"image":1,"medium":"pictorial_map","type":"medium"}"#,
    )
}

#[test]
fn golden_identity_fact_same_entity() -> Result<()> {
    let f = identity::Fact::<MemoryEntityId, MemoryEventId, MemoryImageId>::same_entity(
        ent(1)?,
        ent(2)?,
    )?;
    assert_golden_roundtrip(&f, r#"{"pair":{"a":1,"b":2},"type":"same_entity"}"#)
}

#[test]
fn golden_depiction_fact_bare() -> Result<()> {
    // The common P18 case: an entity↔image link with no localization and no
    // perspective. The struct carries no inner tag — the
    // `JudgmentAssertion::Depiction` wrapper tags it.
    let f: depiction::Fact<MemoryEntityId, MemoryImageId> = depiction::Fact {
        entity: ent(1)?,
        image: img(1)?,
        localization: None,
        perspective: None,
    };
    assert_golden_roundtrip(
        &f,
        r#"{"entity":1,"image":1,"localization":null,"perspective":null}"#,
    )
}

#[test]
fn golden_depiction_fact_localized() -> Result<()> {
    // A localized, classified depiction: the `ImageGeometry` bbox rides under
    // `localization`, the leaf `Perspective` value under `perspective`.
    let f: depiction::Fact<MemoryEntityId, MemoryImageId> = depiction::Fact {
        entity: ent(1)?,
        image: img(1)?,
        localization: Some(ImageGeometry::bbox(0.1, 0.2, 0.3, 0.4)?),
        perspective: Some(Perspective::Interior),
    };
    assert_golden_roundtrip(
        &f,
        r#"{"entity":1,"image":1,"localization":{"rect":{"max":{"x":0.30000001192092896,"y":0.4000000059604645},"min":{"x":0.10000000149011612,"y":0.20000000298023224}},"type":"bbox"},"perspective":"interior"}"#,
    )
}

#[test]
fn golden_observation_fact_feature() -> Result<()> {
    // `Feature` carries each claim in a named field (`{"type":..,"shape":..}`).
    // Naming the field lets internal tagging wrap a fieldless inner enum like
    // `RoofShape`; a bare newtype payload would flatten beside the tag and
    // collide.
    let f: observation::Fact<MemoryEntityId> = observation::Fact::Feature {
        entity: ent(1)?,
        feature: crate::facts::features::Feature::RoofShape {
            shape: crate::facts::features::RoofShape::Gabled,
        },
    };
    assert_golden_roundtrip(
        &f,
        r#"{"entity":1,"feature":{"shape":"gabled","type":"roof_shape"},"type":"feature"}"#,
    )
}

#[test]
fn golden_observation_fact_spatial() -> Result<()> {
    let f = observation::Fact::spatial(ent(1)?, ent(2)?, TopologicalRel::Adjacent)?;
    assert_golden_roundtrip(
        &f,
        r#"{"pair":{"from":1,"to":2},"relation":{"type":"adjacent"},"type":"spatial"}"#,
    )
}

#[test]
fn golden_composites_fact_is_subimage_of() -> Result<()> {
    let region = SubimageRegion::rect(0.0, 0.0, 0.5, 0.5)?;
    let f: composites::Fact<MemoryImageId> = composites::Fact::IsSubimageOf {
        subimage: img(1)?,
        parent: img(2)?,
        region,
    };
    assert_golden_roundtrip(
        &f,
        r#"{"parent":2,"region":{"rect":{"max":{"x":0.5,"y":0.5},"min":{"x":0,"y":0}},"type":"rect"},"subimage":1,"type":"is_subimage_of"}"#,
    )
}

// ---------------------------------------------------------------------------
// Streaming-round-trip variant locks
// ---------------------------------------------------------------------------
//
// Each pins a variant whose shape is a tagged enum over a non-map payload,
// or a tagged enum nested inside another tagged enum — shapes JCS can't vet.
// The streaming serialize in `assert_golden_roundtrip` errors on a broken
// tag where JCS canonicalizes it silently.

#[test]
fn golden_feature_story_count_locks_integer_payload() -> Result<()> {
    // Integer payload: the `u32` rides in a named `stories` field. Locks
    // `{"stories":..,"type":..}`.
    let feature = Feature::StoryCount { stories: 3 };
    assert_golden_roundtrip(&feature, r#"{"stories":3,"type":"story_count"}"#)
}

#[test]
fn golden_decl_existing_locks_tagged_id_payload() -> Result<()> {
    // `Decl::Existing` carries an id, part of the commit address. Locks that
    // the `{"type":"existing","id":..}` tagging round-trips.
    let decl: Decl<MemoryEntityId> = Decl::Existing { id: ent(5)? };
    assert_golden_roundtrip(&decl, r#"{"id":5,"type":"existing"}"#)
}

#[test]
fn golden_external_reference_unmodeled_url_locks_variant() -> Result<()> {
    let reference = ExternalReference::UnmodeledUrl {
        url: Url::parse("https://example.com/some/path")?,
    };
    assert_golden_roundtrip(
        &reference,
        r#"{"type":"unmodeled_url","url":"https://example.com/some/path"}"#,
    )
}

#[test]
fn golden_location_empty_locks_bottom_tag() -> Result<()> {
    // The empty location (⊥) is a unit variant tagged on `type`, byte-pinned so
    // the bottom of the lattice has one stable wire form.
    assert_golden_roundtrip(&Location::Empty, r#"{"type":"empty"}"#)
}

#[test]
fn golden_location_one_of_locks_nested_internal_tag() -> Result<()> {
    // `OneOf` nests `Location` values themselves tagged on `type`. Locks
    // that the nesting keeps the tag keys in separate maps.
    let location = Location::one_of(vec![
        Location::circle(GeoPoint::new(48.8, 2.3)?, Meters(10.0))?,
        Location::circle(GeoPoint::new(51.5, -0.1)?, Meters(10.0))?,
    ])?;
    assert_golden_roundtrip(
        &location,
        r#"{"members":[{"center":{"lat":48.8,"lon":2.3},"radius":10,"type":"circle"},{"center":{"lat":51.5,"lon":-0.1},"radius":10,"type":"circle"}],"type":"one_of"}"#,
    )
}

#[test]
fn golden_image_geometry_bbox_locks_tagged_rect() -> Result<()> {
    // `ImageGeometry::BBox` carries its `ProportionalRect` corner-pair payload
    // under the named `rect` field. The round-trip locks the bbox wire form and
    // proves the manually-deserialized rect survives a streaming serialize.
    let geometry = ImageGeometry::bbox(0.1, 0.2, 0.3, 0.4)?;
    assert_golden_roundtrip(
        &geometry,
        r#"{"rect":{"max":{"x":0.30000001192092896,"y":0.4000000059604645},"min":{"x":0.10000000149011612,"y":0.20000000298023224}},"type":"bbox"}"#,
    )
}

#[test]
fn golden_image_geometry_polyline_locks_proportional_trace() -> Result<()> {
    // The image-space trace: a `ProportionalPolyline` nests under the named
    // `polyline` field. Locks the proportional-point wire form and its
    // validating `Deserialize` round-trip.
    let geometry = ImageGeometry::Polyline {
        polyline: ProportionalPolyline::new(vec![(0.1, 0.2), (0.3, 0.4)])?,
    };
    assert_golden_roundtrip(
        &geometry,
        r#"{"polyline":{"points":[{"x":0.10000000149011612,"y":0.20000000298023224},{"x":0.30000001192092896,"y":0.4000000059604645}]},"type":"polyline"}"#,
    )
}

// ---------------------------------------------------------------------------
// Per-SubmitFact-category goldens
// ---------------------------------------------------------------------------

#[test]
fn golden_submit_fact_factual() -> Result<()> {
    let assertion = FactualAssertion::Attribute {
        fact: attribute::Fact::Name {
            entity: EntityIdx(0),
            name: NameText::new("X"),
            language: en()?,
            name_type: NameType::Common,
            valid_from: None,
            valid_to: None,
        },
    };
    let fact = SubmitFact::Factual {
        assertion,
        citation: sample_factual_citation()?,
    };
    assert_golden_roundtrip(
        &fact,
        r#"{"assertion":{"fact":{"entity":0,"language":"en","name":"X","name_type":"common","type":"name","valid_from":null,"valid_to":null},"type":"attribute"},"citation":{"excerpts":["source-text"],"source":{"published":null,"type":"url","url":"https://example.com/source"}},"type":"factual"}"#,
    )
}

#[test]
fn golden_submit_fact_judgment() -> Result<()> {
    let assertion = JudgmentAssertion::Identity {
        fact: identity::Fact::same_entity(EntityIdx(0), EntityIdx(1))?,
    };
    let fact = SubmitFact::Judgment {
        assertion,
        citation: sample_judgment_source()?,
    };
    assert_golden_roundtrip(
        &fact,
        r#"{"assertion":{"fact":{"pair":{"a":0,"b":1},"type":"same_entity"},"type":"identity"},"citation":{"image":0,"observer":{"justification":null,"type":"user","user":"alice"},"region":null,"type":"image_observation"},"type":"judgment"}"#,
    )
}

#[test]
fn golden_submit_fact_judgment_derivation() -> Result<()> {
    let assertion = JudgmentAssertion::Identity {
        fact: identity::Fact::same_entity(EntityIdx(0), EntityIdx(1))?,
    };
    let fact = SubmitFact::Judgment {
        assertion,
        citation: sample_derivation_source()?,
    };
    assert_golden_roundtrip(
        &fact,
        r#"{"assertion":{"fact":{"pair":{"a":0,"b":1},"type":"same_entity"},"type":"identity"},"citation":{"basis":[3,5],"process":"matcher","snapshot":7,"type":"derivation","version":"test-version"},"type":"judgment"}"#,
    )
}

#[test]
fn golden_submit_fact_meta() -> Result<()> {
    let assertion = MetaAssertion::RetractFact {
        target: FactId::new(7),
        reason: crate::facts::assertions::RetractionReason::FactualError,
    };
    let fact = SubmitFact::Meta {
        assertion,
        citation: MetaSource::External {
            source: ExternalSource::Url {
                url: Url::parse("https://example.com/retract")?,
                published: None,
            },
        },
    };
    assert_golden_roundtrip(
        &fact,
        r#"{"assertion":{"reason":"factual_error","target":7,"type":"retract_fact"},"citation":{"source":{"published":null,"type":"url","url":"https://example.com/retract"},"type":"external"},"type":"meta"}"#,
    )
}

// ---------------------------------------------------------------------------
// Full Commit golden
// ---------------------------------------------------------------------------

/// Mixes a Factual and a Judgment fact under a Local entity decl and a User
/// author, then pins the canonical JCS the `CommitId` hashes. The JCS fully
/// determines the id, so this catches any wire-shape change that would
/// invalidate every existing commit hash, with the changed field visible.
#[test]
fn golden_commit_canonical_jcs_full_bundle() -> Result<()> {
    let mut facts = BTreeSet::new();
    facts.insert(SubmitFact::Factual {
        assertion: FactualAssertion::Attribute {
            fact: attribute::Fact::Name {
                entity: EntityIdx(0),
                name: NameText::new("Pantheon"),
                language: en()?,
                name_type: NameType::Common,
                valid_from: None,
                valid_to: None,
            },
        },
        citation: sample_factual_citation()?,
    });
    facts.insert(SubmitFact::Judgment {
        assertion: JudgmentAssertion::Identity {
            fact: identity::Fact::same_entity(EntityIdx(0), EntityIdx(1))?,
        },
        citation: sample_judgment_source()?,
    });

    let bundle: Commit<MemoryIds> = Commit {
        author: CommitAuthor::User(UserId::new("alice")),
        recorded_at: chrono::Utc
            .with_ymd_and_hms(2024, 1, 1, 12, 0, 0)
            .single()
            .ok_or("fixed time")?,
        entities: vec![Decl::Local, Decl::Local],
        events: Vec::new(),
        images: vec![Decl::Local],
        facts,
    };

    assert_golden(
        &bundle.canonical_jcs()?,
        r#"{"author":"user:alice","entities":[{"type":"local"},{"type":"local"}],"events":[],"facts":[{"assertion":{"fact":{"entity":0,"language":"en","name":"Pantheon","name_type":"common","type":"name","valid_from":null,"valid_to":null},"type":"attribute"},"citation":{"excerpts":["source-text"],"source":{"published":null,"type":"url","url":"https://example.com/source"}},"type":"factual"},{"assertion":{"fact":{"pair":{"a":0,"b":1},"type":"same_entity"},"type":"identity"},"citation":{"image":0,"observer":{"justification":null,"type":"user","user":"alice"},"region":null,"type":"image_observation"},"type":"judgment"}],"images":[{"type":"local"}],"recorded_at":"2024-01-01T12:00:00+00:00"}"#,
    );
    Ok(())
}

/// Pins the externally-tagged struct-variant shape of the machine author.
#[test]
fn golden_commit_author_analyzer() -> Result<()> {
    let author = CommitAuthor::Analyzer {
        process: AnalyzerProcess::new("matcher"),
        version: AnalyzerVersion::new("test-version"),
    };
    assert_golden_roundtrip(
        &author,
        r#"{"analyzer":{"process":"matcher","version":"test-version"}}"#,
    )
}

/// A companion-shaped bundle — analyzer author, `Existing` decls, one
/// `Derivation`-cited identity judgment — pinning the canonical JCS its
/// `CommitId` hashes. The JCS fully determines the id, so this catches any
/// wire-shape change to the machine-author canonical string or the derivation
/// citation. Built from fixed literals, so the bytes are independent of the
/// build environment.
#[test]
fn golden_commit_canonical_jcs_analyzer_companion_bundle() -> Result<()> {
    let mut facts = BTreeSet::new();
    facts.insert(SubmitFact::Judgment {
        assertion: JudgmentAssertion::Identity {
            fact: identity::Fact::same_entity(EntityIdx(0), EntityIdx(1))?,
        },
        citation: sample_derivation_source()?,
    });

    let bundle: Commit<MemoryIds> = Commit {
        author: CommitAuthor::Analyzer {
            process: AnalyzerProcess::new("matcher"),
            version: AnalyzerVersion::new("test-version"),
        },
        recorded_at: chrono::Utc
            .with_ymd_and_hms(2024, 1, 1, 12, 0, 0)
            .single()
            .ok_or("fixed time")?,
        entities: vec![
            Decl::Existing { id: ent(1)? },
            Decl::Existing { id: ent(2)? },
        ],
        events: Vec::new(),
        images: Vec::new(),
        facts,
    };

    assert_golden(
        &bundle.canonical_jcs()?,
        r#"{"author":"analyzer:matcher@test-version","entities":[{"id":1,"type":"existing"},{"id":2,"type":"existing"}],"events":[],"facts":[{"assertion":{"fact":{"pair":{"a":0,"b":1},"type":"same_entity"},"type":"identity"},"citation":{"basis":[3,5],"process":"matcher","snapshot":7,"type":"derivation","version":"test-version"},"type":"judgment"}],"images":[],"recorded_at":"2024-01-01T12:00:00+00:00"}"#,
    );
    Ok(())
}
