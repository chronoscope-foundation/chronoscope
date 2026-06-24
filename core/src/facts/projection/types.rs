//! The projected value types — the pure read surface a projection builds.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::date::UncertainDate;
use crate::facts::attribute::{EntityRelationType, NameText, NameType};
use crate::facts::citations::{ExternalReference, Language};
use crate::facts::lifecycle::{DurationalKind, PointKind};
use crate::location::UnresolvedLocation;

use super::CitationMap;

/// One name claim. Slots collapse only on an exact `(name, language,
/// name_type)` match; validity bounds ride through verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectedName {
    /// The name text, NFC-canonical.
    pub name: NameText,
    /// The name's language tag, canonical BCP-47.
    pub language: Language,
    /// What kind of name this is.
    pub name_type: NameType,
    /// When the name began applying, when claimed.
    pub valid_from: Option<UncertainDate>,
    /// When the name stopped applying, when claimed.
    pub valid_to: Option<UncertainDate>,
}

/// One directed relationship to another entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectedRelation<EntId> {
    /// The target entity.
    pub to: EntId,
    /// The kind of relationship.
    pub kind: EntityRelationType,
}

/// A bookend phase (construction or demolition) as projected: its endpoint
/// dates joined from every contributing bound, its location joined from every
/// contributing claim. Each field is `None` when no fact spoke to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct ProjectedBookend {
    /// When the phase started, joined across claims.
    pub started_at: Option<UncertainDate>,
    /// When the phase completed, joined across claims.
    pub completed_at: Option<UncertainDate>,
    /// Where the phase took place, joined across claims.
    pub location: Option<UnresolvedLocation>,
}

impl ProjectedBookend {
    pub(super) fn is_empty(&self) -> bool {
        self.started_at.is_none() && self.completed_at.is_none() && self.location.is_none()
    }
}

/// One interior lifetime event, grouped by its event id. The arm mirrors the
/// grammar's category split — a span or an instant — chosen by the event's
/// `HasEvent` kind, which the submit layer requires exactly one of per id.
///
/// `kind` is the set of the event's declared kinds, joined by set union like the
/// date and location lattices. Per event id the submit layer guarantees one
/// `HasEvent`, so the set is a singleton today; `SameEvent`-class aggregation —
/// which would union several members' kinds and surface a disagreement as a
/// >1-element set — is deferred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "category", rename_all = "snake_case")]
pub enum ProjectedLifetimeEvent<EvtId> {
    /// A durational event spanning an interval. Each endpoint joins its own
    /// same-role claims independently, so a span's start and completion stay
    /// distinct.
    Durational {
        /// The event's id.
        event: EvtId,
        /// The durational kinds declared for the event (singleton today).
        kind: BTreeSet<DurationalKind>,
        /// When the event started, joined across `Started` claims.
        started_at: Option<UncertainDate>,
        /// When the event completed, joined across `Completed` claims.
        completed_at: Option<UncertainDate>,
        /// Where a `Moved` event landed, joined across claims.
        location: Option<UnresolvedLocation>,
    },
    /// A point event at a single instant. No location field — only a durational
    /// `MovedToLocation` carries one.
    Point {
        /// The event's id.
        event: EvtId,
        /// The point kinds declared for the event (singleton today).
        kind: BTreeSet<PointKind>,
        /// When the event occurred, joined across claims.
        occurred_at: Option<UncertainDate>,
    },
}

impl<EvtId> ProjectedLifetimeEvent<EvtId> {
    /// The event's id, whichever arm.
    pub(super) fn event(&self) -> &EvtId {
        match self {
            Self::Durational { event, .. } | Self::Point { event, .. } => event,
        }
    }
}

/// A pure projected entity. The lifecycle mirrors the grammar's split:
/// explicit `construction` / `demolition` bookends plus a list of interior
/// events, rather than one flattened timeline.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectedEntity<EntId, EvtId> {
    /// Every distinct name claim.
    pub names: Vec<ProjectedName>,
    /// The construction bookend, when any fact spoke to it.
    pub construction: Option<ProjectedBookend>,
    /// The demolition bookend, when any fact spoke to it.
    pub demolition: Option<ProjectedBookend>,
    /// Interior lifetime events, grouped by id, in a stable order.
    pub events: Vec<ProjectedLifetimeEvent<EvtId>>,
    /// Every distinct directed relationship.
    pub relations: Vec<ProjectedRelation<EntId>>,
    /// Every distinct external reference.
    pub external_references: Vec<ExternalReference>,
}

/// A projected entity paired with its citation sidecar — the entity read
/// surface. Parametric over both the entity id and the image id: the image id
/// rides in through [`ProjectedCitation`](super::ProjectedCitation)'s
/// [`JudgmentSource`](crate::facts::citations::JudgmentSource).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EntityWithCitations<EntId, EvtId, ImgId> {
    /// The projected value.
    pub entity: ProjectedEntity<EntId, EvtId>,
    /// Provenance addressed by [`JsonPath`](super::JsonPath).
    pub citations: CitationMap<ImgId>,
}
