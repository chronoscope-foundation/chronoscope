//! Fact-store commit construction from Wikidata entities.
//!
//! Reshapes the reusable Wikidata parsing layer (`parsing`, `handlers`,
//! `lifecycle`, `usage`) into `submit::Commit`s: one commit per Wikidata item,
//! declarations minted local, facts referencing them by bundle-local index.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};

use chronoscope_core::facts::assertions::{FactualAssertion, JudgmentAssertion};
use chronoscope_core::facts::attribute::{self, EntityRelationType, NameText, NameType};
use chronoscope_core::facts::bookend;
use chronoscope_core::facts::citations::{
    CitationError, Excerpt, ExcerptError, ExternalReference, ExternalSource, FactualCitation,
    JudgmentSource, Language, LanguageError, WikidataField,
};
use chronoscope_core::facts::depiction::{self, Perspective};
use chronoscope_core::facts::event;
use chronoscope_core::facts::ids::IngesterRunId;
use chronoscope_core::facts::image::{self, ImageMedium};
use chronoscope_core::facts::lifecycle::{
    DamageCause, DurationalKind, DurationalRole, LifetimeEventKind, MoveMethod, PointKind, Usage,
};
use chronoscope_core::facts::memory::{MemoryFactStore, MemoryIds};
use chronoscope_core::facts::submit::{
    Commit, CommitAuthor, Decl, EntityIdx, EventIdx, ImageIdx, SubmitFact, commit_facts,
};
use chronoscope_core::{
    Cited, DamageCause as OldDamageCause, EntityName, EntityTransition, Evidence,
    MoveMethod as OldMoveMethod, NameType as OldNameType, UncertainDate, Usage as OldUsage,
    WikidataEntityId, WikidataField as OldWikidataField, WikidataPropertyId,
};
use chronoscope_integrations::wikidata::{CommonsFilename, WikidataEntity, url_for_filename};

use crate::SourceIdx;
use crate::wikidata::handlers::PROPERTY_HANDLERS;
use crate::wikidata::ingest::PropertyContext;
use crate::wikidata::lifecycle::build_lifecycles;
use crate::wikidata::parsing::{extract_names, parse_sitelink};
use crate::wikidata::usage;

/// A failure while turning a Wikidata entity into a commit.
///
/// Distinct from "skip this entity" (a non-Q id), which is `Ok(None)`: these are
/// malformed inputs the boundary rejects.
#[derive(Debug)]
pub enum BuildError {
    /// A language tag failed BCP-47 canonicalization.
    Language(LanguageError),
    /// A citation excerpt was empty or over-length.
    Excerpt(ExcerptError),
    /// A factual citation had no excerpts.
    Citation(CitationError),
    /// A `Replaces` relationship's endpoints collapsed to one entity.
    SelfRelationship,
    /// An evidence variant the Wikidata parsing layer never emits reached the
    /// citation mapper.
    UnsupportedEvidence,
    /// A cited value reached the citation mapper with no evidence — every value
    /// the parsing layer emits is expected to carry a source.
    MissingEvidence,
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Language(e) => write!(f, "language tag: {e}"),
            Self::Excerpt(e) => write!(f, "citation excerpt: {e}"),
            Self::Citation(e) => write!(f, "citation: {e}"),
            Self::SelfRelationship => {
                write!(f, "replaces relationship collapsed to a self-loop")
            }
            Self::UnsupportedEvidence => {
                write!(f, "evidence source is not a Wikidata claim")
            }
            Self::MissingEvidence => write!(f, "cited value carried no evidence"),
        }
    }
}

impl std::error::Error for BuildError {}

impl From<LanguageError> for BuildError {
    fn from(e: LanguageError) -> Self {
        Self::Language(e)
    }
}

impl From<ExcerptError> for BuildError {
    fn from(e: ExcerptError) -> Self {
        Self::Excerpt(e)
    }
}

impl From<CitationError> for BuildError {
    fn from(e: CitationError) -> Self {
        Self::Citation(e)
    }
}

