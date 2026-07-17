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
use chronoscope_core::grammar::ids::{IdScheme, IngesterRunId};
use chronoscope_core::grammar::image::{self, ImageMedium};
use chronoscope_core::grammar::lifecycle::DurationalRole;
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
use crate::wikidata::{ItemContext, asserted_claims};

/// A failure while turning a Wikidata entity into a commit.
///
/// Distinct from "skip this entity" (a non-Q id), which is `Ok(None)`: these are
/// malformed inputs the boundary rejects.
#[derive(Debug)]
pub enum BuildError {
    /// A citation excerpt was empty or over-length.
    Excerpt(ExcerptError),
    /// A `Replaces` relationship's endpoints collapsed to one entity.
    SelfRelationship,
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Excerpt(e) => write!(f, "citation excerpt: {e}"),
            Self::SelfRelationship => {
                write!(f, "replaces relationship collapsed to a self-loop")
            }
        }
    }
}

impl std::error::Error for BuildError {}

impl From<ExcerptError> for BuildError {
    fn from(e: ExcerptError) -> Self {
        Self::Excerpt(e)
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
/// Extraction skips (unparseable dates, uncitable names, unrecognized event
/// QIDs) are collected into `warnings` for the ingest driver to surface; they
/// don't fail the build.
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
    let refs = collect_external_references(entity, &ctx, warnings)?;

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
    let image_count = push_image_facts(&mut facts, entity, latest, &ctx)?;
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
fn collect_external_references(
    entity: &WikidataEntity,
    ctx: &ItemContext,
    warnings: &mut Vec<String>,
) -> Result<Vec<(ExternalReference, FactualCitation)>, BuildError> {
    let mut refs = Vec::new();

    refs.push((
        ExternalReference::Wikidata {
            qid: ctx.entity_id(),
        },
        ctx.item_citation(),
    ));

    for (site, sitelink) in &entity.sitelinks {
        let title = sitelink.title.as_str();
        match parse_sitelink(site.as_str(), title) {
            Some(reference) => {
                let citation = ctx.citation(
                    WikidataField::Sitelink {
                        site: site.as_str().to_owned(),
                    },
                    sitelink.title.clone(),
                )?;
                refs.push((reference, citation));
            }
            // An empty title is malformed and worth a diagnostic; a non-empty
            // title on an unsupported project (wikiquote, wikisource) is
            // filtered by design and stays silent.
            None if title.is_empty() => {
                warnings.push(format!("sitelink {site}: empty title"));
            }
            None => {}
        }
    }

    let (links, issues) = extract_link_references(&entity.claims);
    warnings.extend(issues);
    for link in links {
        let citation = ctx.statement_citation(link.property_id, link.raw)?;
        refs.push((link.reference, citation));
    }

    Ok(refs)
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
fn push_image_facts(
    facts: &mut Vec<SubmitFact>,
    entity: &WikidataEntity,
    latest: usize,
    ctx: &ItemContext,
) -> Result<usize, BuildError> {
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
            let url = url_for_filename(&CommonsFilename(filename.to_owned()));
            let image = ImageIdx(image_count);
            image_count += 1;

            let citation = ctx.statement_citation(property_id, filename)?;
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
                        value: filename.to_owned(),
                    },
                },
            });
        }
    }
    Ok(image_count)
}

