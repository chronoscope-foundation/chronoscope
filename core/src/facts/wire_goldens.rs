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
use oxilangtag::LanguageTag;
use serde::Serialize;
use serde::de::DeserializeOwned;
use url::Url;

use crate::date::{DatePrecision, UncertainDate};
use crate::facts::assertions::{FactualAssertion, JudgmentAssertion, MetaAssertion};
use crate::facts::attribute::{self, EntityRelationType, NameType};
use crate::facts::bookend;
use crate::facts::citations::{
    Excerpt, ExternalReference, ExternalSource, FactualCitation, JudgmentSource, MetaSource,
    Observer,
};
use crate::facts::composites::{self, SubimageRegion};
use crate::facts::depiction::{self, Perspective};
use crate::facts::event;
use crate::facts::features::Feature;
use crate::facts::geometry::{ImageRegion, SpatialGeometry};
use crate::facts::identity;
use crate::facts::ids::{EntityId, FactId, ImageId, LifetimeEventId, UserId};
use crate::facts::image;
use crate::facts::lifecycle::{DurationalRole, MoveMethod};
use crate::facts::map;
use crate::facts::observation;
use crate::facts::picture;
use crate::facts::spatial::TopologicalRel;
use crate::facts::submit::{Commit, CommitAuthor, Decl, EntityIdx, SubmitFact};
use crate::geo::GeoPoint;
use crate::location::Location;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn ent(s: &str) -> std::result::Result<EntityId, std::convert::Infallible> {
    Ok(EntityId::new(s))
}
fn evt(s: &str) -> std::result::Result<LifetimeEventId, std::convert::Infallible> {
    Ok(LifetimeEventId::new(s))
}
fn img(s: &str) -> std::result::Result<ImageId, std::convert::Infallible> {
    Ok(ImageId::new(s))
}

fn en() -> std::result::Result<LanguageTag<String>, Box<dyn std::error::Error>> {
    Ok(LanguageTag::parse("en".to_owned())?)
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

fn sample_judgment_source() -> Result<JudgmentSource> {
    Ok(JudgmentSource::ImageObservation {
        image: img("img-1")?,
        region: None,
        observer: Observer::User {
            user: UserId::new("alice"),
            justification: None,
        },
    })
}

/// Assert `actual == expected`, with both strings in the panic message so a
/// regenerated golden is easy to copy in.
///
/// For serialize-only types: `SubmitFact` and `Commit` are built in Rust and
/// never deserialized, so they pin only the JCS bytes. Types that also
/// implement `DeserializeOwned` use [`assert_golden_roundtrip`].
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
    let f: attribute::Fact<EntityId> = attribute::Fact::Name {
        entity: ent("e-1")?,
        name: "Pantheon".to_owned(),
        language: en()?,
        name_type: NameType::Common,
        valid_from: None,
        valid_to: None,
    };
    assert_golden_roundtrip(
        &f,
        r#"{"entity":"e-1","language":"en","name":"Pantheon","name_type":"common","type":"name","valid_from":null,"valid_to":null}"#,
    )
}

#[test]
fn golden_attribute_fact_relationship() -> Result<()> {
    let f = attribute::Fact::relationship(ent("e-1")?, ent("e-2")?, EntityRelationType::Contains)?;
    assert_golden_roundtrip(
        &f,
        r#"{"pair":{"from":"e-1","to":"e-2"},"relation":"contains","type":"relationship"}"#,
    )
}

#[test]
fn golden_bookend_fact_started() -> Result<()> {
    let f: bookend::Fact<EntityId> = bookend::Fact::Started {
        entity: ent("e-1")?,
        bound: sample_date()?,
    };
    assert_golden_roundtrip(
        &f,
        r#"{"bound":{"earliest":{"date":"1700-01-01","precision":"year"},"latest":{"date":"1700-01-01","precision":"year"}},"entity":"e-1","type":"started"}"#,
    )
}

#[test]
fn golden_event_fact_durational_date() -> Result<()> {
    let f: event::Fact<EntityId, LifetimeEventId> = event::Fact::DurationalDate {
        event: evt("evt-1")?,
        role: DurationalRole::Started,
        bound: sample_date()?,
    };
    assert_golden_roundtrip(
        &f,
        r#"{"bound":{"earliest":{"date":"1700-01-01","precision":"year"},"latest":{"date":"1700-01-01","precision":"year"}},"event":"evt-1","role":"started","type":"durational_date"}"#,
    )
}

#[test]
fn golden_event_fact_move_method() -> Result<()> {
    let f: event::Fact<EntityId, LifetimeEventId> = event::Fact::MoveMethod {
        event: evt("evt-1")?,
        method: MoveMethod::Whole,
    };
    assert_golden_roundtrip(
        &f,
        r#"{"event":"evt-1","method":"whole","type":"move_method"}"#,
    )
}

#[test]
fn golden_image_fact_source() -> Result<()> {
    let f: image::Fact<ImageId> = image::Fact::Source {
        image: img("img-1")?,
        url: Url::parse("https://example.com/img")?,
    };
    assert_golden_roundtrip(
        &f,
        r#"{"image":"img-1","type":"source","url":"https://example.com/img"}"#,
    )
}