/// Per-item context threaded through fact construction: the parsed id, its
/// revision, and the fallback "came from this item" citation.
struct Ctx {
    entity_id: WikidataEntityId,
    revision_id: u64,
    item_citation: FactualCitation,
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
pub fn build_commit(
    entity: &WikidataEntity,
    run: &IngesterRunId,
    recorded_at: DateTime<Utc>,
) -> Result<Option<Commit<MemoryIds>>, BuildError> {
    let Ok(entity_id) = WikidataEntityId::parse(entity.id.as_str()) else {
        return Ok(None);
    };
    let revision_id = entity.lastrevid.0;
    let item_citation = wikidata_citation(
        entity_id,
        revision_id,
        WikidataField::Item,
        entity_id.to_string(),
    )?;
    let ctx = Ctx {
        entity_id,
        revision_id,
        item_citation,
    };

    let names = extract_names(entity, entity.id.as_str(), revision_id);

    // P793 is the significant-event context; the top-level date/location
    // properties cite their own property per extraction.
    let lifecycle_ctx =
        PropertyContext::with_property(entity_id, revision_id, WikidataPropertyId::new(793));
    let (mut splits, _warnings) = build_lifecycles(&entity.claims, &lifecycle_ctx);
    if splits.is_empty() {
        splits.push(Vec::new());
    }
    let n_splits = splits.len();
    let latest = n_splits - 1;

    let inferred = usage::infer(entity);
    let refs = collect_external_references(entity, &ctx)?;

    let mut facts: Vec<SubmitFact> = Vec::new();
    push_name_facts(&mut facts, &names, n_splits)?;
    push_reference_facts(&mut facts, &refs, n_splits);
    let event_count = push_lifecycle_facts(&mut facts, &splits, &inferred, &ctx)?;
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
// Names — on every split
// ============================================================================

fn push_name_facts(
    facts: &mut Vec<SubmitFact>,
    names: &[Cited<EntityName, SourceIdx>],
    n_splits: usize,
) -> Result<(), BuildError> {
    for cited in names {
        let citation = citation_of(cited)?;
        let language = Language::new(cited.value.language.as_str())?;
        let name_type = map_name_type(cited.value.name_type);
        for k in 0..n_splits {
            facts.push(attribute_fact(
                attribute::Fact::Name {
                    entity: EntityIdx(k),
                    name: NameText::new(&cited.value.name),
                    language: language.clone(),
                    name_type,
                    valid_from: None,
                    valid_to: None,
                },
                citation.clone(),
            ));
        }
    }
    Ok(())
}

// ============================================================================
// External references — on every split
// ============================================================================

/// Every external reference the item carries: its own QID, its sitelinks, and
/// its handler-produced links (OSM, Pleiades, URLs). Each is paired with the
/// citation naming where it was read from. Emitted on every split — a lookup by
/// external ref returns the whole demolish→rebuild set, not one split.
fn collect_external_references(
    entity: &WikidataEntity,
    ctx: &Ctx,
) -> Result<Vec<(ExternalReference, FactualCitation)>, BuildError> {
    let mut refs = Vec::new();

    refs.push((
        ExternalReference::Wikidata { qid: ctx.entity_id },
        ctx.item_citation.clone(),
    ));

    for (site, sitelink) in &entity.sitelinks {
        if let Some(link) = parse_sitelink(site.as_str(), sitelink.title.as_str()) {
            let url = link.to_url();
            refs.push((
                ExternalReference::from_url(&url),
                wikidata_citation(
                    ctx.entity_id,
                    ctx.revision_id,
                    WikidataField::Sitelink {
                        site: site.as_str().to_owned(),
                    },
                    url.to_string(),
                )?,
            ));
        }
    }

    for (property, claims) in &entity.claims {
        let prop = property.as_str();
        if image_medium_perspective(prop).is_some() {
            continue;
        }
        let Some(handler) = PROPERTY_HANDLERS.get(prop) else {
            continue;
        };
        let Ok(property_id) = WikidataPropertyId::parse(prop) else {
            continue;
        };
        let pctx = PropertyContext::with_property(ctx.entity_id, ctx.revision_id, property_id);
        let Ok(output) = handler(claims.as_slice(), &pctx) else {
            continue;
        };
        for link in &output.links {
            let url = link.to_url();
            refs.push((
                ExternalReference::from_url(&url),
                wikidata_citation(
                    ctx.entity_id,
                    ctx.revision_id,
                    WikidataField::Statement { property_id },
                    url.to_string(),
                )?,
            ));
        }
    }

    Ok(refs)
}

fn push_reference_facts(
    facts: &mut Vec<SubmitFact>,
    refs: &[(ExternalReference, FactualCitation)],
    n_splits: usize,
) {
    for (reference, citation) in refs {
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
}

// ============================================================================
// Bookends + interior events — per split
// ============================================================================

/// Emit each split's bookend and interior-event facts, minting one `EventIdx`
/// per interior transition. Returns the total interior-event count so the caller
/// sizes the event declaration list.
fn push_lifecycle_facts(
    facts: &mut Vec<SubmitFact>,
    splits: &[Vec<EntityTransition<SourceIdx>>],
    inferred: &BTreeSet<OldUsage>,
    ctx: &Ctx,
) -> Result<usize, BuildError> {
    let mut event_count = 0usize;
    for (k, transitions) in splits.iter().enumerate() {
        let entity = EntityIdx(k);
        for transition in transitions {
            match transition {
                EntityTransition::Constructed {
                    started_at,
                    completed_at,
                    location,
                    ..
                } => {
                    push_bookend_dates(facts, entity, started_at, completed_at, false)?;
                    if let Some(c) = location {
                        facts.push(construction_fact(
                            bookend::ConstructionFact::Location {
                                entity,
                                location: c.value.clone(),
                            },
                            citation_of(c)?,
                        ));
                    }
                }
                EntityTransition::Demolished {
                    started_at,
                    completed_at,
                    ..
                } => {
                    push_bookend_dates(facts, entity, started_at, completed_at, true)?;
                }
                EntityTransition::Modified {
                    started_at,
                    completed_at,
                    ..
                } => {
                    let event = EventIdx(event_count);
                    event_count += 1;
                    facts.push(event_fact(
                        event::Fact::HasEvent {
                            entity,
                            event,
                            kind: durational(DurationalKind::Modified),
                        },
                        event_citation(transition, ctx)?,
                    ));
                    push_durational_dates(facts, event, started_at, completed_at)?;
                }
                EntityTransition::Repaired {
                    started_at,
                    completed_at,
                    ..
                } => {
                    let event = EventIdx(event_count);
                    event_count += 1;
                    facts.push(event_fact(
                        event::Fact::HasEvent {
                            entity,
                            event,
                            kind: durational(DurationalKind::Repaired),
                        },
                        event_citation(transition, ctx)?,
                    ));
                    push_durational_dates(facts, event, started_at, completed_at)?;
                }
                EntityTransition::Damaged {
                    occurred_at, cause, ..
                } => {
                    let event = EventIdx(event_count);
                    event_count += 1;
                    let cite = event_citation(transition, ctx)?;
                    facts.push(event_fact(
                        event::Fact::HasEvent {
                            entity,
                            event,
                            kind: durational(DurationalKind::Damaged),
                        },
                        cite.clone(),
                    ));
                    push_durational_dates(facts, event, occurred_at, &None)?;
                    if let Some(cause) = cause {
                        facts.push(event_fact(
                            event::Fact::DamageCause {
                                event,
                                cause: map_damage_cause(cause),
                            },
                            cite,
                        ));
                    }
                }
                EntityTransition::Moved {
                    occurred_at,
                    location,
                    method,
                    ..
                } => {
                    let event = EventIdx(event_count);
                    event_count += 1;
                    let cite = event_citation(transition, ctx)?;
                    facts.push(event_fact(
                        event::Fact::HasEvent {
                            entity,
                            event,
                            kind: durational(DurationalKind::Moved),
                        },
                        cite.clone(),
                    ));
                    push_durational_dates(facts, event, occurred_at, &None)?;
                    if let Some(loc) = location {
                        facts.push(event_fact(
                            event::Fact::MovedToLocation {
                                event,
                                location: loc.value.clone(),
                            },
                            citation_of(loc)?,
                        ));
                    }
                    if let Some(method) = method {
                        facts.push(event_fact(
                            event::Fact::MoveMethod {
                                event,
                                method: map_move_method(method),
                            },
                            cite,
                        ));
                    }
                }
                EntityTransition::UsageModified {
                    occurred_at,
                    new_usages,
                    ..
                } => {
                    let event = EventIdx(event_count);
                    event_count += 1;
                    let cite = event_citation(transition, ctx)?;
                    facts.push(event_fact(
                        event::Fact::HasEvent {
                            entity,
                            event,
                            kind: point(PointKind::UsageChanged),
                        },
                        cite.clone(),
                    ));
                    if let Some(c) = occurred_at {
                        facts.push(event_fact(
                            event::Fact::PointDate {
                                event,
                                bound: c.value.clone(),
                            },
                            citation_of(c)?,
                        ));
                    }
                    facts.push(event_fact(
                        event::Fact::UsageChange {
                            event,
                            new_usages: resolve_usages(new_usages, inferred),
                        },
                        cite,
                    ));
                }
                EntityTransition::Designated {
                    occurred_at,
                    designation,
                    ..
                } => {
                    let event = EventIdx(event_count);
                    event_count += 1;
                    let cite = event_citation(transition, ctx)?;
                    facts.push(event_fact(
                        event::Fact::HasEvent {
                            entity,
                            event,
                            kind: point(PointKind::Designated),
                        },
                        cite.clone(),
                    ));
                    if let Some(c) = occurred_at {
                        facts.push(event_fact(
                            event::Fact::PointDate {
                                event,
                                bound: c.value.clone(),
                            },
                            citation_of(c)?,
                        ));
                    }
                    facts.push(event_fact(
                        event::Fact::Designation {
                            event,
                            designation: designation.clone(),
                        },
                        cite,
                    ));
                }
            }
        }
    }
    Ok(event_count)
}

/// A bookend's start/completion date facts. `demolition` picks the outer variant
/// (`Demolition` never carries a location, so only its dates route here).
fn push_bookend_dates(
    facts: &mut Vec<SubmitFact>,
    entity: EntityIdx,
    started_at: &Option<Cited<UncertainDate, SourceIdx>>,
    completed_at: &Option<Cited<UncertainDate, SourceIdx>>,
    demolition: bool,
) -> Result<(), BuildError> {
    if let Some(c) = started_at {
        let bound = c.value.clone();
        let citation = citation_of(c)?;
        facts.push(if demolition {
            demolition_fact(bookend::DemolitionFact::Started { entity, bound }, citation)
        } else {
            construction_fact(
                bookend::ConstructionFact::Started { entity, bound },
                citation,
            )
        });
    }
    if let Some(c) = completed_at {
        let bound = c.value.clone();
        let citation = citation_of(c)?;
        facts.push(if demolition {
            demolition_fact(
                bookend::DemolitionFact::Completed { entity, bound },
                citation,
            )
        } else {
            construction_fact(
                bookend::ConstructionFact::Completed { entity, bound },
                citation,
            )
        });
    }
    Ok(())
}

/// A durational event's start/completion `DurationalDate` facts.
fn push_durational_dates(
    facts: &mut Vec<SubmitFact>,
    event: EventIdx,
    started_at: &Option<Cited<UncertainDate, SourceIdx>>,
    completed_at: &Option<Cited<UncertainDate, SourceIdx>>,
) -> Result<(), BuildError> {
    if let Some(c) = started_at {
        facts.push(event_fact(
            event::Fact::DurationalDate {
                event,
                role: DurationalRole::Started,
                bound: c.value.clone(),
            },
            citation_of(c)?,
        ));
    }
    if let Some(c) = completed_at {
        facts.push(event_fact(
            event::Fact::DurationalDate {
                event,
                role: DurationalRole::Completed,
                bound: c.value.clone(),
            },
            citation_of(c)?,
        ));
    }
    Ok(())
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
    ctx: &Ctx,
) -> Result<usize, BuildError> {
    let mut image_count = 0usize;
    for (property, claims) in &entity.claims {
        let Some((medium, perspective)) = image_medium_perspective(property.as_str()) else {
            continue;
        };
        let Ok(property_id) = WikidataPropertyId::parse(property.as_str()) else {
            continue;
        };
        for claim in claims {
            if claim.mainsnak.is_special() {
                continue;
            }
            let Some(filename) = claim.mainsnak.string_value() else {
                continue;
            };
            let url = url_for_filename(&CommonsFilename(filename.to_owned()));
            let image = ImageIdx(image_count);
            image_count += 1;

            let citation = wikidata_citation(
                ctx.entity_id,
                ctx.revision_id,
                WikidataField::Statement { property_id },
                filename.to_owned(),
            )?;
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
                        entity_id: ctx.entity_id,
                        field: WikidataField::Statement { property_id },
                        revision_id: ctx.revision_id,
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
    splits: &[Vec<EntityTransition<SourceIdx>>],
    ctx: &Ctx,
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
        if excerpts.is_empty() {
            excerpts.push(Excerpt::new("demolish→rebuild")?);
        }

        let source = ExternalSource::Wikidata {
            entity_id: ctx.entity_id,
            field: WikidataField::Item,
            revision_id: ctx.revision_id,
            value: "demolish→rebuild".to_owned(),
        };
        facts.push(attribute_fact(
            fact,
            FactualCitation::new(source, excerpts)?,
        ));
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
/// date, else its start.
fn bookend_raw_value(split: &[EntityTransition<SourceIdx>], phase: BookendPhase) -> Option<String> {
    for t in split {
        match (phase, t) {
            (
                BookendPhase::Construction,
                EntityTransition::Constructed {
                    started_at,
                    completed_at,
                    ..
                },
            )
            | (
                BookendPhase::Demolition,
                EntityTransition::Demolished {
                    started_at,
                    completed_at,
                    ..
                },
            ) => return raw_value(completed_at.as_ref().or(started_at.as_ref())),
            _ => {}
        }
    }
    None
}

// ============================================================================
// Citation mapping (old Evidence -> new FactualCitation)
// ============================================================================

/// The citation backing a cited value, from its first evidence. Every value the
/// Wikidata parsing layer emits is cited, so an empty evidence list is a boundary
/// violation, not a fallback case.
fn citation_of<T>(cited: &Cited<T, SourceIdx>) -> Result<FactualCitation, BuildError> {
    let evidence = cited.evidence.first().ok_or(BuildError::MissingEvidence)?;
    factual_citation(evidence)
}

/// The citation for an interior event: its primary date's evidence, or the item
/// citation when the transition carries no date.
fn event_citation(
    transition: &EntityTransition<SourceIdx>,
    ctx: &Ctx,
) -> Result<FactualCitation, BuildError> {
    let (start, end) = transition.date_range();
    match start.or(end) {
        Some(cited) => citation_of(cited),
        // The sole item-wide fallback: a dateless interior event carries no
        // per-fact evidence, so the item citation is its most precise source.
        None => Ok(ctx.item_citation.clone()),
    }
}

/// Map one old-grammar [`Evidence`] to a [`FactualCitation`]. The Wikidata
/// parsing layer only ever emits `Wikidata` evidence; `Web` maps to a URL
/// source, and the two in-system variants are rejected rather than faked.
fn factual_citation(evidence: &Evidence<SourceIdx>) -> Result<FactualCitation, BuildError> {
    match evidence {
        Evidence::Wikidata {
            entity_id,
            revision_id,
            field,
            observed_value,
        } => wikidata_citation(
            *entity_id,
            *revision_id,
            map_wikidata_field(field)?,
            observed_value.clone(),
        ),
        Evidence::Web {
            source_url,
            excerpt,
        } => {
            let text = excerpt
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| source_url.to_string());
            let source = ExternalSource::Url {
                url: source_url.clone(),
                published: None,
            };
            Ok(FactualCitation::new(source, vec![Excerpt::new(text)?])?)
        }
        Evidence::Source { .. } | Evidence::Dbpedia { .. } => Err(BuildError::UnsupportedEvidence),
    }
}

/// A Wikidata-sourced factual citation with the observed value as its excerpt.
fn wikidata_citation(
    entity_id: WikidataEntityId,
    revision_id: u64,
    field: WikidataField,
    value: String,
) -> Result<FactualCitation, BuildError> {
    let excerpt = Excerpt::new(value.clone())?;
    let source = ExternalSource::Wikidata {
        entity_id,
        field,
        revision_id,
        value,
    };
    Ok(FactualCitation::new(source, vec![excerpt])?)
}

fn map_wikidata_field(field: &OldWikidataField) -> Result<WikidataField, BuildError> {
    match field {
        OldWikidataField::Statement { property_id } => Ok(WikidataField::Statement {
            property_id: *property_id,
        }),
        OldWikidataField::Label { language } => Ok(WikidataField::Label {
            language: Language::new(language.as_str())?,
        }),
    }
}

/// The raw observed value behind a date's first evidence, for the `Replaces`
/// excerpt.
fn raw_value(cited: Option<&Cited<UncertainDate, SourceIdx>>) -> Option<String> {
    cited?.evidence.first().and_then(evidence_raw_value)
}

fn evidence_raw_value(ev: &Evidence<SourceIdx>) -> Option<String> {
    match ev {
        Evidence::Wikidata { observed_value, .. } => Some(observed_value.clone()),
        Evidence::Web {
            excerpt,
            source_url,
        } => excerpt.clone().or_else(|| Some(source_url.to_string())),
        Evidence::Dbpedia { .. } | Evidence::Source { .. } => None,
    }
}

// ============================================================================
// SubmitFact constructors + enum mappings
// ============================================================================

fn attribute_fact(fact: attribute::Fact<EntityIdx>, citation: FactualCitation) -> SubmitFact {
    SubmitFact::Factual {
        assertion: FactualAssertion::Attribute { fact },
        citation,
    }
}

fn construction_fact(
    fact: bookend::ConstructionFact<EntityIdx>,
    citation: FactualCitation,
) -> SubmitFact {
    SubmitFact::Factual {
        assertion: FactualAssertion::Construction { fact },
        citation,
    }
}

fn demolition_fact(
    fact: bookend::DemolitionFact<EntityIdx>,
    citation: FactualCitation,
) -> SubmitFact {
    SubmitFact::Factual {
        assertion: FactualAssertion::Demolition { fact },
        citation,
    }
}

fn event_fact(fact: event::Fact<EntityIdx, EventIdx>, citation: FactualCitation) -> SubmitFact {
    SubmitFact::Factual {
        assertion: FactualAssertion::Event { fact },
        citation,
    }
}

fn image_fact(fact: image::Fact<ImageIdx>, citation: FactualCitation) -> SubmitFact {
    SubmitFact::Factual {
        assertion: FactualAssertion::Image { fact },
        citation,
    }
}

fn durational(kind: DurationalKind) -> LifetimeEventKind {
    LifetimeEventKind::Durational { kind }
}

fn point(kind: PointKind) -> LifetimeEventKind {
    LifetimeEventKind::Point { kind }
}

fn map_name_type(old: OldNameType) -> NameType {
    match old {
        OldNameType::Official => NameType::Official,
        OldNameType::Common => NameType::Common,
        OldNameType::Historical => NameType::Historical,
    }
}

fn map_damage_cause(old: &OldDamageCause) -> DamageCause {
    match old {
        OldDamageCause::Earthquake => DamageCause::Earthquake,
        OldDamageCause::Fire => DamageCause::Fire,
        OldDamageCause::Flood => DamageCause::Flood,
        OldDamageCause::Neglect => DamageCause::Neglect,
        OldDamageCause::Structural => DamageCause::Structural,
        OldDamageCause::Vandalism => DamageCause::Vandalism,
        OldDamageCause::War => DamageCause::War,
        OldDamageCause::Weather => DamageCause::Weather,
        OldDamageCause::Other { description } => DamageCause::Other {
            description: description.clone(),
        },
    }
}

fn map_move_method(old: &OldMoveMethod) -> MoveMethod {
    match old {
        OldMoveMethod::Disassembled => MoveMethod::Disassembled,
        OldMoveMethod::Whole => MoveMethod::Whole,
    }
}

fn map_usage(old: &OldUsage) -> Usage {
    match old {
        OldUsage::Unknown => Usage::Unknown,
        OldUsage::Agricultural => Usage::Agricultural,
        OldUsage::Commercial => Usage::Commercial,
        OldUsage::Cultural => Usage::Cultural,
        OldUsage::Educational => Usage::Educational,
        OldUsage::Healthcare => Usage::Healthcare,
        OldUsage::Industrial => Usage::Industrial,
        OldUsage::Infrastructure => Usage::Infrastructure,
        OldUsage::Institutional => Usage::Institutional,
        OldUsage::Military => Usage::Military,
        OldUsage::Recreational => Usage::Recreational,
        OldUsage::Religious => Usage::Religious,
        OldUsage::Residential => Usage::Residential,
        OldUsage::Transportation => Usage::Transportation,
        OldUsage::Other { description } => Usage::Other {
            description: description.clone(),
        },
    }
}

/// The post-event usage set, applying the ingester's back-fill: a placeholder
/// `{Unknown}` (opening/service events carry no explicit use) is replaced by the
/// usage inferred from P366/P31.
fn resolve_usages(
    new_usages: &BTreeSet<OldUsage>,
    inferred: &BTreeSet<OldUsage>,
) -> BTreeSet<Usage> {
    let source = if new_usages.len() == 1 && new_usages.contains(&OldUsage::Unknown) {
        inferred
    } else {
        new_usages
    };
    source.iter().map(map_usage).collect()
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
pub async fn ingest_entities(
    store: &MemoryFactStore,
    entities: impl IntoIterator<Item = WikidataEntity>,
    run: &IngesterRunId,
    recorded_at: DateTime<Utc>,
) -> Result<IngestStats, IngestError> {
    let mut stats = IngestStats::default();
    for entity in entities {
        match build_commit(&entity, run, recorded_at)? {
            Some(commit) => {
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

    use std::collections::BTreeMap;
    use std::num::NonZeroUsize;

    use chrono::{Datelike, TimeZone};

    use chronoscope_core::facts::listing::summaries_in_bbox;
    use chronoscope_core::facts::memory::MemoryEntityId;
    use chronoscope_core::facts::projection::{member_lineage, project_entity};
    use chronoscope_core::facts::store::FactStore;
    use chronoscope_core::facts::typed;
    use chronoscope_core::geo::{Bbox, GeoPoint};
    use chronoscope_integrations::wikidata::{
        Claim, CoordinateValue, DataValue, EntityRefValue, Label, LanguageCode, PropertyId, Rank,
        RevisionId, Snak, TimeValue, WikidataEntityType, WikidataId, WikidataPrecision,
        WikidataTimestamp,
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
        Ok(WikidataEntity {
            id: WikidataId::try_from("Q1234".to_owned())?,
            entity_type: WikidataEntityType::Item,
            lastrevid: RevisionId(100),
            labels,
            claims,
            sitelinks: BTreeMap::new(),
        })
    }

    fn run_id() -> IngesterRunId {
        IngesterRunId::new("wikidata-test")
    }

    #[test]
    fn build_commit_emits_names_qid_bookend_event_and_image() -> TestResult {
        let commit = build_commit(&pantheon()?, &run_id(), fixed_time()?)?
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
                                    ..
                                },
                        },
                    ..
                } = f
                {
                    *entity == EntityIdx(0) && name.as_str() == text && language.as_str() == lang
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
                        fact: bookend::ConstructionFact::Completed { entity, .. }
                    },
                    ..
                } if *entity == EntityIdx(0)
            )),
            "the P571 inception is a construction-completion bookend"
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

    #[tokio::test]
    async fn round_trip_projects_names_refs_and_timeline() -> TestResult {
        let store = MemoryFactStore::new();
        let recorded = fixed_time()?;
        let commit = build_commit(&pantheon()?, &run_id(), recorded)?.ok_or("expected commit")?;
        let result = commit_facts(&store, commit)
            .await
            .map_err(|e| format!("{e:?}"))?;
        let id = result
            .entities
            .get(&EntityIdx(0))
            .ok_or("entity 0 resolved")?
            .id;

        let view = store.now().await.map_err(|e| format!("{e:?}"))?;
        let (class, projected) =
            project_entity::<MemoryFactStore, _, _>(&view, id, member_lineage).await?;
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

        let construction_year = entity.timeline.iter().find_map(|entry| {
            if let typed::EventDetail::Constructed { period, .. } = &entry.detail {
                period.completed.possible.earliest()
            } else {
                None
            }
        });
        assert_eq!(
            construction_year.map(|d| d.year()),
            Some(1800),
            "the P571 inception survives as a construction date on the timeline"
        );

        assert!(
            entity.timeline.iter().any(|entry| matches!(
                &entry.detail,
                typed::EventDetail::Interior {
                    kind: typed::InteriorEvent::Damaged { .. },
                    ..
                }
            )),
            "the fire reads back as a Damaged timeline entry"
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
        assert!(stats.facts > 0, "the commit carries facts");
        Ok(())
    }

    /// The Pantheon's P625 coordinate, the point the bbox listing pins it at.
    const PANTHEON_LAT: f64 = 41.8986;
    const PANTHEON_LON: f64 = 12.4769;

    /// Commit the Pantheon fixture into a fresh store, returning the store and
    /// the minted entity id so a listing test can query the same snapshot.
    async fn committed_pantheon()
    -> Result<(MemoryFactStore, MemoryEntityId), Box<dyn std::error::Error>> {
        let store = MemoryFactStore::new();
        let commit =
            build_commit(&pantheon()?, &run_id(), fixed_time()?)?.ok_or("expected commit")?;
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
    async fn bbox_listing_surfaces_pantheon_at_its_coordinate() -> TestResult {
        let (store, id) = committed_pantheon().await?;
        let view = store.now().await.map_err(|e| format!("{e:?}"))?;

        // A box around central Rome, comfortably covering the P625 point.
        let bbox = Bbox::new(GeoPoint::new(41.8, 12.4)?, GeoPoint::new(42.0, 12.6)?)?;
        let limit = NonZeroUsize::new(16).ok_or("nonzero limit")?;
        let page = summaries_in_bbox::<MemoryFactStore, _>(&view, &bbox, None, limit)
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
        assert_eq!(
            summary.earliest.map(|d| d.year()),
            Some(1800),
            "the P571 inception bounds the summary's timeline span"
        );
        Ok(())
    }

    #[tokio::test]
    async fn bbox_listing_omits_pantheon_from_a_faraway_box() -> TestResult {
        let (store, id) = committed_pantheon().await?;
        let view = store.now().await.map_err(|e| format!("{e:?}"))?;

        // A box over the mid-Atlantic — nowhere near Rome.
        let bbox = Bbox::new(GeoPoint::new(0.0, -40.0)?, GeoPoint::new(10.0, -30.0)?)?;
        let limit = NonZeroUsize::new(16).ok_or("nonzero limit")?;
        let page = summaries_in_bbox::<MemoryFactStore, _>(&view, &bbox, None, limit)
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
