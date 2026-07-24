//! Shared fixtures for the conformance cases: producer-form fact and bundle
//! builders (over bundle-local indices, so they fit any id scheme), commit
//! helpers generic over the store, and walk-draining utilities.
//!
//! Backend-specific tests (the memory backend's own module, the matcher
//! tests) reuse these too, so a fixture lives here even when only one
//! consumer needs it.

use chrono::TimeZone;
use futures_util::TryStreamExt;
use url::Url;

use crate::date::{DatePrecision, UncertainDate};
use crate::geo::{GeoPoint, Meters, Viewport};
use crate::grammar::assertions::{FactualAssertion, JudgmentAssertion};
use crate::grammar::attribute::{self, NameText, NameType};
use crate::grammar::bookend;
use crate::grammar::citations::{Excerpt, ExternalReference, ExternalSource, FactualCitation};
use crate::grammar::citations::{JudgmentSource, Justification, Language};
use crate::grammar::identity;
use crate::grammar::ids::{CommitId, FactId, IdScheme, UserId};
use crate::grammar::lifecycle::{
    DamageCause, DurationalKind, DurationalRole, LifetimeEventKind, MoveMethod, PointKind,
};
use crate::location::{Location, LocationReference, UnresolvedLocation};
use crate::nonempty::NonEmptyVec;
use crate::store::pagination::paginate;
use crate::store::schema::{ClassRow, EntityStream, ImageStream, PageItem};
use crate::store::{
    EntityIdOf, EntityView, FactStore, ImageIdOf, ImageView, StoredFactOf, SubmitCommitError,
    SubmitCommitInput,
};
use crate::submit::{
    Commit as SubmitBundle, CommitAuthor, Decl, EntityIdx, EventIdx, ImageIdx, SubmitError,
    SubmitFact, SubmitResult, commit_facts,
};

use super::{TestError, TestResult};

/// The accumulated rejection batch of a submit under id scheme `R`.
pub type SchemeErrorBatch<R> = NonEmptyVec<
    SubmitError<<R as IdScheme>::Entity, <R as IdScheme>::Event, <R as IdScheme>::Image>,
>;

/// [`SchemeErrorBatch`] pinned to store `S`'s scheme.
pub type SubmitErrorBatch<S> = SchemeErrorBatch<<S as FactStore>::Ids>;

/// A page limit large enough to fit every fact the backlink tests submit.
pub const PAGE_100: std::num::NonZeroUsize = match std::num::NonZeroUsize::new(100) {
    Some(n) => n,
    None => std::num::NonZeroUsize::MIN,
};

/// Drain an entity class walk over `stream` to its rows at the given page
/// limit, threading the exclusive view through [`paginate`]'s walk state.
pub async fn drain_entity_classes<S, V>(
    view: &mut V,
    stream: &EntityStream<'_>,
    limit: std::num::NonZeroUsize,
) -> Result<Vec<ClassRow<EntityIdOf<S>>>, S::Error>
where
    S: FactStore,
    V: EntityView<S>,
{
    paginate(view, |v, cursor| async move {
        let page = v.walk_entity_classes(stream, cursor, limit).await?;
        let (rows, next) = page.into_parts();
        Ok::<_, S::Error>((rows, next, v))
    })
    .try_collect()
    .await
}

/// The image analogue of [`drain_entity_classes`].
pub async fn drain_image_classes<S, V>(
    view: &mut V,
    stream: &ImageStream<'_>,
    limit: std::num::NonZeroUsize,
) -> Result<Vec<ClassRow<ImageIdOf<S>>>, S::Error>
where
    S: FactStore,
    V: ImageView<S>,
{
    paginate(view, |v, cursor| async move {
        let page = v.walk_image_classes(stream, cursor, limit).await?;
        let (rows, next) = page.into_parts();
        Ok::<_, S::Error>((rows, next, v))
    })
    .try_collect()
    .await
}