#[test]
fn golden_picture_fact_is_picture() -> Result<()> {
    let f: picture::Fact<ImageId> = picture::Fact::IsPicture {
        image: img("img-1")?,
    };
    assert_golden_roundtrip(&f, r#"{"image":"img-1","type":"is_picture"}"#)
}

#[test]
fn golden_map_fact_is_map() -> Result<()> {
    let f: map::Fact<ImageId> = map::Fact::IsMap {
        image: img("img-1")?,
    };
    assert_golden_roundtrip(&f, r#"{"image":"img-1","type":"is_map"}"#)
}

#[test]
fn golden_identity_fact_same_entity() -> Result<()> {
    let f = identity::Fact::<EntityId, LifetimeEventId, ImageId>::same_entity(
        ent("e-1")?,
        ent("e-2")?,
    )?;
    assert_golden_roundtrip(&f, r#"{"pair":{"a":"e-1","b":"e-2"},"type":"same_entity"}"#)
}

#[test]
fn golden_depiction_fact_in_picture() -> Result<()> {
    let f: depiction::Fact<EntityId, ImageId> = depiction::Fact::InPicture {
        entity: ent("e-1")?,
        image: img("img-1")?,
        perspective: Perspective::Exterior,
        region: None,
    };
    assert_golden_roundtrip(
        &f,
        r#"{"entity":"e-1","image":"img-1","perspective":"exterior","region":null,"type":"in_picture"}"#,
    )
}

#[test]
fn golden_observation_fact_feature() -> Result<()> {
    // `Feature` carries each claim in a named field (`{"type":..,"shape":..}`).
    // Naming the field lets internal tagging wrap a fieldless inner enum like
    // `RoofShape`; a bare newtype payload would flatten beside the tag and
    // collide.
    let f: observation::Fact<EntityId> = observation::Fact::Feature {
        entity: ent("e-1")?,
        feature: crate::facts::features::Feature::RoofShape {
            shape: crate::facts::features::RoofShape::Gabled,
        },
    };
    assert_golden_roundtrip(
        &f,
        r#"{"entity":"e-1","feature":{"shape":"gabled","type":"roof_shape"},"type":"feature"}"#,
    )
}

#[test]
fn golden_observation_fact_spatial() -> Result<()> {
    let f = observation::Fact::spatial(ent("e-1")?, ent("e-2")?, TopologicalRel::Adjacent)?;
    assert_golden_roundtrip(
        &f,
        r#"{"pair":{"from":"e-1","to":"e-2"},"relation":{"type":"adjacent"},"type":"spatial"}"#,
    )
}

#[test]
fn golden_composites_fact_is_subimage_of() -> Result<()> {
    let region = SubimageRegion::rect(0.0, 0.0, 0.5, 0.5)?;
    let f: composites::Fact<ImageId> = composites::Fact::IsSubimageOf {
        subimage: img("img-sub")?,
        parent: img("img-par")?,
        region,
    };
    assert_golden_roundtrip(
        &f,
        r#"{"parent":"img-par","region":{"rect":{"height":0.5,"width":0.5,"x":0,"y":0},"type":"rect"},"subimage":"img-sub","type":"is_subimage_of"}"#,
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
    let decl: Decl<EntityId> = Decl::Existing { id: ent("e-5")? };
    assert_golden_roundtrip(&decl, r#"{"id":"e-5","type":"existing"}"#)
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
fn golden_location_union_of_locks_nested_internal_tag() -> Result<()> {
    // `UnionOf` nests `Location` values themselves tagged on `type`. Locks
    // that the nesting keeps the tag keys in separate maps.
    let location = Location::union_of(vec![
        Location::circle(GeoPoint::new(48.8, 2.3)?, 10.0)?,
        Location::circle(GeoPoint::new(51.5, -0.1)?, 10.0)?,
    ])?;
    assert_golden_roundtrip(
        &location,
        r#"{"members":[{"center":{"lat":48.8,"lon":2.3},"radius_m":10,"type":"circle"},{"center":{"lat":51.5,"lon":-0.1},"radius_m":10,"type":"circle"}],"type":"union_of"}"#,
    )
}

#[test]
fn golden_spatial_geometry_region_bbox_locks_nested_tag() -> Result<()> {
    // `SpatialGeometry` nests its tagged `ImageRegion` payload under the named
    // `region` field so the inner `type` key stays in a separate map; the bbox
    // nests one level deeper under `rect`. The round-trip guards that
    // multi-level tagging.
    let geometry = SpatialGeometry::Region {
        region: ImageRegion::bbox(0.1, 0.2, 0.3, 0.4)?,
    };
    assert_golden_roundtrip(
        &geometry,
        r#"{"region":{"rect":{"height":0.4000000059604645,"width":0.30000001192092896,"x":0.10000000149011612,"y":0.20000000298023224},"type":"bbox"},"type":"region"}"#,
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
            name: "X".to_owned(),
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
        r#"{"assertion":{"fact":{"pair":{"a":0,"b":1},"type":"same_entity"},"type":"identity"},"citation":{"image":"img-1","observer":{"justification":null,"type":"user","user":"alice"},"region":null,"type":"image_observation"},"type":"judgment"}"#,
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
/// author, then pins the resulting `CommitId`. Catches any wire-shape change
/// that would invalidate every existing commit hash.
#[test]
fn golden_commit_id_full_bundle() -> Result<()> {
    let mut facts = BTreeSet::new();
    facts.insert(SubmitFact::Factual {
        assertion: FactualAssertion::Attribute {
            fact: attribute::Fact::Name {
                entity: EntityIdx(0),
                name: "Pantheon".to_owned(),
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

    let bundle: Commit<EntityId, LifetimeEventId, ImageId> = Commit {
        author: CommitAuthor::User(UserId::new("alice")),
        recorded_at: chrono::Utc
            .with_ymd_and_hms(2024, 1, 1, 12, 0, 0)
            .single()
            .ok_or("fixed time")?,
        entities: vec![Decl::Local, Decl::Local],
        events: Vec::new(),
        images: Vec::new(),
        facts,
    };

    let id = bundle.id()?;
    assert_golden(
        id.as_str(),
        "238a5f549dc36b779c10e89b915fb8c2fa6977ca3485fb31d7dcd0be4434c3e8",
    );
    Ok(())
}
