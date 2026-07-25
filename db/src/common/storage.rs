//! Row ⇄ domain codecs for the fact-store tables.
//!
//! The JSON columns are this backend's own storage encoding, so their shapes
//! live here as mirror types with exhaustive `From` conversions — adding a
//! field or variant to the core type breaks the conversion at compile time
//! instead of silently dropping data. `Cow` fields borrow on encode and own
//! on decode, so one mirror shape serves both directions and encoding never
//! clones the domain value.
//!
//! The facet columns are single-valued projections of a stored fact, pulled
//! out in one place ([`facet_columns`]) so the write path and any future
//! index-backed read agree on what each column means.

use std::borrow::Cow;

use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};

use chronoscope_core::date::UncertainDate;
use chronoscope_core::grammar::assertions::{FactualAssertion, JudgmentAssertion, MetaAssertion};
use chronoscope_core::grammar::citations::{
    ExternalReference, FactualCitation, JudgmentSource, MetaSource,
};
use chronoscope_core::grammar::ids::{CommitId, FactId, SubjectKind};
use chronoscope_core::grammar::lifecycle::DurationalRole;
use chronoscope_core::grammar::{attribute, bookend, depiction, event, identity, image};
use chronoscope_core::location::{Location, UnresolvedLocation};
use chronoscope_core::nonempty::NonEmptyVec;
use chronoscope_core::store::schema::normalize_name;
use chronoscope_core::submit::result::{StoredFactualFact, StoredJudgmentFact, StoredMetaFact};
use chronoscope_core::submit::{
    Decl, EntityIdx, EventIdx, ImageIdx, Resolution, ResolutionOrigin, StoredCommit, StoredFact,
    SubmitResult,
};

use super::convert::u64_to_i64;
use super::error::CodecError;
use super::ids::{SqlEntityId, SqlEventId, SqlIds, SqlImageId};

// ============================================================================
// Subject-kind column tags
// ============================================================================

/// The `fact_subjects.kind` / `facts.edge_kind` column value for a subject
/// kind. One home for the tag strings, shared by inserts and reads.
pub(crate) fn kind_tag(kind: SubjectKind) -> &'static str {
    match kind {
        SubjectKind::Entity => "entity",
        SubjectKind::Event => "event",
        SubjectKind::Image => "image",
    }
}

/// A typed subject id viewed as its storage columns: the kind tag it files
/// under and the raw `i64`. Lets the subject-parametric reads run once,
/// generic over the three id kinds.
pub(crate) trait SubjectColumn: Copy + Ord + Send + Sync {
    const KIND: SubjectKind;
    fn raw(self) -> i64;
    fn from_raw(raw: i64) -> Self;
}

impl SubjectColumn for SqlEntityId {
    const KIND: SubjectKind = SubjectKind::Entity;
    fn raw(self) -> i64 {
        self.0
    }
    fn from_raw(raw: i64) -> Self {
        Self(raw)
    }
}

impl SubjectColumn for SqlEventId {
    const KIND: SubjectKind = SubjectKind::Event;
    fn raw(self) -> i64 {
        self.0
    }
    fn from_raw(raw: i64) -> Self {
        Self(raw)
    }
}

impl SubjectColumn for SqlImageId {
    const KIND: SubjectKind = SubjectKind::Image;
    fn raw(self) -> i64 {
        self.0
    }
    fn from_raw(raw: i64) -> Self {
        Self(raw)
    }
}

// ============================================================================
// fact_json
// ============================================================================

/// Storage form of a [`StoredFact<SqlIds>`] — the `facts.fact_json`
/// column.
#[derive(Serialize, Deserialize)]
#[serde(tag = "category", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum FactJson {
    Factual {
        assertion: FactualAssertion<SqlIds>,
        citation: FactualCitation,
    },
    Judgment {
        assertion: JudgmentAssertion<SqlIds>,
        source: JudgmentSource<SqlImageId>,
    },
    Meta {
        assertion: MetaAssertion,
        source: MetaSource,
    },
}

impl From<StoredFact<SqlIds>> for FactJson {
    fn from(fact: StoredFact<SqlIds>) -> Self {
        match fact {
            StoredFact::Factual(StoredFactualFact {
                assertion,
                citation,
            }) => Self::Factual {
                assertion,
                citation,
            },
            StoredFact::Judgment(StoredJudgmentFact { assertion, source }) => {
                Self::Judgment { assertion, source }
            }
            StoredFact::Meta(StoredMetaFact { assertion, source }) => {
                Self::Meta { assertion, source }
            }
        }
    }
}