/// Drain an entity's depiction walk to its rows at the given page limit,
/// threading the exclusive view through [`paginate`]'s walk state. Each row is
/// a raw depiction fact under its depicted-image `SameArtifact` rep.
pub async fn drain_entity_depictions<S, V>(
    view: &mut V,
    entity: &EntityIdOf<S>,
    limit: std::num::NonZeroUsize,
) -> Result<Vec<PageItem<StoredFactOf<S>, ImageIdOf<S>>>, S::Error>
where
    S: FactStore,
    V: EntityView<S>,
{
    paginate(view, |v, cursor| async move {
        let page = v.walk_entity_depictions(entity, cursor, limit).await?;
        Ok::<_, S::Error>((page.rows, page.next_class, v))
    })
    .try_collect()
    .await
}

/// The batch of `SubmitError`s a rejected submit carries, or a test failure if
/// the error was a backend failure. The single entry point every error-path
/// test uses to inspect the accumulated batch.
pub fn submit_batch<E, R>(err: SubmitCommitError<E, R>) -> Result<SchemeErrorBatch<R>, TestError>
where
    E: std::fmt::Debug,
    R: IdScheme,
{
    match err {
        SubmitCommitError::Submit(errs) => Ok(errs),
        other => Err(format!("expected Submit batch, got {other:?}").into()),
    }
}

// --- time, authorship, citations ---

/// A fixed UTC timestamp for test fixtures.
pub fn fixed_time() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc
        .with_ymd_and_hms(2024, 1, 1, 12, 0, 0)
        .single()
        .unwrap_or_default()
}

/// The user author every fixture commit records.
pub fn user_author() -> Result<CommitAuthor, TestError> {
    Ok(CommitAuthor::User(UserId::new("alice")))
}

/// A URL-sourced factual citation with one excerpt.
pub fn sample_citation() -> Result<FactualCitation, TestError> {
    let url = Url::parse("https://example.com/source")?;
    let source = ExternalSource::Url {
        url,
        published: None,
    };
    let excerpts = vec![Excerpt::new("source-text")?];
    Ok(FactualCitation::new(source, excerpts)?)
}

/// A personal-knowledge judgment citation.
pub fn judgment_citation() -> Result<JudgmentSource<ImageIdx>, TestError> {
    Ok(JudgmentSource::PersonalKnowledge {
        user: UserId::new("alice"),
        justification: Justification::new("a test judgment")?,
    })
}

/// An `ImageObservation` citation pointing at the given image index.
pub fn image_observation_citation(image_idx: usize) -> Result<JudgmentSource<ImageIdx>, TestError> {
    Ok(JudgmentSource::ImageObservation {
        image: ImageIdx(image_idx),
        region: None,
        observer: crate::grammar::citations::Observer::User {
            user: UserId::new("alice"),
            justification: None,
        },
    })
}

/// An `External`-cited judgment source (carries no observed image).
pub fn external_judgment_citation() -> Result<JudgmentSource<ImageIdx>, TestError> {
    Ok(JudgmentSource::External {
        source: ExternalSource::Url {
            url: Url::parse("https://example.com/observed")?,
            published: None,
        },
    })
}

// --- dates and locations ---

/// A year-precision [`UncertainDate`].
pub fn year_date(year: i32) -> Result<UncertainDate, TestError> {
    Ok(UncertainDate::with_precision(
        chrono::NaiveDate::from_ymd_opt(year, 1, 1).ok_or("date")?,
        DatePrecision::Year,
    )?)
}

/// A two-interval disjunction ("1920 or 1940"), built by joining two disjoint
/// year claims — the read-side shape a stored fact may not carry.
pub fn disjunctive_date() -> Result<UncertainDate, TestError> {
    let a = year_date(1920)?;
    let b = year_date(1940)?;
    let joined = a.join(&b);
    assert_eq!(joined.intervals().len(), 2, "expected a real disjunction");
    Ok(joined)
}

