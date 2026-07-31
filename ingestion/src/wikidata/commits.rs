//! Fact-store commit construction from Wikidata entities.
//!
//! Shapes the Wikidata extraction layer (`parsing`, `handlers`, `lifecycle`)
//! into `submit::Commit`s: one commit per Wikidata item, declarations minted
//! local, facts referencing them by bundle-local index.

use chrono::{DateTime, Utc};

use chronoscope_core::external_ids::{WikidataEntityId, WikidataPropertyId};
use chronoscope_core::grammar::assertions::{FactualAssertion, JudgmentAssertion};
use chronoscope_core::grammar::attribute::{self, EntityRelationType};
use chronoscope_core::grammar::bookend;
use chronoscope_core::grammar::citations::{
    Excerpt, ExcerptError, ExternalReference, ExternalSource, FactualCitation, JudgmentSource,
    WikidataField,
};
use chronoscope_core::grammar::depiction::{self, Perspective};
use chronoscope_core::grammar::event;
use chronoscope_core::grammar::existence;
use chronoscope_core::grammar::ids::{IdScheme, IngesterRunId, ValidatedStringError};
use chronoscope_core::grammar::image::{self, ImageMedium};
use chronoscope_core::grammar::lifecycle::DurationalRole;
use chronoscope_core::grammar::text::Text;
use chronoscope_core::nonempty::NonEmptyVec;
use chronoscope_core::store::FactStore;
use chronoscope_core::submit::{
    BundleLocal, Commit, CommitAuthor, Decl, EntityIdx, EventIdx, ImageIdx, SubmitFact,
    commit_facts,
};
use chronoscope_integrations::wikidata::{CommonsFilename, WikidataEntity, url_for_filename};

use crate::wikidata::handlers::extract_link_references;
use crate::wikidata::lifecycle::{CitedDate, Contribution, EventShape, build_lifecycles};
use crate::wikidata::parsing::{extract_names, parse_sitelink};
use crate::wikidata::{CitationError, ItemContext, asserted_claims};

/// A failure while turning a Wikidata entity into a commit.
///
/// The structural backstop, not the boundary's rejection channel: a value the
/// store can't hold costs the image, reference or name it belongs to and leaves
/// a warning, so what remains here is the entity's own scaffolding — its
/// citation context and the edges between its splits. Distinct again from "skip
/// this entity" (a non-Q id), which is `Ok(None)`.
#[derive(Debug)]
pub enum BuildError {
    /// A rejection, named by the entity field the value was read from.
    Field {
        /// Where the value came from, as an operator would name it —
        /// `"predecessor demolition value"`.
        field: &'static str,
        /// Why it was rejected.
        source: Box<BuildError>,
    },
    /// A citation excerpt was empty or over-length.
    Excerpt(ExcerptError),
    /// A grammar text value held a character the fact store can't store.
    Text(ValidatedStringError),
    /// A `Replaces` relationship's endpoints collapsed to one entity.
    SelfRelationship,
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // The wrapped rejections name themselves ("excerpt too long",
            // "Text contains a NUL"), so the field is all the framing a log
            // line needs.
            Self::Field { field, source } => write!(f, "{field}: {source}"),
            Self::Excerpt(e) => write!(f, "{e}"),
            Self::Text(e) => write!(f, "{e}"),
            Self::SelfRelationship => {
                write!(f, "replaces relationship collapsed to a self-loop")
            }
        }
    }
}

impl std::error::Error for BuildError {}