/// The medium and depiction perspective an image property carries: photos
/// (P18/P3451) are exterior pictures, P5775 interiors, P3311 plans are maps with
/// no perspective. `None` for non-image properties.
fn image_medium_perspective(property: &str) -> Option<(ImageMedium, Option<Perspective>)> {
    match property {
        "P18" | "P3451" => Some((ImageMedium::Picture, Some(Perspective::Exterior))),
        "P5775" => Some((ImageMedium::Picture, Some(Perspective::Interior))),
        "P3311" => Some((ImageMedium::Map, None)),
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
            excerpts.push(Excerpt::new(v)?);
        }
        if let Some(v) = bookend_raw_value(&splits[k + 1], BookendPhase::Construction) {
            excerpts.push(Excerpt::new(v)?);
        }
        let excerpts = match NonEmptyVec::try_from_vec(excerpts) {
            Ok(excerpts) => excerpts,
            Err(_) => NonEmptyVec::singleton(Excerpt::new("demolish→rebuild")?),
        };

        let source = ExternalSource::Wikidata {
            entity_id: ctx.entity_id(),
            field: WikidataField::Item,
            revision_id: ctx.revision_id(),
            value: "demolish→rebuild".to_owned(),
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
    /// Entities skipped because their id wasn't a `Q`-item.
    pub skipped: usize,
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
            issues,
        } = rhs;
        self.commits += commits;
        self.entities += entities;
        self.facts += facts;
        self.skipped += skipped;
        self.issues += issues;
    }
}

/// A failure during an ingest pass.
#[derive(Debug)]
pub enum IngestError {
    /// A commit couldn't be built from an entity.
    Build(BuildError),
    /// The store rejected a commit (rendered, since the backend error isn't
    /// `Eq`).
    Submit(String),
}

impl std::fmt::Display for IngestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Build(e) => write!(f, "build commit: {e}"),
            Self::Submit(msg) => write!(f, "submit commit: {msg}"),
        }
    }
}

impl std::error::Error for IngestError {}

impl From<BuildError> for IngestError {
    fn from(e: BuildError) -> Self {
        Self::Build(e)
    }
}