/// A symbolic named-place location reference.
pub fn sample_location() -> UnresolvedLocation {
    UnresolvedLocation::Reference(LocationReference::NamedPlace {
        name: "somewhere".to_owned(),
    })
}

/// A resolved `OneOf` of `n` distinct-center, equal-radius circles — none
/// subsumes another, so the union keeps every circle and the leaf count is
/// exactly `n`. Centers step along a meridian by 0.01° (~1.1 km), well clear of
/// the 1 m radii, so the disjoint members never collapse.
pub fn n_circle_location(n: usize) -> Result<UnresolvedLocation, TestError> {
    let mut circles = Vec::with_capacity(n);
    for i in 0..n {
        let lon = -120.0 + i as f64 * 0.01;
        circles.push(Location::circle(
            GeoPoint::new(0.0, lon)?,
            Meters::new_unchecked(1.0),
        )?);
    }
    Ok(UnresolvedLocation::Resolved(Location::one_of(circles)?))
}

/// The NYC-ish box the spatial walk tests query: lat `[40, 41]`, lon
/// `[-74, -73]`.
pub fn sample_viewport() -> Result<Viewport, TestError> {
    Ok(Viewport::new(
        GeoPoint::new(40.0, -74.0)?,
        GeoPoint::new(41.0, -73.0)?,
    )?)
}

// --- attribute facts ---

/// An English-language `Name` fact on the given entity index.
pub fn name_fact(entity_idx: usize, name: &str) -> Result<SubmitFact, TestError> {
    name_fact_in_language(entity_idx, name, "en")
}

/// A `Name` fact with an explicit language tag, canonicalized at
/// construction.
pub fn name_fact_in_language(
    entity_idx: usize,
    name: &str,
    language: &str,
) -> Result<SubmitFact, TestError> {
    let language = Language::new(language)?;
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Attribute {
            fact: attribute::Fact::Name {
                entity: EntityIdx(entity_idx),
                name: NameText::new(name)?,
                language,
                name_type: NameType::Common,
                valid_from: None,
                valid_to: None,
            },
        },
        citation: sample_citation()?,
    })
}

/// A `Name` fact with optional year-precision validity bounds.
pub fn name_window_fact(
    entity_idx: usize,
    valid_from: Option<i32>,
    valid_to: Option<i32>,
) -> Result<SubmitFact, TestError> {
    let language = Language::new("en")?;
    let valid_from = valid_from.map(year_date).transpose()?;
    let valid_to = valid_to.map(year_date).transpose()?;
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Attribute {
            fact: attribute::Fact::Name {
                entity: EntityIdx(entity_idx),
                name: NameText::new("name")?,
                language,
                name_type: NameType::Common,
                valid_from,
                valid_to,
            },
        },
        citation: sample_citation()?,
    })
}

/// An `ExternalReference` attribute fact pointing the entity at a Wikidata
/// QID.
pub fn external_reference_fact(entity_idx: usize, qid: u64) -> Result<SubmitFact, TestError> {
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

// --- bookend facts ---

/// A `Construction::Started` fact bounded in 1700.
pub fn construction_started_fact(entity_idx: usize) -> Result<SubmitFact, TestError> {
    construction_started_in(entity_idx, 1700)
}

/// A `Construction::Started` fact with a year-precision bound.
pub fn construction_started_in(entity_idx: usize, year: i32) -> Result<SubmitFact, TestError> {
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

/// A `Demolition::Completed` fact with a year-precision bound.
pub fn demolition_completed_in(entity_idx: usize, year: i32) -> Result<SubmitFact, TestError> {
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

/// A `Construction::Location` fact placing the entity at a named place.
pub fn construction_location_in(entity_idx: usize, place: &str) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Construction {
            fact: bookend::ConstructionFact::Location {
                entity: EntityIdx(entity_idx),
                location: UnresolvedLocation::Reference(LocationReference::NamedPlace {
                    name: place.to_owned(),
                }),
            },
        },
        citation: sample_citation()?,
    })
}