/// Name the field a rejected value was read from.
///
/// An item carries dozens of strings, and a bare rejection ("… too long") says
/// which rule was broken but not which string broke it. A dropped entity leaves
/// one warning line behind, so the field has to travel in it.
pub trait FieldContext<T> {
    /// Attribute a rejection to `field`.
    fn in_field(self, field: &'static str) -> Result<T, BuildError>;
}

impl<T, E: Into<BuildError>> FieldContext<T> for Result<T, E> {
    fn in_field(self, field: &'static str) -> Result<T, BuildError> {
        self.map_err(|e| BuildError::Field {
            field,
            source: Box::new(e.into()),
        })
    }
}

impl From<ExcerptError> for BuildError {
    fn from(e: ExcerptError) -> Self {
        Self::Excerpt(e)
    }
}

impl From<ValidatedStringError> for BuildError {
    fn from(e: ValidatedStringError) -> Self {
        Self::Text(e)
    }
}

impl From<CitationError> for BuildError {
    fn from(e: CitationError) -> Self {
        match e {
            CitationError::Excerpt(e) => Self::Excerpt(e),
            CitationError::Value(e) => Self::Text(e),
        }
    }
}

/// Turn one Wikidata entity into one fact-store commit.
///
/// `None` when the id isn't a `Q`-item (a property or lexeme carries no
/// Chronoscope facts). `recorded_at` is a parameter so the commit is
/// content-stable across runs of the same snapshot.
///
/// One item may split into several chronological entities (a demolish→rebuild
/// pattern); every split, its events, its images, and the `Replaces` edges
/// between adjacent splits go in the one commit, all declared `Local`.
///
/// A value the fact store can't hold costs the thing it belongs to and nothing
/// more — an unstorable filename costs its image, an unstorable title costs its
/// reference. Those skips join the extraction skips (unparseable dates,
/// uncitable names, unrecognized event QIDs) in `warnings` for the ingest driver
/// to surface; they don't fail the build. `Err` is left for the entity's own
/// scaffolding failing, which no dump value can provoke.
pub fn build_commit<R: IdScheme>(
    entity: &WikidataEntity,
    run: &IngesterRunId,
    recorded_at: DateTime<Utc>,
    warnings: &mut Vec<String>,
) -> Result<Option<Commit<R>>, BuildError> {
    let Ok(entity_id) = WikidataEntityId::parse(entity.id.as_str()) else {
        return Ok(None);
    };
    let ctx = ItemContext::new(entity_id, entity.lastrevid.0)?;

    let names = extract_names(entity, &ctx, warnings);
    let refs = collect_external_references(entity, &ctx, warnings);

    let (mut splits, lifecycle_warnings) = build_lifecycles(&entity.claims, &ctx);
    warnings.extend(lifecycle_warnings);
    if splits.is_empty() {
        splits.push(Vec::new());
    }
    let n_splits = splits.len();
    let latest = n_splits - 1;

    let mut facts: Vec<SubmitFact> = Vec::new();
    for name in &names {
        for k in 0..n_splits {
            facts.push(attribute_fact(
                attribute::Fact::Name {
                    entity: EntityIdx(k),
                    name: name.name.clone(),
                    language: name.language.clone(),
                    name_type: name.name_type,
                    valid_from: None,
                    valid_to: None,
                },
                name.citation.clone(),
            ));
        }
    }
    for (reference, citation) in &refs {
        for k in 0..n_splits {
            facts.push(attribute_fact(
                attribute::Fact::ExternalReference {
                    entity: EntityIdx(k),
                    reference: reference.clone(),
                },
                citation.clone(),
            ));
        }
    }
    let event_count = push_lifecycle_facts(&mut facts, &splits);
    let image_count = push_image_facts(&mut facts, entity, latest, &ctx, warnings);
    push_replaces_facts(&mut facts, &splits, &ctx)?;

    Ok(Some(Commit {
        author: CommitAuthor::Ingester(run.clone()),
        recorded_at,
        entities: vec![Decl::Local; n_splits],
        events: vec![Decl::Local; event_count],
        images: vec![Decl::Local; image_count],
        facts: facts.into_iter().collect(),
    }))
}

// ============================================================================
// External references — on every split
// ============================================================================

/// Every external reference the item carries: its own QID, its sitelinks, and
/// its link-property references (OSM, Pleiades, URLs). Each is paired with the
/// citation naming where it was read from. Emitted on every split — a lookup by
/// external ref returns the whole demolish→rebuild set, not one split.
///
/// A reference whose site, title or claim value the store can't hold is dropped
/// with a warning naming it. The item's own QID reference is built from the
/// parsed entity id, so an item always carries at least that one.
fn collect_external_references(
    entity: &WikidataEntity,
    ctx: &ItemContext,
    warnings: &mut Vec<String>,
) -> Vec<(ExternalReference, FactualCitation)> {
    let mut refs = Vec::new();

    refs.push((
        ExternalReference::Wikidata {
            qid: ctx.entity_id(),
        },
        ctx.item_citation(),
    ));

    for (site, sitelink) in &entity.sitelinks {
        let site = site.as_str();
        let Some(reference) = parse_sitelink(site, sitelink.title.as_str(), warnings) else {
            continue;
        };
        let field = match Text::new(site) {
            Ok(site) => WikidataField::Sitelink { site },
            Err(e) => {
                warnings.push(format!("sitelink {site}: {e}"));
                continue;
            }
        };
        match ctx.citation(field, sitelink.title.clone()) {
            Ok(citation) => refs.push((reference, citation)),
            Err(e) => warnings.push(format!("sitelink {site}: {e}")),
        }
    }

    let (links, issues) = extract_link_references(&entity.claims);
    warnings.extend(issues);
    for link in links {
        let property_id = link.property_id;
        match ctx.statement_citation(property_id, link.raw) {
            Ok(citation) => refs.push((link.reference, citation)),
            Err(e) => warnings.push(format!("{property_id}: {e}")),
        }
    }

    refs
}

// ============================================================================
// Bookends + interior events — per split
// ============================================================================

/// Emit each split's bookend and interior-event facts, minting one `EventIdx`
/// per interior event. Returns the total interior-event count so the caller
/// sizes the event declaration list.
fn push_lifecycle_facts(facts: &mut Vec<SubmitFact>, splits: &[Vec<Contribution>]) -> usize {
    let mut event_count = 0usize;
    for (k, contributions) in splits.iter().enumerate() {
        let entity = EntityIdx(k);
        for contribution in contributions {
            match contribution {
                Contribution::Construction {
                    started,
                    completed,
                    location,
                } => {
                    for d in started {
                        facts.push(construction_fact(
                            bookend::ConstructionFact::Started {
                                entity,
                                bound: d.bound.clone(),
                            },
                            d.citation.clone(),
                        ));
                    }
                    for d in completed {
                        facts.push(construction_fact(
                            bookend::ConstructionFact::Completed {
                                entity,
                                bound: d.bound.clone(),
                            },
                            d.citation.clone(),
                        ));
                    }
                    for l in location {
                        facts.push(construction_fact(
                            bookend::ConstructionFact::Location {
                                entity,
                                location: l.location.clone(),
                            },
                            l.citation.clone(),
                        ));
                    }
                }
                Contribution::Demolition { started, completed } => {
                    for d in started {
                        facts.push(demolition_fact(
                            bookend::DemolitionFact::Started {
                                entity,
                                bound: d.bound.clone(),
                            },
                            d.citation.clone(),
                        ));
                    }
                    for d in completed {
                        facts.push(demolition_fact(
                            bookend::DemolitionFact::Completed {
                                entity,
                                bound: d.bound.clone(),
                            },
                            d.citation.clone(),
                        ));
                    }
                }
                Contribution::Existence { dates } => {
                    for d in dates {
                        facts.push(existence_fact(
                            existence::Fact {
                                entity,
                                at: d.bound.clone(),
                            },
                            d.citation.clone(),
                        ));
                    }
                }
                Contribution::Event(interior) => {
                    let event = EventIdx(event_count);
                    event_count += 1;
                    facts.push(event_fact(
                        event::Fact::HasEvent {
                            entity,
                            event,
                            kind: interior.shape.kind(),
                        },
                        interior.citation.clone(),
                    ));
                    match &interior.shape {
                        EventShape::Durational {
                            started, completed, ..
                        } => {
                            push_durational_dates(facts, event, started, DurationalRole::Started);
                            push_durational_dates(
                                facts,
                                event,
                                completed,
                                DurationalRole::Completed,
                            );
                        }
                        EventShape::Point { at, .. } => {
                            for d in at {
                                facts.push(event_fact(
                                    event::Fact::PointDate {
                                        event,
                                        bound: d.bound.clone(),
                                    },
                                    d.citation.clone(),
                                ));
                            }
                        }
                    }
                    if let Some(payload) = &interior.payload {
                        facts.push(event_fact(payload.fact(event), interior.citation.clone()));
                    }
                }
            }
        }
    }
    event_count
}

/// A durational event's date facts for one role, one per parallel bound.
fn push_durational_dates(
    facts: &mut Vec<SubmitFact>,
    event: EventIdx,
    bounds: &[CitedDate],
    role: DurationalRole,
) {
    for d in bounds {
        facts.push(event_fact(
            event::Fact::DurationalDate {
                event,
                role,
                bound: d.bound.clone(),
            },
            d.citation.clone(),
        ));
    }
}

// ============================================================================
// Images + depictions — on the latest split only
// ============================================================================

/// Emit `Source` + `Medium` facts and a depiction judgment for each image the
/// item names, attaching depictions to the latest split (a photo shows the
/// current building, not a demolished predecessor). Returns the image count.
///
/// A filename the store can't hold costs that one image: it is warned about and
/// passed over before an `ImageIdx` is minted, so the declaration list stays
/// dense and the item's other photos are unaffected.
fn push_image_facts(
    facts: &mut Vec<SubmitFact>,
    entity: &WikidataEntity,
    latest: usize,
    ctx: &ItemContext,
    warnings: &mut Vec<String>,
) -> usize {
    let mut image_count = 0usize;
    for (property, claims) in &entity.claims {
        let Some((medium, perspective)) = image_medium_perspective(property.as_str()) else {
            continue;
        };
        let Ok(property_id) = WikidataPropertyId::parse(property.as_str()) else {
            continue;
        };
        for claim in asserted_claims(claims) {
            let Some(filename) = claim.mainsnak.string_value() else {
                continue;
            };
            let value = match Text::new(filename) {
                Ok(value) => value,
                Err(e) => {
                    warnings.push(format!("{property_id}: image filename: {e}"));
                    continue;
                }
            };
            let citation = match ctx.statement_citation(property_id, filename) {
                Ok(citation) => citation,
                Err(e) => {
                    warnings.push(format!("{property_id}: image filename: {e}"));
                    continue;
                }
            };

            let url = url_for_filename(&CommonsFilename(filename.to_owned()));
            let image = ImageIdx(image_count);
            image_count += 1;

            facts.push(image_fact(
                image::Fact::Source { image, url },
                citation.clone(),
            ));
            facts.push(image_fact(image::Fact::Medium { image, medium }, citation));

            facts.push(SubmitFact::Judgment {
                assertion: JudgmentAssertion::Depiction {
                    fact: depiction::Fact {
                        entity: EntityIdx(latest),
                        image,
                        localization: None,
                        perspective,
                    },
                },
                citation: JudgmentSource::External {
                    source: ExternalSource::Wikidata {
                        entity_id: ctx.entity_id(),
                        field: WikidataField::Statement { property_id },
                        revision_id: ctx.revision_id(),
                        value,
                    },
                },
            });
        }
    }
    image_count
}

/// The medium and depiction perspective an image property carries: photos
/// (P18/P3451) are exterior pictures, P5775 interiors, P3311 ("plan view image")
/// plans with no perspective. `None` for non-image properties.
fn image_medium_perspective(property: &str) -> Option<(ImageMedium, Option<Perspective>)> {
    match property {
        "P18" | "P3451" => Some((ImageMedium::Picture, Some(Perspective::Exterior))),
        "P5775" => Some((ImageMedium::Picture, Some(Perspective::Interior))),
        "P3311" => Some((ImageMedium::Plan, None)),
        _ => None,
    }
}

// ============================================================================
// Replaces — between adjacent splits
// ============================================================================

/// Emit a `Replaces` edge from each newer split to the older one it succeeds,
/// cited from the predecessor's demolition and the successor's inception raw
/// values.
fn push_replaces_facts(
    facts: &mut Vec<SubmitFact>,
    splits: &[Vec<Contribution>],
    ctx: &ItemContext,
) -> Result<(), BuildError> {
    for k in 0..splits.len().saturating_sub(1) {
        let older = EntityIdx(k);
        let newer = EntityIdx(k + 1);
        let fact = attribute::Fact::relationship(newer, older, EntityRelationType::Replaces)
            .map_err(|_| BuildError::SelfRelationship)?;

        let mut excerpts = Vec::new();
        if let Some(v) = bookend_raw_value(&splits[k], BookendPhase::Demolition) {
            excerpts.push(Excerpt::new(v).in_field("predecessor demolition value")?);
        }
        if let Some(v) = bookend_raw_value(&splits[k + 1], BookendPhase::Construction) {
            excerpts.push(Excerpt::new(v).in_field("successor construction value")?);
        }
        let excerpts = match NonEmptyVec::try_from_vec(excerpts) {
            Ok(excerpts) => excerpts,
            Err(_) => NonEmptyVec::singleton(Excerpt::new("demolish→rebuild")?),
        };

        let source = ExternalSource::Wikidata {
            entity_id: ctx.entity_id(),
            field: WikidataField::Item,
            revision_id: ctx.revision_id(),
            value: Text::new("demolish→rebuild")?,
        };
        facts.push(attribute_fact(fact, FactualCitation { source, excerpts }));
    }
    Ok(())
}

/// Which bookend of a split to read a raw date from, for the `Replaces` excerpt.
#[derive(Clone, Copy)]
enum BookendPhase {
    Construction,
    Demolition,
}

/// The raw observed value behind a split's `phase` bookend — its completion
/// date's citation excerpt, else its start's.
fn bookend_raw_value(split: &[Contribution], phase: BookendPhase) -> Option<String> {
    for contribution in split {
        let (started, completed) = match (phase, contribution) {
            (
                BookendPhase::Construction,
                Contribution::Construction {
                    started, completed, ..
                },
            )
            | (BookendPhase::Demolition, Contribution::Demolition { started, completed }) => {
                (started, completed)
            }
            _ => continue,
        };
        return completed
            .first()
            .or(started.first())
            .map(|d| d.citation.excerpts.first().as_str().to_owned());
    }
    None
}

// ============================================================================
// SubmitFact constructors
// ============================================================================

fn attribute_fact(fact: attribute::Fact<BundleLocal>, citation: FactualCitation) -> SubmitFact {
    SubmitFact::Factual {
        assertion: FactualAssertion::Attribute { fact },
        citation,
    }
}

fn construction_fact(
    fact: bookend::ConstructionFact<BundleLocal>,
    citation: FactualCitation,
) -> SubmitFact {
    SubmitFact::Factual {
        assertion: FactualAssertion::Construction { fact },
        citation,
    }
}

fn demolition_fact(
    fact: bookend::DemolitionFact<BundleLocal>,
    citation: FactualCitation,
) -> SubmitFact {
    SubmitFact::Factual {
        assertion: FactualAssertion::Demolition { fact },
        citation,
    }
}

fn existence_fact(fact: existence::Fact<BundleLocal>, citation: FactualCitation) -> SubmitFact {
    SubmitFact::Factual {
        assertion: FactualAssertion::Existence { fact },
        citation,
    }
}

fn event_fact(fact: event::Fact<BundleLocal>, citation: FactualCitation) -> SubmitFact {
    SubmitFact::Factual {
        assertion: FactualAssertion::Event { fact },
        citation,
    }
}

fn image_fact(fact: image::Fact<BundleLocal>, citation: FactualCitation) -> SubmitFact {
    SubmitFact::Factual {
        assertion: FactualAssertion::Image { fact },
        citation,
    }
}

// ============================================================================
// Submit driver
// ============================================================================

/// A running tally of an ingest pass.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct IngestStats {
    /// Commits successfully submitted.
    pub commits: usize,
    /// Entity declarations across those commits (splits included).
    pub entities: usize,
    /// Facts across those commits.
    pub facts: usize,
    /// Entities skipped because their id wasn't a `Q`-item — a property or
    /// lexeme carries nothing to model, so there was no commit to build.
    pub skipped: usize,
    /// Entities dropped because their commit wouldn't build. Unlike `skipped`,
    /// there was something to model and the build refused it. Values the store
    /// can't hold are counted in `issues` instead — they cost their own fact,
    /// not the entity — so this counts only an entity whose own scaffolding
    /// failed.
    pub failed: usize,
    /// Extraction warnings across those commits — uncitable names, unparseable
    /// dates, unrecognized event QIDs, and other per-claim skips.
    pub issues: usize,
}