impl From<FactJson> for StoredFact<SqlIds> {
    fn from(row: FactJson) -> Self {
        match row {
            FactJson::Factual {
                assertion,
                citation,
            } => Self::Factual(StoredFactualFact {
                assertion,
                citation,
            }),
            FactJson::Judgment { assertion, source } => {
                Self::Judgment(StoredJudgmentFact { assertion, source })
            }
            FactJson::Meta { assertion, source } => {
                Self::Meta(StoredMetaFact { assertion, source })
            }
        }
    }
}

/// Decode a `facts.fact_json` column.
pub(crate) fn fact_from_json(json: &str) -> Result<StoredFact<SqlIds>, CodecError> {
    let row: FactJson =
        serde_json::from_str(json).map_err(super::error::json("decoding fact_json"))?;
    Ok(row.into())
}

/// Encode a fact for the `facts.fact_json` column.
pub(crate) fn fact_to_json(fact: StoredFact<SqlIds>) -> Result<String, CodecError> {
    serde_json::to_string(&FactJson::from(fact)).map_err(super::error::json("encoding fact_json"))
}

// ============================================================================
// commit_json
// ============================================================================

/// Storage form of a commit — the `fact_commits.commit_json` column: only
/// what the other tables can't reconstruct (author, recorded time,
/// declaration lists, fact ids). With `result_json`'s resolution maps and
/// the `facts` rows, the producer-form commit — and so its `CommitId` —
/// stays re-derivable and checkable.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommitJson<'a> {
    author: Cow<'a, chronoscope_core::submit::CommitAuthor>,
    recorded_at: chrono::DateTime<chrono::Utc>,
    entities: Vec<Decl<SqlEntityId>>,
    events: Vec<Decl<SqlEventId>>,
    images: Vec<Decl<SqlImageId>>,
    fact_ids: Cow<'a, [FactId]>,
}

/// One kind's declaration list, recovered from its resolution map: a
/// `DeclaredExisting` origin was a `Decl::Existing`, everything else a
/// `Decl::Local`.
fn decl_list<Idx, Id>(
    map: &std::collections::HashMap<Idx, Resolution<Id>>,
    idx: fn(usize) -> Idx,
    kind: &str,
) -> Result<Vec<Decl<Id>>, CodecError>
where
    Idx: Eq + std::hash::Hash,
    Id: Clone,
{
    (0..map.len())
        .map(|position| {
            let resolution = map
                .get(&idx(position))
                .ok_or_else(|| CodecError::CommitRow {
                    message: format!(
                        "resolution map missing {kind} declaration position {position}"
                    ),
                })?;
            Ok(match &resolution.origin {
                ResolutionOrigin::DeclaredExisting => Decl::Existing {
                    id: resolution.id.clone(),
                },
                _ => Decl::Local,
            })
        })
        .collect()
}

/// Encode a commit record for the `fact_commits.commit_json` column.
pub(crate) fn commit_to_json(
    commit: &StoredCommit,
    result: &SubmitResult<SqlIds>,
) -> Result<String, CodecError> {
    let row = CommitJson {
        author: Cow::Borrowed(&commit.author),
        recorded_at: commit.recorded_at,
        entities: decl_list(&result.entities, EntityIdx, "entity")?,
        events: decl_list(&result.events, EventIdx, "event")?,
        images: decl_list(&result.images, ImageIdx, "image")?,
        fact_ids: Cow::Borrowed(&commit.fact_ids),
    };
    serde_json::to_string(&row).map_err(super::error::json("encoding commit_json"))
}

// ============================================================================
// result_json
// ============================================================================

/// Storage form of a declaration's [`ResolutionOrigin`].
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
// NonEmptyVec's Deserialize wants DeserializeOwned, which the derive's
// inferred `Id: Deserialize<'de>` bound doesn't imply.
#[serde(bound(
    serialize = "Id: ::serde::Serialize",
    deserialize = "Id: ::serde::de::DeserializeOwned"
))]
enum OriginJson<'a, Id: Clone> {
    NewlyMinted,
    DeclaredExisting,
    MatchedExisting {
        matched: Id,
    },
    Ambiguous {
        candidates: Cow<'a, NonEmptyVec<Id>>,
    },
}