/// A `Construction` bookend carrying a symbolic location.
pub fn construction_location_fact(entity_idx: usize) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Construction {
            fact: bookend::ConstructionFact::Location {
                entity: EntityIdx(entity_idx),
                location: sample_location(),
            },
        },
        citation: sample_citation()?,
    })
}

/// A `Construction` bookend carrying an explicit location — the host for the
/// geometric-emptiness guard tests.
pub fn construction_with_location(
    entity_idx: usize,
    location: UnresolvedLocation,
) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Construction {
            fact: bookend::ConstructionFact::Location {
                entity: EntityIdx(entity_idx),
                location,
            },
        },
        citation: sample_citation()?,
    })
}

/// A construction `Location` bookend pinning the entity at a resolved point.
pub fn construction_at(entity_idx: usize, lat: f64, lon: f64) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Construction {
            fact: bookend::ConstructionFact::Location {
                entity: EntityIdx(entity_idx),
                location: UnresolvedLocation::Resolved(Location::point(GeoPoint::new(lat, lon)?)),
            },
        },
        citation: sample_citation()?,
    })
}

/// A construction `Location` bookend placing the entity in a resolved circle
/// of `radius_m` uncertainty.
pub fn construction_circle_at(
    entity_idx: usize,
    lat: f64,
    lon: f64,
    radius_m: f64,
) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Construction {
            fact: bookend::ConstructionFact::Location {
                entity: EntityIdx(entity_idx),
                location: UnresolvedLocation::Resolved(Location::circle(
                    GeoPoint::new(lat, lon)?,
                    Meters::new_unchecked(radius_m),
                )?),
            },
        },
        citation: sample_citation()?,
    })
}

/// A `Construction::Started` bookend carrying an arbitrary date — the
/// fact-payload host for the single-interval rule.
pub fn started_with_date(entity_idx: usize, bound: UncertainDate) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Construction {
            fact: bookend::ConstructionFact::Started {
                entity: EntityIdx(entity_idx),
                bound,
            },
        },
        citation: sample_citation()?,
    })
}

// --- image facts ---

/// An `image::Fact::Source` fact sourcing the image from `url`.
pub fn image_source_fact(image_idx: usize, url: &str) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Image {
            fact: crate::grammar::image::Fact::Source {
                image: ImageIdx(image_idx),
                url: Url::parse(url)?,
            },
        },
        citation: sample_citation()?,
    })
}

/// An image-touching `Medium` fact tagging the image as a picture.
pub fn medium_picture_fact(image_idx: usize) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Image {
            fact: crate::grammar::image::Fact::Medium {
                image: ImageIdx(image_idx),
                medium: crate::grammar::image::ImageMedium::Picture,
            },
        },
        citation: sample_citation()?,
    })
}

/// An image-touching `Medium` fact tagging the image as a map.
pub fn map_medium_fact(image_idx: usize) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Image {
            fact: crate::grammar::image::Fact::Medium {
                image: ImageIdx(image_idx),
                medium: crate::grammar::image::ImageMedium::Map,
            },
        },
        citation: sample_citation()?,
    })
}

/// An image-touching `CapturedDate` fact for the given image index.
pub fn captured_date_fact(image_idx: usize) -> Result<SubmitFact, TestError> {
    let bound = UncertainDate::with_precision(
        chrono::NaiveDate::from_ymd_opt(1850, 1, 1).ok_or("date")?,
        DatePrecision::Year,
    )?;
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Image {
            fact: crate::grammar::image::Fact::CapturedDate {
                image: ImageIdx(image_idx),
                bound,
            },
        },
        citation: sample_citation()?,
    })
}