impl std::ops::AddAssign for IngestStats {
    fn add_assign(&mut self, rhs: Self) {
        // Destructured so a new counter forces an update here at compile time
        // rather than silently going unfolded.
        let IngestStats {
            commits,
            entities,
            facts,
            skipped,
            failed,
            issues,
        } = rhs;
        self.commits += commits;
        self.entities += entities;
        self.facts += facts;
        self.skipped += skipped;
        self.failed += failed;
        self.issues += issues;
    }
}

/// The store rejected a commit, ending the ingest pass.
///
/// Holds the backend error rendered — backend errors aren't `Eq`.
#[derive(Debug)]
pub struct IngestError(String);

impl std::fmt::Display for IngestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "submit commit: {}", self.0)
    }
}

impl std::error::Error for IngestError {}

/// Build and submit one commit per entity, tallying as it goes.
///
/// An entity whose commit won't build is warned about, counted in
/// [`IngestStats::failed`], and passed over, so a bulk run over millions of
/// items outlives one malformed record. It is the backstop rather than the
/// common path: a value the store can't hold costs its own fact and lands in
/// [`IngestStats::issues`], leaving the entity to commit what remains.
///
/// A store rejection stays fatal. It can't be told apart from a store outage,
/// and a run that carried on through one would report success having ingested
/// nothing.
pub async fn ingest_entities<S: FactStore>(
    store: &S,
    entities: impl IntoIterator<Item = WikidataEntity>,
    run: &IngesterRunId,
    recorded_at: DateTime<Utc>,
) -> Result<IngestStats, IngestError> {
    let mut stats = IngestStats::default();
    for entity in entities {
        let mut warnings = Vec::new();
        let built = build_commit(&entity, run, recorded_at, &mut warnings);

        // Surfaced whatever the outcome: a build that then fails has already
        // collected the skips behind it, and one pass over the dump is meant to
        // name everything an operator has to fix.
        for warning in &warnings {
            tracing::warn!(qid = entity.id.as_str(), "ingestion: {warning}");
        }
        stats.issues += warnings.len();

        match built {
            Ok(Some(commit)) => {
                stats.entities += commit.entities.len();
                stats.facts += commit.facts.len();
                commit_facts(store, commit)
                    .await
                    .map_err(|e| IngestError(format!("{e:?}")))?;
                stats.commits += 1;
            }
            Ok(None) => stats.skipped += 1,
            Err(e) => {
                tracing::warn!(qid = entity.id.as_str(), "ingestion: entity dropped: {e}");
                stats.failed += 1;
            }
        }
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::{BTreeMap, BTreeSet};
    use std::num::NonZeroUsize;

    use chrono::{Datelike, TimeZone};

    use chronoscope_core::geo::{GeoPoint, Viewport};
    use chronoscope_core::grammar::attribute::NameType;
    use chronoscope_core::grammar::lifecycle::{DurationalKind, LifetimeEventKind, PointKind};
    use chronoscope_core::grammar::text::TEXT_MAX_LEN;
    use chronoscope_core::lifespan::ExistenceState;
    use chronoscope_core::listing::{EntitySummary, summaries_in_viewport};
    use chronoscope_core::projection::{member_lineage, project_entity};
    use chronoscope_core::store::FactStore;
    use chronoscope_core::store::memory::{
        MemoryEntityId, MemoryFactStore, MemoryIds, MemoryImageId,
    };
    use chronoscope_core::typed;
    use chronoscope_integrations::wikidata::{
        Claim, CoordinateValue, DataValue, EntityRefValue, Label, LanguageCode, PropertyId, Rank,
        RevisionId, SiteId, Sitelink, Snak, TimeValue, WikidataEntityType, WikidataId,
        WikidataPrecision, WikidataTimestamp,
    };

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn fixed_time() -> Result<DateTime<Utc>, Box<dyn std::error::Error>> {
        Ok(Utc
            .with_ymd_and_hms(2024, 1, 1, 12, 0, 0)
            .single()
            .ok_or("unambiguous timestamp")?)
    }

    fn time_claim(
        time: &str,
        precision: WikidataPrecision,
    ) -> Result<Claim, Box<dyn std::error::Error>> {
        Ok(Claim::simple(Snak::Value(DataValue::Time(TimeValue {
            time: WikidataTimestamp::try_from(time.to_owned())?,
            precision,
        }))))
    }

    fn coordinate_claim(lat: f64, lon: f64) -> Claim {
        Claim::simple(Snak::Value(DataValue::GlobeCoordinate(CoordinateValue {
            latitude: lat,
            longitude: lon,
            precision: Some(0.0001),
        })))
    }

    fn string_claim(value: &str) -> Claim {
        Claim::simple(Snak::Value(DataValue::String(value.to_owned())))
    }

    /// A P793 significant-event claim with a P585 point-in-time qualifier.
    fn p793_event(
        qid: &str,
        point: &str,
        precision: WikidataPrecision,
    ) -> Result<Claim, Box<dyn std::error::Error>> {
        let mut qualifiers = BTreeMap::new();
        qualifiers.insert(
            PropertyId::try_from("P585".to_owned())?,
            vec![Snak::Value(DataValue::Time(TimeValue {
                time: WikidataTimestamp::try_from(point.to_owned())?,
                precision,
            }))],
        );
        Ok(Claim {
            mainsnak: Snak::Value(DataValue::WikibaseEntityId(EntityRefValue {
                id: WikidataId::try_from(qid.to_owned())?,
            })),
            qualifiers,
            rank: Rank::Normal,
        })
    }

    fn label(lang: &str, value: &str) -> (LanguageCode, Label) {
        (
            LanguageCode(lang.to_owned()),
            Label {
                language: LanguageCode(lang.to_owned()),
                value: value.to_owned(),
            },
        )
    }

    fn item(
        qid: &str,
        labels: BTreeMap<LanguageCode, Label>,
        claims: BTreeMap<PropertyId, Vec<Claim>>,
    ) -> Result<WikidataEntity, Box<dyn std::error::Error>> {
        Ok(WikidataEntity {
            id: WikidataId::try_from(qid.to_owned())?,
            entity_type: WikidataEntityType::Item,
            lastrevid: RevisionId(100),
            labels,
            claims,
            sitelinks: BTreeMap::new(),
        })
    }

    /// A realistic item: two labels, a P571 inception, P625 coordinates, a P18
    /// image, and one P793 fire (an interior `Damaged` event).
    fn pantheon() -> Result<WikidataEntity, Box<dyn std::error::Error>> {
        let labels = BTreeMap::from([label("en", "Pantheon"), label("it", "Pantheon (Roma)")]);
        let claims = BTreeMap::from([
            (
                PropertyId::try_from("P571".to_owned())?,
                vec![time_claim(
                    "+1800-01-01T00:00:00Z",
                    WikidataPrecision::Year,
                )?],
            ),
            (
                PropertyId::try_from("P625".to_owned())?,
                vec![coordinate_claim(41.8986, 12.4769)],
            ),
            (
                PropertyId::try_from("P18".to_owned())?,
                vec![string_claim("Pantheon Rome.jpg")],
            ),
            (
                PropertyId::try_from("P793".to_owned())?,
                vec![p793_event(
                    "Q168983",
                    "+1900-01-01T00:00:00Z",
                    WikidataPrecision::Year,
                )?],
            ),
        ]);
        item("Q1234", labels, claims)
    }

    fn run_id() -> Result<IngesterRunId, ValidatedStringError> {
        IngesterRunId::new("wikidata-test")
    }

    #[test]
    fn build_commit_emits_names_qid_bookend_event_and_image() -> TestResult {
        let commit =
            build_commit::<MemoryIds>(&pantheon()?, &run_id()?, fixed_time()?, &mut Vec::new())?
                .ok_or("a Q-item builds a commit")?;

        assert_eq!(commit.entities.len(), 1, "no demolish→rebuild, one entity");
        assert_eq!(commit.events.len(), 1, "the fire is one interior event");
        assert_eq!(commit.images.len(), 1, "the P18 photo is one image");

        let name = |lang: &str, text: &str| {
            commit.facts.iter().any(|f| {
                if let SubmitFact::Factual {
                    assertion:
                        FactualAssertion::Attribute {
                            fact:
                                attribute::Fact::Name {
                                    entity,
                                    name,
                                    language,
                                    name_type,
                                    ..
                                },
                        },
                    ..
                } = f
                {
                    *entity == EntityIdx(0)
                        && name.as_str() == text
                        && language.as_str() == lang
                        && *name_type == NameType::Common
                } else {
                    false
                }
            })
        };
        assert!(
            name("en", "Pantheon"),
            "english label projects to a name fact"
        );
        assert!(name("it", "Pantheon (Roma)"), "italian label projects too");

        let expected_ref = ExternalReference::Wikidata {
            qid: WikidataEntityId::new(1234),
        };
        assert!(
            commit.facts.iter().any(|f| {
                if let SubmitFact::Factual {
                    assertion:
                        FactualAssertion::Attribute {
                            fact: attribute::Fact::ExternalReference { entity, reference },
                        },
                    ..
                } = f
                {
                    *entity == EntityIdx(0) && *reference == expected_ref
                } else {
                    false
                }
            }),
            "the item's own QID is an external reference"
        );

        assert!(
            commit.facts.iter().any(|f| matches!(
                f,
                SubmitFact::Factual {
                    assertion: FactualAssertion::Construction {
                        fact: bookend::ConstructionFact::Started { entity, .. }
                    },
                    ..
                } if *entity == EntityIdx(0)
            )),
            "the P571 inception is the construction start bookend"
        );

        assert!(
            commit.facts.iter().any(|f| matches!(
                f,
                SubmitFact::Factual {
                    assertion: FactualAssertion::Construction {
                        fact: bookend::ConstructionFact::Location { entity, .. }
                    },
                    ..
                } if *entity == EntityIdx(0)
            )),
            "the P625 coordinate is the construction location bookend"
        );

        assert!(
            commit.facts.iter().any(|f| matches!(
                f,
                SubmitFact::Factual {
                    assertion: FactualAssertion::Event {
                        fact: event::Fact::HasEvent {
                            kind: LifetimeEventKind::Durational {
                                kind: DurationalKind::Damaged
                            },
                            ..
                        }
                    },
                    ..
                }
            )),
            "the fire is a Damaged interior event"
        );

        assert!(
            commit.facts.iter().any(|f| matches!(
                f,
                SubmitFact::Factual {
                    assertion: FactualAssertion::Image {
                        fact: image::Fact::Source { .. }
                    },
                    ..
                }
            )),
            "the P18 photo has an image source fact"
        );
        assert!(
            commit.facts.iter().any(|f| matches!(
                f,
                SubmitFact::Judgment {
                    assertion: JudgmentAssertion::Depiction { .. },
                    ..
                }
            )),
            "the photo depicts the entity"
        );
        Ok(())
    }

    #[test]
    fn deprecated_image_claim_is_not_ingested() -> TestResult {
        let mut deprecated = string_claim("Old retouched.jpg");
        deprecated.rank = Rank::Deprecated;
        let claims = BTreeMap::from([(
            PropertyId::try_from("P18".to_owned())?,
            vec![deprecated, string_claim("Current.jpg")],
        )]);
        let commit = build_commit::<MemoryIds>(
            &item("Q7", BTreeMap::new(), claims)?,
            &run_id()?,
            fixed_time()?,
            &mut Vec::new(),
        )?
        .ok_or("expected commit")?;

        assert_eq!(commit.images.len(), 1, "the deprecated photo is dropped");
        assert!(
            commit.facts.iter().any(|f| matches!(
                f,
                SubmitFact::Factual {
                    assertion: FactualAssertion::Image {
                        fact: image::Fact::Source { url, .. }
                    },
                    ..
                } if url.as_str().contains("Current.jpg")
            )),
            "the non-deprecated photo survives"
        );
        Ok(())
    }

    #[test]
    fn opening_event_carries_no_usage_change_payload() -> TestResult {
        let claims = BTreeMap::from([(
            PropertyId::try_from("P1619".to_owned())?,
            vec![time_claim(
                "+1855-06-01T00:00:00Z",
                WikidataPrecision::Month,
            )?],
        )]);
        let commit = build_commit::<MemoryIds>(
            &item("Q8", BTreeMap::new(), claims)?,
            &run_id()?,
            fixed_time()?,
            &mut Vec::new(),
        )?
        .ok_or("expected commit")?;

        assert!(
            commit.facts.iter().any(|f| matches!(
                f,
                SubmitFact::Factual {
                    assertion: FactualAssertion::Event {
                        fact: event::Fact::HasEvent {
                            kind: LifetimeEventKind::Point {
                                kind: PointKind::UsageChanged
                            },
                            ..
                        }
                    },
                    ..
                }
            )),
            "the opening mints a UsageChanged event"
        );
        assert!(
            commit.facts.iter().any(|f| matches!(
                f,
                SubmitFact::Factual {
                    assertion: FactualAssertion::Event {
                        fact: event::Fact::PointDate { .. }
                    },
                    ..
                }
            )),
            "the opening date is a point-date fact"
        );
        assert!(
            !commit.facts.iter().any(|f| matches!(
                f,
                SubmitFact::Factual {
                    assertion: FactualAssertion::Event {
                        fact: event::Fact::UsageChange { .. }
                    },
                    ..
                }
            )),
            "an opening asserts the change, not a usage set"
        );
        Ok(())
    }

    /// A demolish→rebuild item: the commit carries two entities, a `Replaces`
    /// edge citing the bookend raw values, and a dateless predecessor location.
    fn rebuilt_church() -> Result<WikidataEntity, Box<dyn std::error::Error>> {
        let claims = BTreeMap::from([
            (
                PropertyId::try_from("P625".to_owned())?,
                vec![coordinate_claim(45.2177, 12.2790)],
            ),
            (
                PropertyId::try_from("P793".to_owned())?,
                vec![
                    p793_event("Q331483", "+1623-01-01T00:00:00Z", WikidataPrecision::Year)?,
                    p793_event("Q385378", "+1633-01-01T00:00:00Z", WikidataPrecision::Year)?,
                ],
            ),
        ]);
        item("Q9", BTreeMap::new(), claims)
    }

    #[test]
    fn demolish_rebuild_emits_replaces_edge_citing_bookend_dates() -> TestResult {
        let commit = build_commit::<MemoryIds>(
            &rebuilt_church()?,
            &run_id()?,
            fixed_time()?,
            &mut Vec::new(),
        )?
        .ok_or("expected commit")?;
        assert_eq!(commit.entities.len(), 2, "the rebuild splits the item");

        let replaces = commit
            .facts
            .iter()
            .find_map(|f| {
                if let SubmitFact::Factual {
                    assertion:
                        FactualAssertion::Attribute {
                            fact: attribute::Fact::Relationship { pair, relation },
                        },
                    citation,
                } = f
                {
                    (*relation == EntityRelationType::Replaces).then_some((pair, citation))
                } else {
                    None
                }
            })
            .ok_or("expected a Replaces relationship")?;
        let (pair, citation) = replaces;
        assert_eq!(*pair.from(), EntityIdx(1), "the newer split replaces");
        assert_eq!(*pair.to(), EntityIdx(0), "the older split is replaced");

        let excerpts: Vec<&str> = citation.excerpts.iter().map(Excerpt::as_str).collect();
        assert_eq!(
            excerpts,
            vec![
                "Q331483 P585:+1623-01-01T00:00:00Z",
                "Q385378 P585:+1633-01-01T00:00:00Z",
            ],
            "the edge quotes the demolition and reconstruction raw values"
        );
        Ok(())
    }

    #[test]
    fn predecessor_inherits_location_without_dates() -> TestResult {
        let commit = build_commit::<MemoryIds>(
            &rebuilt_church()?,
            &run_id()?,
            fixed_time()?,
            &mut Vec::new(),
        )?
        .ok_or("expected commit")?;

        assert!(
            commit.facts.iter().any(|f| matches!(
                f,
                SubmitFact::Factual {
                    assertion: FactualAssertion::Construction {
                        fact: bookend::ConstructionFact::Location { entity, .. }
                    },
                    ..
                } if *entity == EntityIdx(0)
            )),
            "the predecessor stands where its replacement stands"
        );
        assert!(
            !commit.facts.iter().any(|f| matches!(
                f,
                SubmitFact::Factual {
                    assertion: FactualAssertion::Construction {
                        fact: bookend::ConstructionFact::Started { entity, .. }
                            | bookend::ConstructionFact::Completed { entity, .. }
                    },
                    ..
                } if *entity == EntityIdx(0)
            )),
            "no construction date is invented for the predecessor"
        );
        Ok(())
    }

    #[tokio::test]
    async fn round_trip_projects_names_refs_and_timeline() -> TestResult {
        let store = MemoryFactStore::new();
        let recorded = fixed_time()?;
        let commit = build_commit(&pantheon()?, &run_id()?, recorded, &mut Vec::new())?
            .ok_or("expected commit")?;
        let result = commit_facts(&store, commit)
            .await
            .map_err(|e| format!("{e:?}"))?;
        let id = result
            .entities
            .get(&EntityIdx(0))
            .ok_or("entity 0 resolved")?
            .id;

        let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
        let (class, projected) =
            project_entity::<MemoryFactStore, _, _>(&mut view, id, member_lineage)
                .await?
                .ok_or("entity should project")?;
        let entity = typed::Entity::parse(&projected, &class);

        let texts: BTreeSet<&str> = entity.names.iter().map(|n| n.text.as_str()).collect();
        assert_eq!(
            texts,
            BTreeSet::from(["Pantheon", "Pantheon (Roma)"]),
            "both labels project back as names"
        );

        let expected_ref = ExternalReference::Wikidata {
            qid: WikidataEntityId::new(1234),
        };
        assert!(
            entity.external_refs.iter().any(|r| r.value == expected_ref),
            "the QID reference reads back off the projection"
        );

        // P571 dates the build, so the Constructed row reads back with its
        // start date.
        let construction_start = entity
            .timeline
            .events()
            .iter()
            .find_map(|entry| {
                if let typed::EventDetail::Constructed { period, .. } = &entry.detail {
                    Some(period.started.possible.earliest())
                } else {
                    None
                }
            })
            .flatten();
        assert_eq!(
            construction_start.map(|d| d.year()),
            Some(1800),
            "the P571 inception dates the construction start"
        );

        assert!(
            entity.timeline.events().iter().any(|entry| matches!(
                &entry.detail,
                typed::EventDetail::Interior {
                    kind: typed::InteriorEvent::Damaged { .. },
                    ..
                }
            )),
            "the fire reads back as a Damaged timeline event"
        );
        Ok(())
    }

    #[tokio::test]
    async fn ingest_entities_tallies_and_skips_non_q_items() -> TestResult {
        let store = MemoryFactStore::new();
        let stats = ingest_entities(&store, [pantheon()?], &run_id()?, fixed_time()?).await?;
        assert_eq!(stats.commits, 1);
        assert_eq!(stats.entities, 1);
        assert_eq!(stats.skipped, 0);
        assert_eq!(stats.failed, 0);
        assert_eq!(stats.issues, 0, "the clean fixture raises no warnings");
        assert!(stats.facts > 0, "the commit carries facts");
        Ok(())
    }

    #[tokio::test]
    async fn ingest_tallies_extraction_warnings_and_still_commits() -> TestResult {
        // An unrecognized P793 event QID is dropped with a warning; the item
        // still commits its other facts.
        let claims = BTreeMap::from([(
            PropertyId::try_from("P793".to_owned())?,
            vec![p793_event(
                "Q99999999",
                "+1920-01-01T00:00:00Z",
                WikidataPrecision::Year,
            )?],
        )]);
        let entity = item("Q123", BTreeMap::from([label("en", "Mystery")]), claims)?;

        let store = MemoryFactStore::new();
        let stats = ingest_entities(&store, [entity], &run_id()?, fixed_time()?).await?;
        assert_eq!(stats.commits, 1, "the item still commits");
        assert!(
            stats.issues > 0,
            "the unrecognized event QID is tallied as an issue"
        );
        Ok(())
    }

    fn external_reference_count(commit: &Commit<MemoryIds>) -> usize {
        commit
            .facts
            .iter()
            .filter(|f| {
                matches!(
                    f,
                    SubmitFact::Factual {
                        assertion: FactualAssertion::Attribute {
                            fact: attribute::Fact::ExternalReference { .. }
                        },
                        ..
                    }
                )
            })
            .count()
    }

    /// An item carrying one sitelink, for the skip paths.
    fn item_with_sitelink(
        site: &str,
        title: String,
    ) -> Result<WikidataEntity, Box<dyn std::error::Error>> {
        let mut entity = item(
            "Q321",
            BTreeMap::from([label("en", "Placeholder")]),
            BTreeMap::new(),
        )?;
        entity
            .sitelinks
            .insert(SiteId(site.to_owned()), Sitelink { title });
        Ok(entity)
    }

    #[test]
    fn empty_sitelink_title_leaves_the_commit_a_reference_short() -> TestResult {
        let entity = item_with_sitelink("commonswiki", String::new())?;

        let mut warnings = Vec::new();
        let commit = build_commit::<MemoryIds>(&entity, &run_id()?, fixed_time()?, &mut warnings)?
            .ok_or("the item still builds a commit")?;

        // The item's own QID is the only external reference; the empty-title
        // sitelink names no page, so there is nothing to build from it.
        assert_eq!(
            external_reference_count(&commit),
            1,
            "only the QID reference survives"
        );
        assert!(
            warnings.iter().any(|w| w.contains("commonswiki")),
            "the empty title is diagnosed, got: {warnings:?}"
        );
        Ok(())
    }

    /// A Commons gallery title (no `Category:` prefix) parses straight into a
    /// URL reference, so an unstorable title first bites at the citation step.
    /// It costs that one reference: the item's labels, dates and coordinates
    /// have nothing to do with a title Commons happens to carry.
    #[tokio::test]
    async fn unstorable_commons_gallery_title_costs_its_reference_not_its_entity() -> TestResult {
        for title in ["Gal\u{0}lery".to_owned(), "G".repeat(TEXT_MAX_LEN + 1)] {
            let entity = item_with_sitelink("commonswiki", title)?;

            let mut warnings = Vec::new();
            let commit =
                build_commit::<MemoryIds>(&entity, &run_id()?, fixed_time()?, &mut warnings)?
                    .ok_or("the item still builds a commit")?;
            assert_eq!(
                external_reference_count(&commit),
                1,
                "the sitelink reference is dropped, the QID one stays"
            );
            assert!(
                warnings.iter().any(|w| w.contains("commonswiki")),
                "the dropped reference is diagnosed, got: {warnings:?}"
            );
            assert!(
                commit.facts.iter().any(|f| matches!(
                    f,
                    SubmitFact::Factual {
                        assertion: FactualAssertion::Attribute {
                            fact: attribute::Fact::Name { .. }
                        },
                        ..
                    }
                )),
                "the item's label still commits"
            );

            let store = MemoryFactStore::new();
            let stats = ingest_entities(&store, [entity], &run_id()?, fixed_time()?).await?;
            assert_eq!(stats.commits, 1, "the entity is submitted");
            assert_eq!(stats.failed, 0, "a bad value is not a build failure");
            assert!(stats.issues > 0, "the drop is tallied as an issue");
        }
        Ok(())
    }

    /// An unstorable filename costs its own image and mints no `ImageIdx`, so
    /// the images behind it keep the indices their facts reference — a gap
    /// would leave the declaration list disagreeing with the facts.
    #[test]
    fn an_unstorable_image_filename_costs_that_image_and_leaves_no_index_gap() -> TestResult {
        let claims = BTreeMap::from([(
            PropertyId::try_from("P18".to_owned())?,
            vec![string_claim("Bro\u{0}ken.jpg"), string_claim("Current.jpg")],
        )]);

        let mut warnings = Vec::new();
        let commit = build_commit::<MemoryIds>(
            &item("Q11", BTreeMap::new(), claims)?,
            &run_id()?,
            fixed_time()?,
            &mut warnings,
        )?
        .ok_or("the item still builds a commit")?;

        assert_eq!(
            commit.images.len(),
            1,
            "only the storable photo is declared"
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("P18") && w.contains("image filename")),
            "the dropped image is diagnosed by property and field, got: {warnings:?}"
        );
        assert!(
            commit.facts.iter().any(|f| matches!(
                f,
                SubmitFact::Factual {
                    assertion: FactualAssertion::Image {
                        fact: image::Fact::Source { image, url }
                    },
                    ..
                } if *image == ImageIdx(0) && url.as_str().contains("Current.jpg")
            )),
            "the surviving photo takes the first declared index"
        );
        Ok(())
    }

    /// A link statement whose value can't be quoted costs that reference alone.
    #[test]
    fn an_unquotable_link_statement_costs_its_reference_not_its_entity() -> TestResult {
        let claims = BTreeMap::from([(
            PropertyId::try_from("P856".to_owned())?,
            vec![string_claim("https://example.com/a\u{0}b")],
        )]);

        let mut warnings = Vec::new();
        let commit = build_commit::<MemoryIds>(
            &item("Q12", BTreeMap::from([label("en", "Placeholder")]), claims)?,
            &run_id()?,
            fixed_time()?,
            &mut warnings,
        )?
        .ok_or("the item still builds a commit")?;

        assert_eq!(
            external_reference_count(&commit),
            1,
            "the official-website reference is dropped, the QID one stays"
        );
        // The URL itself parses — a NUL percent-encodes into the path — so the
        // rejection lands where the citation quotes the raw claim value.
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("P856") && w.contains("NUL")),
            "the dropped reference is diagnosed with its reason, got: {warnings:?}"
        );
        Ok(())
    }

    /// One item full of values the store can't hold must not cost the items
    /// queued around it — a bulk run over millions would otherwise be lost to
    /// it. Every rejection lands on the fact it belongs to, so all three commit.
    #[tokio::test]
    async fn an_item_whose_values_are_all_rejected_still_commits_alongside_its_neighbours()
    -> TestResult {
        let mut bad = item_with_sitelink("commonswiki", "Gal\u{0}lery".to_owned())?;
        bad.sitelinks.insert(
            SiteId("enwiki".to_owned()),
            Sitelink {
                title: "Pan\u{0}theon".to_owned(),
            },
        );
        bad.claims.insert(
            PropertyId::try_from("P18".to_owned())?,
            vec![string_claim("Bro\u{0}ken.jpg")],
        );
        let after = item(
            "Q999",
            BTreeMap::from([label("en", "Behind the bad one")]),
            BTreeMap::new(),
        )?;

        let store = MemoryFactStore::new();
        let stats =
            ingest_entities(&store, [pantheon()?, bad, after], &run_id()?, fixed_time()?).await?;

        assert_eq!(stats.commits, 3, "every item commits what it could");
        assert_eq!(stats.failed, 0, "no item is dropped over a bad value");
        assert!(stats.issues >= 3, "each rejection is tallied: {stats:?}");
        Ok(())
    }

    /// Sites outside the modeled set are filtered by design, so they leave the
    /// issue list alone — a dump's non-edition `*wiki` sites would otherwise
    /// bury the diagnostics that name a lost reference.
    #[test]
    fn sites_outside_the_modeled_set_are_skipped_in_silence() -> TestResult {
        for site in ["be_x_oldwiki", "mediawikiwiki", "enwikiquote"] {
            let entity = item_with_sitelink(site, "Placeholder".to_owned())?;

            let mut warnings = Vec::new();
            let commit =
                build_commit::<MemoryIds>(&entity, &run_id()?, fixed_time()?, &mut warnings)?
                    .ok_or("the item still builds a commit")?;

            assert_eq!(
                external_reference_count(&commit),
                1,
                "{site} contributes no reference"
            );
            assert!(
                warnings.is_empty(),
                "{site} is filtered in silence, got: {warnings:?}"
            );
        }
        Ok(())
    }

    /// The Pantheon's P625 coordinate, the point the viewport listing pins it at.
    const PANTHEON_LAT: f64 = 41.8986;
    const PANTHEON_LON: f64 = 12.4769;

    /// Commit one item into a fresh store, returning the store and the id
    /// minted for its first entity so a listing test can query the same
    /// snapshot.
    async fn committed(
        entity: &WikidataEntity,
    ) -> Result<(MemoryFactStore, MemoryEntityId), Box<dyn std::error::Error>> {
        let store = MemoryFactStore::new();
        let commit = build_commit::<MemoryIds>(entity, &run_id()?, fixed_time()?, &mut Vec::new())?
            .ok_or("expected commit")?;
        let result = commit_facts(&store, commit)
            .await
            .map_err(|e| format!("{e:?}"))?;
        let id = result
            .entities
            .get(&EntityIdx(0))
            .ok_or("entity 0 resolved")?
            .id;
        Ok((store, id))
    }

    /// The summaries a viewport listing returns for `viewport` as of `as_of`.
    async fn summaries_at(
        store: &MemoryFactStore,
        viewport: &Viewport,
        as_of: chrono::NaiveDate,
    ) -> Result<Vec<EntitySummary<MemoryEntityId, MemoryImageId>>, Box<dyn std::error::Error>> {
        let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;
        let limit = NonZeroUsize::new(16).ok_or("nonzero limit")?;
        let page =
            summaries_in_viewport::<MemoryFactStore, _>(&mut view, viewport, None, limit, as_of)
                .await
                .map_err(|e| format!("{e:?}"))?;
        Ok(page.summaries)
    }

    #[tokio::test]
    async fn viewport_listing_surfaces_pantheon_at_its_coordinate() -> TestResult {
        let (store, id) = committed(&pantheon()?).await?;

        // A box around central Rome, comfortably covering the P625 point.
        let viewport = Viewport::new(GeoPoint::new(41.8, 12.4)?, GeoPoint::new(42.0, 12.6)?)?;
        let summaries = summaries_at(&store, &viewport, chrono::NaiveDate::MIN).await?;

        let summary = summaries
            .iter()
            .find(|s| s.id == id)
            .ok_or("the ingested Pantheon surfaces in a box covering its coordinate")?;
        assert_eq!(
            summary.point,
            GeoPoint::new(PANTHEON_LAT, PANTHEON_LON)?,
            "the summary pins the entity at its P625 coordinate"
        );
        assert!(
            summary.names.iter().any(|n| n.text == "Pantheon"),
            "the summary carries the ingested name: {:?}",
            summary.names
        );
        // The P571 inception (1800) starts the construction and anchors the
        // span's earliest bound; the fire (1900) is a later interior event.
        assert_eq!(
            summary.earliest.map(|d| d.year()),
            Some(1800),
            "the P571 construction start anchors the summary's earliest span bound"
        );
        Ok(())
    }

    #[tokio::test]
    async fn viewport_listing_omits_pantheon_from_a_faraway_box() -> TestResult {
        let (store, id) = committed(&pantheon()?).await?;

        // A box over the mid-Atlantic — nowhere near Rome.
        let viewport = Viewport::new(GeoPoint::new(0.0, -40.0)?, GeoPoint::new(10.0, -30.0)?)?;
        let summaries = summaries_at(&store, &viewport, chrono::NaiveDate::MIN).await?;

        assert!(
            summaries.iter().all(|s| s.id != id),
            "the spatial filter excludes the Pantheon from a box that omits its \
             coordinate: {summaries:?}"
        );
        Ok(())
    }

    /// The listing's existence verdict for `id` at `as_of`, over `viewport`.
    async fn existence_at(
        store: &MemoryFactStore,
        id: MemoryEntityId,
        viewport: &Viewport,
        as_of: chrono::NaiveDate,
    ) -> Result<ExistenceState, Box<dyn std::error::Error>> {
        Ok(summaries_at(store, viewport, as_of)
            .await?
            .iter()
            .find(|s| s.id == id)
            .ok_or("the entity surfaces in a box covering its coordinate")?
            .existence)
    }

    /// The commonest Wikidata shape — a P571 inception and a P625 coordinate,
    /// nothing else — places the entity in time as well as space: the map holds
    /// it absent until the inception, then standing.
    #[tokio::test]
    async fn an_inception_and_a_coordinate_date_the_entity_on_the_map() -> TestResult {
        let claims = BTreeMap::from([
            (
                PropertyId::try_from("P571".to_owned())?,
                vec![time_claim("+1889-03-31T00:00:00Z", WikidataPrecision::Day)?],
            ),
            (
                PropertyId::try_from("P625".to_owned())?,
                vec![coordinate_claim(48.8584, 2.2945)],
            ),
        ]);
        let tower = item(
            "Q243",
            BTreeMap::from([label("en", "Eiffel Tower")]),
            claims,
        )?;
        let (store, id) = committed(&tower).await?;

        let viewport = Viewport::new(GeoPoint::new(48.8, 2.2)?, GeoPoint::new(48.9, 2.4)?)?;
        let year = |y: i32| chrono::NaiveDate::from_ymd_opt(y, 7, 1).ok_or("valid date");
        assert_eq!(
            existence_at(&store, id, &viewport, year(1850)?).await?,
            ExistenceState::Absent,
            "rewound past the 1889 inception, the tower was not there yet"
        );
        assert_eq!(
            existence_at(&store, id, &viewport, year(1900)?).await?,
            ExistenceState::Presumed,
            "past the inception with no removal on record, it stands"
        );
        Ok(())
    }
}