impl<'a, Id: Clone> From<&'a ResolutionOrigin<Id>> for OriginJson<'a, Id> {
    fn from(origin: &'a ResolutionOrigin<Id>) -> Self {
        match origin {
            ResolutionOrigin::NewlyMinted => Self::NewlyMinted,
            ResolutionOrigin::DeclaredExisting => Self::DeclaredExisting,
            ResolutionOrigin::MatchedExisting { matched } => Self::MatchedExisting {
                matched: matched.clone(),
            },
            ResolutionOrigin::Ambiguous { candidates } => Self::Ambiguous {
                candidates: Cow::Borrowed(candidates),
            },
        }
    }
}

impl<Id: Clone> From<OriginJson<'_, Id>> for ResolutionOrigin<Id> {
    fn from(origin: OriginJson<'_, Id>) -> Self {
        match origin {
            OriginJson::NewlyMinted => Self::NewlyMinted,
            OriginJson::DeclaredExisting => Self::DeclaredExisting,
            OriginJson::MatchedExisting { matched } => Self::MatchedExisting { matched },
            OriginJson::Ambiguous { candidates } => Self::Ambiguous {
                candidates: candidates.into_owned(),
            },
        }
    }
}

/// Storage form of one declaration's [`Resolution`].
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(bound(
    serialize = "Id: ::serde::Serialize",
    deserialize = "Id: ::serde::de::DeserializeOwned"
))]
struct ResolutionJson<'a, Id: Clone> {
    id: Id,
    origin: OriginJson<'a, Id>,
}

impl<'a, Id: Clone> From<&'a Resolution<Id>> for ResolutionJson<'a, Id> {
    fn from(resolution: &'a Resolution<Id>) -> Self {
        let Resolution { id, origin } = resolution;
        Self {
            id: id.clone(),
            origin: origin.into(),
        }
    }
}

impl<Id: Clone> From<ResolutionJson<'_, Id>> for Resolution<Id> {
    fn from(row: ResolutionJson<'_, Id>) -> Self {
        let ResolutionJson { id, origin } = row;
        Self {
            id,
            origin: origin.into(),
        }
    }
}