/// A `CapturedLocation` fact placing the image's viewpoint in a resolved
/// circle of `radius_m` uncertainty (`0.0` for a bare point).
pub fn captured_location_at(
    image_idx: usize,
    lat: f64,
    lon: f64,
    radius_m: f64,
) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Image {
            fact: crate::grammar::image::Fact::CapturedLocation {
                image: ImageIdx(image_idx),
                location: UnresolvedLocation::Resolved(Location::circle(
                    GeoPoint::new(lat, lon)?,
                    Meters::new_unchecked(radius_m),
                )?),
            },
        },
        citation: sample_citation()?,
    })
}

// --- event facts ---

/// An event-touching `PointDate` fact for the given event index.
pub fn event_point_date_fact(event_idx: usize) -> Result<SubmitFact, TestError> {
    let bound = UncertainDate::with_precision(
        chrono::NaiveDate::from_ymd_opt(1850, 1, 1).ok_or("date")?,
        DatePrecision::Year,
    )?;
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Event {
            fact: crate::grammar::event::Fact::PointDate {
                event: EventIdx(event_idx),
                bound,
            },
        },
        citation: sample_citation()?,
    })
}

/// An event-touching `Description` fact for the given event index.
pub fn event_description_fact(event_idx: usize) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Event {
            fact: crate::grammar::event::Fact::Description {
                event: EventIdx(event_idx),
                text: "an interior event".to_owned(),
            },
        },
        citation: sample_citation()?,
    })
}

/// A fire `DamageCause` payload for the given event index.
pub fn event_damage_cause_fact(event_idx: usize) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Event {
            fact: crate::grammar::event::Fact::DamageCause {
                event: EventIdx(event_idx),
                cause: DamageCause::Fire,
            },
        },
        citation: sample_citation()?,
    })
}

/// A whole-structure `MoveMethod` payload for the given event index.
pub fn event_move_method_fact(event_idx: usize) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Event {
            fact: crate::grammar::event::Fact::MoveMethod {
                event: EventIdx(event_idx),
                method: MoveMethod::Whole,
            },
        },
        citation: sample_citation()?,
    })
}

/// A `DurationalDate` (started, 1850) payload for the given event index.
pub fn event_durational_date_fact(event_idx: usize) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Event {
            fact: crate::grammar::event::Fact::DurationalDate {
                event: EventIdx(event_idx),
                role: DurationalRole::Started,
                bound: year_date(1850)?,
            },
        },
        citation: sample_citation()?,
    })
}

/// A `MovedToLocation` payload — suits a `Moved` event, contradicts every other
/// kind.
pub fn event_moved_to_location_fact(event_idx: usize) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Event {
            fact: crate::grammar::event::Fact::MovedToLocation {
                event: EventIdx(event_idx),
                location: sample_location(),
            },
        },
        citation: sample_citation()?,
    })
}

/// A `MovedToLocation` payload landing the event's `Moved` at a resolved point.
pub fn moved_to(event_idx: usize, lat: f64, lon: f64) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Event {
            fact: crate::grammar::event::Fact::MovedToLocation {
                event: EventIdx(event_idx),
                location: UnresolvedLocation::Resolved(Location::point(GeoPoint::new(lat, lon)?)),
            },
        },
        citation: sample_citation()?,
    })
}

/// A `HasEvent` fact tying the event index to the entity index and declaring its
/// kind — the typing claim every minted event needs.
pub fn has_event_fact(
    event_idx: usize,
    entity_idx: usize,
    kind: LifetimeEventKind,
) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Event {
            fact: crate::grammar::event::Fact::HasEvent {
                entity: EntityIdx(entity_idx),
                event: EventIdx(event_idx),
                kind,
            },
        },
        citation: sample_citation()?,
    })
}

/// `Durational { Damaged }`.
pub fn damaged_kind() -> LifetimeEventKind {
    LifetimeEventKind::Durational {
        kind: DurationalKind::Damaged,
    }
}

/// `Durational { Moved }`.
pub fn moved_kind() -> LifetimeEventKind {
    LifetimeEventKind::Durational {
        kind: DurationalKind::Moved,
    }
}

/// `Point { Designated }`.
pub fn designated_kind() -> LifetimeEventKind {
    LifetimeEventKind::Point {
        kind: PointKind::Designated,
    }
}