/// Build and submit one commit per entity, tallying as it goes.
pub async fn ingest_entities<S: FactStore>(
    store: &S,
    entities: impl IntoIterator<Item = WikidataEntity>,
    run: &IngesterRunId,
    recorded_at: DateTime<Utc>,
) -> Result<IngestStats, IngestError> {
    let mut stats = IngestStats::default();
    for entity in entities {
        let mut warnings = Vec::new();
        match build_commit(&entity, run, recorded_at, &mut warnings)? {
            Some(commit) => {
                for warning in &warnings {
                    tracing::warn!(qid = entity.id.as_str(), "ingestion: {warning}");
                }
                stats.issues += warnings.len();
                stats.entities += commit.entities.len();
                stats.facts += commit.facts.len();
                commit_facts(store, commit)
                    .await
                    .map_err(|e| IngestError::Submit(format!("{e:?}")))?;
                stats.commits += 1;
            }
            None => stats.skipped += 1,
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
    use chronoscope_core::listing::summaries_in_viewport;
    use chronoscope_core::projection::{member_lineage, project_entity};
    use chronoscope_core::store::FactStore;
    use chronoscope_core::store::memory::{MemoryEntityId, MemoryFactStore, MemoryIds};
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

    fn run_id() -> IngesterRunId {
        IngesterRunId::new("wikidata-test")
    }

    #[test]
    fn build_commit_emits_names_qid_bookend_event_and_image() -> TestResult {
        let commit =
            build_commit::<MemoryIds>(&pantheon()?, &run_id(), fixed_time()?, &mut Vec::new())?
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
                    assertion: FactualAssertion::Existence {
                        fact: existence::Fact { entity, .. }
                    },
                    ..
                } if *entity == EntityIdx(0)
            )),
            "the P571 inception is an existence witness"
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
            &run_id(),
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
            &run_id(),
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
            &run_id(),
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
            &run_id(),
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
        let commit = build_commit(&pantheon()?, &run_id(), recorded, &mut Vec::new())?
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

        // P625 gives a construction location, but no source dates the build —
        // P571 witnesses existence, off the timeline. The Constructed row is
        // present yet undated.
        let construction_start = entity.timeline.events().iter().find_map(|entry| {
            if let typed::EventDetail::Constructed { period, .. } = &entry.detail {
                Some(period.started.possible.earliest())
            } else {
                None
            }
        });
        assert_eq!(
            construction_start,
            Some(None),
            "the construction is a location-only bookend with no start date"
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
        let stats = ingest_entities(&store, [pantheon()?], &run_id(), fixed_time()?).await?;
        assert_eq!(stats.commits, 1);
        assert_eq!(stats.entities, 1);
        assert_eq!(stats.skipped, 0);
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
        let stats = ingest_entities(&store, [entity], &run_id(), fixed_time()?).await?;
        assert_eq!(stats.commits, 1, "the item still commits");
        assert!(
            stats.issues > 0,
            "the unrecognized event QID is tallied as an issue"
        );
        Ok(())
    }

    #[test]
    fn empty_sitelink_title_is_skipped_without_aborting_the_commit() -> TestResult {
        let mut entity = item(
            "Q321",
            BTreeMap::from([label("en", "Placeholder")]),
            BTreeMap::new(),
        )?;
        entity.sitelinks.insert(
            SiteId("commonswiki".to_owned()),
            Sitelink {
                title: String::new(),
            },
        );

        let mut warnings = Vec::new();
        let commit = build_commit::<MemoryIds>(&entity, &run_id(), fixed_time()?, &mut warnings)?
            .ok_or("the item still builds a commit")?;

        // The item's own QID is the only external reference; the empty-title
        // sitelink contributes none.
        let external_refs = commit
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
            .count();
        assert_eq!(external_refs, 1, "only the QID reference survives");
        assert!(
            warnings.iter().any(|w| w.contains("empty title")),
            "the empty sitelink title records a warning, got: {warnings:?}"
        );
        Ok(())
    }

    /// The Pantheon's P625 coordinate, the point the viewport listing pins it at.
    const PANTHEON_LAT: f64 = 41.8986;
    const PANTHEON_LON: f64 = 12.4769;

    /// Commit the Pantheon fixture into a fresh store, returning the store and
    /// the minted entity id so a listing test can query the same snapshot.
    async fn committed_pantheon()
    -> Result<(MemoryFactStore, MemoryEntityId), Box<dyn std::error::Error>> {
        let store = MemoryFactStore::new();
        let commit = build_commit(&pantheon()?, &run_id(), fixed_time()?, &mut Vec::new())?
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

    #[tokio::test]
    async fn viewport_listing_surfaces_pantheon_at_its_coordinate() -> TestResult {
        let (store, id) = committed_pantheon().await?;
        let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;

        // A box around central Rome, comfortably covering the P625 point.
        let viewport = Viewport::new(GeoPoint::new(41.8, 12.4)?, GeoPoint::new(42.0, 12.6)?)?;
        let limit = NonZeroUsize::new(16).ok_or("nonzero limit")?;
        let page = summaries_in_viewport::<MemoryFactStore, _>(&mut view, &viewport, None, limit)
            .await
            .map_err(|e| format!("{e:?}"))?;

        let summary = page
            .summaries
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
        // The P571 inception (1800) witnesses existence and anchors the span's
        // earliest bound; the fire (1900) is a later interior event.
        assert_eq!(
            summary.earliest.map(|d| d.year()),
            Some(1800),
            "the P571 inception anchors the summary's earliest span bound"
        );
        Ok(())
    }

    #[tokio::test]
    async fn viewport_listing_omits_pantheon_from_a_faraway_box() -> TestResult {
        let (store, id) = committed_pantheon().await?;
        let mut view = store.now().await.map_err(|e| format!("{e:?}"))?;

        // A box over the mid-Atlantic — nowhere near Rome.
        let viewport = Viewport::new(GeoPoint::new(0.0, -40.0)?, GeoPoint::new(10.0, -30.0)?)?;
        let limit = NonZeroUsize::new(16).ok_or("nonzero limit")?;
        let page = summaries_in_viewport::<MemoryFactStore, _>(&mut view, &viewport, None, limit)
            .await
            .map_err(|e| format!("{e:?}"))?;

        assert!(
            page.summaries.iter().all(|s| s.id != id),
            "the spatial filter excludes the Pantheon from a box that omits its \
             coordinate: {:?}",
            page.summaries
        );
        Ok(())
    }
}