/// One kind's resolution map as sorted `(decl position, resolution)` entries
/// — JSON object keys are strings, so the positional maps store as arrays.
fn resolution_entries<Idx, Id>(
    map: &std::collections::HashMap<Idx, Resolution<Id>>,
) -> Vec<(Idx, ResolutionJson<'_, Id>)>
where
    Idx: Ord + Copy,
    Id: Clone,
{
    let mut entries: Vec<(Idx, ResolutionJson<'_, Id>)> = map
        .iter()
        .map(|(idx, resolution)| (*idx, resolution.into()))
        .collect();
    entries.sort_by(|(a, _), (b, _)| a.cmp(b));
    entries
}

/// Storage form of a [`SubmitResult<SqlIds>`] — the
/// `fact_commits.result_json` column.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultJson<'a> {
    commit_id: Cow<'a, CommitId>,
    previously_committed: bool,
    fact_ids: Cow<'a, [FactId]>,
    entities: Vec<(EntityIdx, ResolutionJson<'a, SqlEntityId>)>,
    events: Vec<(EventIdx, ResolutionJson<'a, SqlEventId>)>,
    images: Vec<(ImageIdx, ResolutionJson<'a, SqlImageId>)>,
    companion_commit_id: Option<Cow<'a, CommitId>>,
}

impl<'a> From<&'a SubmitResult<SqlIds>> for ResultJson<'a> {
    fn from(result: &'a SubmitResult<SqlIds>) -> Self {
        let SubmitResult {
            commit_id,
            previously_committed,
            fact_ids,
            entities,
            events,
            images,
            companion_commit_id,
        } = result;
        Self {
            commit_id: Cow::Borrowed(commit_id),
            previously_committed: *previously_committed,
            fact_ids: Cow::Borrowed(fact_ids),
            entities: resolution_entries(entities),
            events: resolution_entries(events),
            images: resolution_entries(images),
            companion_commit_id: companion_commit_id.as_ref().map(Cow::Borrowed),
        }
    }
}

impl From<ResultJson<'_>> for SubmitResult<SqlIds> {
    fn from(row: ResultJson<'_>) -> Self {
        let ResultJson {
            commit_id,
            previously_committed,
            fact_ids,
            entities,
            events,
            images,
            companion_commit_id,
        } = row;
        Self {
            commit_id: commit_id.into_owned(),
            previously_committed,
            fact_ids: fact_ids.into_owned(),
            entities: rebuild(entities),
            events: rebuild(events),
            images: rebuild(images),
            companion_commit_id: companion_commit_id.map(Cow::into_owned),
        }
    }
}

/// Rebuild one kind's positional resolution map from its stored entries.
fn rebuild<Idx, Id>(
    entries: Vec<(Idx, ResolutionJson<'_, Id>)>,
) -> std::collections::HashMap<Idx, Resolution<Id>>
where
    Idx: Eq + std::hash::Hash,
    Id: Clone,
{
    entries
        .into_iter()
        .map(|(idx, resolution)| (idx, resolution.into()))
        .collect()
}

/// Encode a submit result for the `fact_commits.result_json` column.
pub(crate) fn result_to_json(result: &SubmitResult<SqlIds>) -> Result<String, CodecError> {
    serde_json::to_string(&ResultJson::from(result))
        .map_err(super::error::json("encoding result_json"))
}

/// Decode a `fact_commits.result_json` column.
pub(crate) fn result_from_json(json: &str) -> Result<SubmitResult<SqlIds>, CodecError> {
    let row: ResultJson =
        serde_json::from_str(json).map_err(super::error::json("decoding result_json"))?;
    Ok(row.into())
}

// ============================================================================
// Facet columns
// ============================================================================

/// The nullable facet columns of one `facts` row. Every field mirrors a
/// column; [`facet_columns`] is the single extraction site.
#[derive(Default)]
pub(crate) struct Facets {
    pub name_norm: Option<String>,
    pub name_language: Option<String>,
    pub external_ref: Option<String>,
    pub source_url: Option<String>,
    pub date_earliest: Option<String>,
    pub date_latest: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub radius_m: Option<f64>,
    pub edge_kind: Option<&'static str>,
    pub edge_a: Option<i64>,
    pub edge_b: Option<i64>,
    /// A `HasEvent` fact's owning entity — the spatial walk's event→entity
    /// hop reads owners off this column without decoding `fact_json`.
    pub event_owner: Option<i64>,
    pub retracts_fact_id: Option<i64>,
    /// A `RetractCommit` target's hash; staging resolves it to the
    /// surrogate `retracts_commit_seq` column value.
    pub retracts_commit_id: Option<String>,
}

impl Facets {
    fn date(bound: &chronoscope_core::date::UncertainDate) -> Self {
        Self {
            date_earliest: bound.earliest().map(|d| d.to_string()),
            date_latest: bound.latest().map(|d| d.to_string()),
            ..Self::default()
        }
    }

    /// The spatial facet of a location-bearing fact. Only a resolved circle
    /// pins a point; symbolic references and combinators denote regions the
    /// geometry-aware layers own, so they project no columns.
    fn location(location: &UnresolvedLocation) -> Self {
        match location {
            UnresolvedLocation::Resolved(Location::Circle { center, radius }) => Self {
                lat: Some(center.lat()),
                lon: Some(center.lon()),
                radius_m: Some(radius.get()),
                ..Self::default()
            },
            _ => Self::default(),
        }
    }

    fn edge(kind: SubjectKind, a: i64, b: i64) -> Self {
        Self {
            edge_kind: Some(kind_tag(kind)),
            edge_a: Some(a),
            edge_b: Some(b),
            ..Self::default()
        }
    }
}

/// The `external_ref` column key for a reference — its JSON encoding, the
/// same on the write path and any index-backed lookup.
pub(crate) fn external_ref_key(reference: &ExternalReference) -> Result<String, CodecError> {
    serde_json::to_string(reference).map_err(super::error::json("encoding external_ref key"))
}

/// Project a stored fact's single-valued facet columns.
pub(crate) fn facet_columns(fact: &StoredFact<SqlIds>) -> Result<Facets, CodecError> {
    match fact {
        StoredFact::Factual(StoredFactualFact { assertion, .. }) => match assertion {
            FactualAssertion::Attribute { fact } => match fact {
                attribute::Fact::Name { name, language, .. } => Ok(Facets {
                    name_norm: Some(normalize_name(name.as_str())),
                    name_language: Some(language.as_str().to_owned()),
                    ..Facets::default()
                }),
                attribute::Fact::ExternalReference { reference, .. } => Ok(Facets {
                    external_ref: Some(external_ref_key(reference)?),
                    ..Facets::default()
                }),
                attribute::Fact::Relationship { .. } => Ok(Facets::default()),
            },
            FactualAssertion::Construction { fact } => match fact {
                bookend::ConstructionFact::Started { bound, .. }
                | bookend::ConstructionFact::Completed { bound, .. } => Ok(Facets::date(bound)),
                bookend::ConstructionFact::Location { location, .. } => {
                    Ok(Facets::location(location))
                }
            },
            FactualAssertion::Demolition { fact } => match fact {
                bookend::DemolitionFact::Started { bound, .. }
                | bookend::DemolitionFact::Completed { bound, .. } => Ok(Facets::date(bound)),
            },
            FactualAssertion::Existence { fact } => Ok(Facets::date(&fact.at)),
            FactualAssertion::Event { fact } => match fact {
                event::Fact::DurationalDate { bound, .. }
                | event::Fact::PointDate { bound, .. } => Ok(Facets::date(bound)),
                event::Fact::MovedToLocation { location, .. } => Ok(Facets::location(location)),
                event::Fact::HasEvent { entity, .. } => Ok(Facets {
                    event_owner: Some(entity.0),
                    ..Facets::default()
                }),
                event::Fact::DamageCause { .. }
                | event::Fact::MoveMethod { .. }
                | event::Fact::UsageChange { .. }
                | event::Fact::Designation { .. }
                | event::Fact::Description { .. } => Ok(Facets::default()),
            },
            FactualAssertion::Gap { .. } => Ok(Facets::default()),
            FactualAssertion::Image { fact } => match fact {
                image::Fact::Source { url, .. } => Ok(Facets {
                    source_url: Some(url.as_str().to_owned()),
                    ..Facets::default()
                }),
                image::Fact::CreatedDate { bound, .. }
                | image::Fact::CapturedDate { bound, .. }
                | image::Fact::SubjectDate { bound, .. } => Ok(Facets::date(bound)),
                image::Fact::CapturedLocation { location, .. } => Ok(Facets::location(location)),
                image::Fact::Author { .. } | image::Fact::Medium { .. } => Ok(Facets::default()),
            },
        },
        StoredFact::Judgment(StoredJudgmentFact { assertion, .. }) => match assertion {
            JudgmentAssertion::Identity { fact } => match fact {
                identity::Fact::SameEntity { pair } => {
                    Ok(Facets::edge(SubjectKind::Entity, pair.a().0, pair.b().0))
                }
                identity::Fact::SameEvent { pair } => {
                    Ok(Facets::edge(SubjectKind::Event, pair.a().0, pair.b().0))
                }
                identity::Fact::SameArtifact { pair } => {
                    Ok(Facets::edge(SubjectKind::Image, pair.a().0, pair.b().0))
                }
            },
            JudgmentAssertion::Depiction { .. }
            | JudgmentAssertion::Observation { .. }
            | JudgmentAssertion::Composite { .. } => Ok(Facets::default()),
        },
        StoredFact::Meta(StoredMetaFact { assertion, .. }) => match assertion {
            MetaAssertion::RetractFact { target, .. }
            | MetaAssertion::SupersedeFact { target, .. } => Ok(Facets {
                retracts_fact_id: Some(u64_to_i64(target.get(), "retraction target fact id")?),
                ..Facets::default()
            }),
            MetaAssertion::RetractCommit { target, .. } => Ok(Facets {
                retracts_commit_id: Some(target.as_str().to_owned()),
                ..Facets::default()
            }),
        },
    }
}

// The keyed class walks fetch candidates by facet column, then file each
// under the subject the facet belongs to. These extractors are the read-side
// halves of `facet_columns`'s Name / ExternalReference / Source arms — kept
// beside it so a facet's column and its subject stay one pairing.

/// The entity a stored `Name` fact names.
pub(crate) fn named_entity(fact: &StoredFact<SqlIds>) -> Option<SqlEntityId> {
    match fact {
        StoredFact::Factual(StoredFactualFact {
            assertion:
                FactualAssertion::Attribute {
                    fact: attribute::Fact::Name { entity, .. },
                },
            ..
        }) => Some(*entity),
        _ => None,
    }
}

/// The entity a stored `ExternalReference` fact names.
pub(crate) fn referenced_entity(fact: &StoredFact<SqlIds>) -> Option<SqlEntityId> {
    match fact {
        StoredFact::Factual(StoredFactualFact {
            assertion:
                FactualAssertion::Attribute {
                    fact: attribute::Fact::ExternalReference { entity, .. },
                },
            ..
        }) => Some(*entity),
        _ => None,
    }
}

/// The image a stored `Source` fact names.
pub(crate) fn sourced_image(fact: &StoredFact<SqlIds>) -> Option<SqlImageId> {
    match fact {
        StoredFact::Factual(StoredFactualFact {
            assertion:
                FactualAssertion::Image {
                    fact: image::Fact::Source { image, .. },
                },
            ..
        }) => Some(*image),
        _ => None,
    }
}

/// The `(entity, image)` pair a stored `Depiction` judgment links.
pub(crate) fn depiction_subjects(fact: &StoredFact<SqlIds>) -> Option<(SqlEntityId, SqlImageId)> {
    match fact {
        StoredFact::Judgment(StoredJudgmentFact {
            assertion:
                JudgmentAssertion::Depiction {
                    fact: depiction::Fact { entity, image, .. },
                },
            ..
        }) => Some((*entity, *image)),
        _ => None,
    }
}

// ============================================================================
// Temporal-conflict witness rows
// ============================================================================

/// The slot-order key a durational endpoint / point date files under in
/// `event_witness`, mirroring the projection's `[occurred, started, completed]`
/// slot order so a bundled conflict picks the same tie-breaking witness the
/// whole-entity oracle does.
const ROLE_OCCURRED: i64 = 0;
const ROLE_STARTED: i64 = 1;
const ROLE_COMPLETED: i64 = 2;

/// The witness / bookend index row a staged fact contributes, keyed by the
/// fact's *immutable* subject. `None` for facts that seed no temporal index.
pub(crate) enum WitnessRow {
    /// An `Existence` witness, under its entity.
    Existence { member: i64, date: UncertainDate },
    /// An interior-event date witness, under its event; `role` is the
    /// projection slot-order key.
    EventDate {
        event: i64,
        date: UncertainDate,
        role: i64,
    },
    /// A `HasEvent` ownership edge, under its entity.
    HasEvent { member: i64, event: i64 },
    /// A `ConstructionFact::Started` bookend, under its entity.
    ConstructionStart { member: i64, date: UncertainDate },
    /// A `DemolitionFact::Completed` bookend, under its entity.
    DemolitionCompleted { member: i64, date: UncertainDate },
}

/// The temporal-index row a staged fact seeds, if any — the write-side half of
/// the composed read in [`crate::sqlite::read`]. Only the five fact shapes the
/// temporal-conflict read consumes appear here; everything else seeds nothing.
pub(crate) fn witness_row(fact: &StoredFact<SqlIds>) -> Option<WitnessRow> {
    let StoredFact::Factual(StoredFactualFact { assertion, .. }) = fact else {
        return None;
    };
    match assertion {
        FactualAssertion::Existence { fact } => Some(WitnessRow::Existence {
            member: fact.entity.0,
            date: fact.at.clone(),
        }),
        FactualAssertion::Construction {
            fact: bookend::ConstructionFact::Started { entity, bound },
        } => Some(WitnessRow::ConstructionStart {
            member: entity.0,
            date: bound.clone(),
        }),
        FactualAssertion::Demolition {
            fact: bookend::DemolitionFact::Completed { entity, bound },
        } => Some(WitnessRow::DemolitionCompleted {
            member: entity.0,
            date: bound.clone(),
        }),
        FactualAssertion::Event { fact } => match fact {
            event::Fact::HasEvent { entity, event, .. } => Some(WitnessRow::HasEvent {
                member: entity.0,
                event: event.0,
            }),
            event::Fact::PointDate { event, bound } => Some(WitnessRow::EventDate {
                event: event.0,
                date: bound.clone(),
                role: ROLE_OCCURRED,
            }),
            event::Fact::DurationalDate { event, role, bound } => Some(WitnessRow::EventDate {
                event: event.0,
                date: bound.clone(),
                role: match role {
                    DurationalRole::Started => ROLE_STARTED,
                    DurationalRole::Completed => ROLE_COMPLETED,
                },
            }),
            _ => None,
        },
        _ => None,
    }
}

/// The `num_days_from_ce` day number of a date — the sortable integer the
/// witness endpoints store and the read-side scan thresholds bind. A monotonic
/// bijection, so `<` / `>` on it is date order for BCE years and years past
/// 9999 alike, where a date string would not sort. One definition both the
/// stored endpoint and the scan threshold share, so the two can never drift.
pub(crate) fn day_number(date: NaiveDate) -> i64 {
    i64::from(date.num_days_from_ce())
}

/// The full `UncertainDate` JSON a witness index row stores. Bookend rows, read
/// whole rather than value-scanned, store only this.
pub(crate) fn witness_date_json(date: &UncertainDate) -> Result<String, CodecError> {
    serde_json::to_string(date).map_err(super::error::json("encoding witness date"))
}

/// The sortable endpoint days and JSON a value-scanned witness index row stores
/// for a date: `(earliest, latest)` as [`day_number`]s, each `None` for an open
/// side, plus the full `UncertainDate` JSON.
pub(crate) fn witness_date_columns(
    date: &UncertainDate,
) -> Result<(Option<i64>, Option<i64>, String), CodecError> {
    Ok((
        date.earliest().map(day_number),
        date.latest().map(day_number),
        witness_date_json(date)?,
    ))
}

/// The distinct `(kind tag, subject id)` pairs a fact mentions — its
/// `fact_subjects` rows. A fact naming one id twice contributes one row.
pub(crate) fn subject_rows(fact: &StoredFact<SqlIds>) -> Vec<(&'static str, i64)> {
    let mut entities: Vec<i64> = Vec::new();
    let mut events: Vec<i64> = Vec::new();
    let mut images: Vec<i64> = Vec::new();
    fact.for_each_id(
        &mut |e: &SqlEntityId| entities.push(e.0),
        &mut |v: &SqlEventId| events.push(v.0),
        &mut |i: &SqlImageId| images.push(i.0),
    );
    let mut subjects: std::collections::BTreeSet<(&'static str, i64)> =
        std::collections::BTreeSet::new();
    subjects.extend(
        entities
            .into_iter()
            .map(|id| (kind_tag(SubjectKind::Entity), id)),
    );
    subjects.extend(
        events
            .into_iter()
            .map(|id| (kind_tag(SubjectKind::Event), id)),
    );
    subjects.extend(
        images
            .into_iter()
            .map(|id| (kind_tag(SubjectKind::Image), id)),
    );
    subjects.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};

    use chronoscope_core::grammar::ids::UserId;
    use chronoscope_core::submit::CommitAuthor;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// `commit_json` stores the minimal commit form and decodes back to it:
    /// author, recorded time, declaration lists (recovered from the
    /// resolution origins), and fact ids. Drift between the encode and
    /// decode directions breaks here.
    #[test]
    fn commit_json_round_trips_minimal_commit_form() -> TestResult {
        let commit = StoredCommit {
            commit_id: CommitId::parse("0f".repeat(32))?,
            author: CommitAuthor::User(UserId::new("user-7")?),
            recorded_at: DateTime::<Utc>::UNIX_EPOCH,
            fact_ids: vec![FactId::new(0), FactId::new(7)],
        };
        // Decl 0 was Existing, decl 1 minted fresh.
        let result = SubmitResult::<SqlIds> {
            commit_id: commit.commit_id.clone(),
            previously_committed: false,
            fact_ids: commit.fact_ids.clone(),
            entities: [
                (
                    EntityIdx(0),
                    Resolution {
                        id: SqlEntityId(3),
                        origin: ResolutionOrigin::DeclaredExisting,
                    },
                ),
                (
                    EntityIdx(1),
                    Resolution {
                        id: SqlEntityId(4),
                        origin: ResolutionOrigin::NewlyMinted,
                    },
                ),
            ]
            .into_iter()
            .collect(),
            events: std::collections::HashMap::new(),
            images: std::collections::HashMap::new(),
            companion_commit_id: None,
        };
        let json = commit_to_json(&commit, &result)?;
        let decoded: CommitJson = serde_json::from_str(&json)?;
        assert_eq!(decoded.author.as_ref(), &commit.author);
        assert_eq!(decoded.recorded_at, commit.recorded_at);
        assert_eq!(decoded.fact_ids.as_ref(), commit.fact_ids.as_slice());
        assert_eq!(
            decoded.entities,
            vec![Decl::Existing { id: SqlEntityId(3) }, Decl::Local,]
        );
        assert!(decoded.events.is_empty());
        assert!(decoded.images.is_empty());
        Ok(())
    }
}