/// A `Gap` fact whose earlier endpoint is the lifetime event `event_idx` and
/// whose later endpoint is `entity_idx`'s construction completion. References the
/// event without typing it — the shape that must still demand a `HasEvent`.
pub fn gap_from_event_fact(event_idx: usize, entity_idx: usize) -> Result<SubmitFact, TestError> {
    use crate::grammar::event::{Days, GapBounds, OrderableEvent};
    let bounds = GapBounds::new(
        OrderableEvent::Event {
            event: EventIdx(event_idx),
        },
        OrderableEvent::ConstructionCompletion {
            entity: EntityIdx(entity_idx),
        },
        Some(Days::new(1)),
        None,
    )?;
    Ok(SubmitFact::Factual {
        assertion: FactualAssertion::Gap { bounds },
        citation: sample_citation()?,
    })
}

// --- judgment facts ---

/// A `SameEntity` judgment between two entity indices.
pub fn same_entity_fact(a: usize, b: usize) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Judgment {
        assertion: JudgmentAssertion::Identity {
            fact: identity::Fact::same_entity(EntityIdx(a), EntityIdx(b))?,
        },
        citation: judgment_citation()?,
    })
}

/// A `SameEvent` judgment between two event indices.
pub fn same_event_fact(a: usize, b: usize) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Judgment {
        assertion: JudgmentAssertion::Identity {
            fact: identity::Fact::same_event(EventIdx(a), EventIdx(b))?,
        },
        citation: judgment_citation()?,
    })
}

/// A composite `IsSubimageOf` fact linking the two image indices.
pub fn subimage_fact(subimage_idx: usize, parent_idx: usize) -> Result<SubmitFact, TestError> {
    let region = crate::grammar::composites::SubimageRegion::rect(0.0, 0.0, 0.5, 0.5)?;
    Ok(SubmitFact::Judgment {
        assertion: JudgmentAssertion::Composite {
            fact: crate::grammar::composites::Fact::IsSubimageOf {
                subimage: ImageIdx(subimage_idx),
                parent: ImageIdx(parent_idx),
                region,
            },
        },
        citation: judgment_citation()?,
    })
}

/// A bare depiction tying `entity_idx` to `image_idx` — no localization, no
/// perspective, the common P18 shape.
pub fn depiction_fact(entity_idx: usize, image_idx: usize) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Judgment {
        assertion: JudgmentAssertion::Depiction {
            fact: crate::grammar::depiction::Fact {
                entity: EntityIdx(entity_idx),
                image: ImageIdx(image_idx),
                localization: None,
                perspective: None,
            },
        },
        citation: judgment_citation()?,
    })
}

/// A feature observation on `entity_idx`, cited by `citation`.
pub fn observation_feature_fact(
    entity_idx: usize,
    citation: JudgmentSource<ImageIdx>,
) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Judgment {
        assertion: JudgmentAssertion::Observation {
            fact: crate::grammar::observation::Fact::Feature {
                entity: EntityIdx(entity_idx),
                feature: crate::grammar::features::Feature::StoryCount { stories: 2 },
            },
        },
        citation,
    })
}

/// An `External`-cited judgment whose citation date is `published` — the
/// non-`Factual` citation host (`JudgmentSource::External`). A feature
/// observation cited externally trips no other rule.
pub fn observation_external_published(
    entity_idx: usize,
    published: Option<UncertainDate>,
) -> Result<SubmitFact, TestError> {
    let citation = JudgmentSource::External {
        source: ExternalSource::Url {
            url: Url::parse("https://example.com/observed")?,
            published,
        },
    };
    observation_feature_fact(entity_idx, citation)
}

// --- meta facts ---

/// A `RetractCommit` meta-fact targeting `target`. References no indices
/// (the target is a `CommitId`), so a bundle carrying only this fact
/// declares no entities / events / images.
pub fn retract_commit_fact(target: CommitId) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Meta {
        assertion: crate::grammar::assertions::MetaAssertion::RetractCommit {
            target,
            reason: crate::grammar::assertions::RetractionReason::FactualError,
        },
        citation: crate::grammar::citations::MetaSource::PersonalKnowledge {
            user: UserId::new("alice"),
            justification: Justification::new("The targeted commit is wrong.")?,
        },
    })
}

/// A `RetractFact` meta-fact targeting `target`. Declares no subjects (the
/// target is a `FactId`), mirroring [`retract_commit_fact`].
pub fn retract_fact(target: FactId) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Meta {
        assertion: crate::grammar::assertions::MetaAssertion::RetractFact {
            target,
            reason: crate::grammar::assertions::RetractionReason::FactualError,
        },
        citation: crate::grammar::citations::MetaSource::PersonalKnowledge {
            user: UserId::new("alice"),
            justification: Justification::new("The targeted fact is wrong.")?,
        },
    })
}

/// A `SupersedeFact` meta-fact replacing `target` with `replacement`.
pub fn supersede_fact(target: FactId, replacement: FactId) -> Result<SubmitFact, TestError> {
    Ok(SubmitFact::Meta {
        assertion: crate::grammar::assertions::MetaAssertion::SupersedeFact {
            target,
            replacement,
            reason: crate::grammar::assertions::RetractionReason::FactualError,
        },
        citation: crate::grammar::citations::MetaSource::PersonalKnowledge {
            user: UserId::new("alice"),
            justification: Justification::new("The targeted fact is superseded.")?,
        },
    })
}

// --- bundles and commits ---

/// A single-commit bundle of all-`Local` declarations.
pub fn local_bundle<R: IdScheme>(
    entities: usize,
    events: usize,
    images: usize,
    secs: i64,
    facts: Vec<SubmitFact>,
) -> Result<SubmitBundle<R>, TestError> {
    Ok(SubmitBundle {
        author: user_author()?,
        recorded_at: fixed_time() + chrono::Duration::seconds(secs),
        entities: vec![Decl::Local; entities],
        events: vec![Decl::Local; events],
        images: vec![Decl::Local; images],
        facts: facts.into_iter().collect(),
    })
}

/// Commit one `Name` fact on a fresh entity and return its `SubmitResult`.
pub async fn commit_name<S: FactStore>(
    store: &S,
    name: &str,
) -> Result<SubmitResult<S::Ids>, TestError> {
    let bundle: SubmitCommitInput<S> = SubmitBundle {
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
pub async fn commit_retract<S: FactStore>(
    store: &S,
    target: FactId,
    secs: i64,
) -> Result<SubmitResult<S::Ids>, TestError> {
    let bundle: SubmitCommitInput<S> = SubmitBundle {
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

/// Commit `bundle`, returning its `SubmitResult` or failing the test if it
/// was rejected.
pub async fn commit_result<S: FactStore>(
    store: &S,
    bundle: SubmitCommitInput<S>,
) -> Result<SubmitResult<S::Ids>, TestError> {
    Ok(commit_facts(store, bundle)
        .await
        .map_err(|e| format!("expected the commit to be accepted, got {e:?}"))?)
}

/// Commit `bundle`, failing the test if it was rejected.
pub async fn commit_ok<S: FactStore>(store: &S, bundle: SubmitCommitInput<S>) -> TestResult {
    commit_facts(store, bundle)
        .await
        .map_err(|e| format!("expected the commit to be accepted, got {e:?}"))?;
    Ok(())
}

/// Commit `bundle`, returning its rejection batch or failing if it was accepted.
pub async fn commit_err<S: FactStore>(
    store: &S,
    bundle: SubmitCommitInput<S>,
) -> Result<SubmitErrorBatch<S>, TestError> {
    match commit_facts(store, bundle).await {
        Ok(_) => Err("expected the commit to be rejected".into()),
        Err(e) => submit_batch(e),
    }
}
