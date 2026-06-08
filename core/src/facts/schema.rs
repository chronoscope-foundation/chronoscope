//! Query types and per-subject stream enums.
//!
//! The fact-store grammar has a closed set of subject kinds (entities,
//! lifetime events, images). Each has one canonical equivalence — `SameEntity`
//! for entities, `SameEvent` for events, `SameArtifact` for images — and
//! entities additionally have one canonical directed-edge relation
//! (`Topological`). These relations are implicit: there's no per-subject
//! relation enum to pass, because there's nothing to choose between. The query
//! types here describe what to walk (the [`EntityStream`] / [`EventStream`] /
//! [`ImageStream`] indices) and what comes back ([`FactPage`] / [`PageItem`],
//! [`EquivClass`], [`EdgeSubgraph`]).
//!
//! ## Subject kinds
//!
//! Three subject kinds, one stream enum per kind:
//!
//! - **Entities** — [`EntityStream`]. Walked over the canonical `SameEntity`
//!   equivalence; the canonical `Topological` edge relation feeds
//!   [`EdgeSubgraph`].
//! - **Lifetime events** — [`EventStream`]. Walked over the canonical
//!   `SameEvent` equivalence. No edge relations today.
//! - **Images** — [`ImageStream`]. Walked over the canonical `SameArtifact`
//!   equivalence. No edge relations today.
//!
//! ## Walk semantics
//!
//! Walks are class-scoped by the subject kind's canonical equivalence: a
//! `walk_*` page carries one row per equivalence class — the introducing fact
//! (minimum `fact_id` per class) — and every row names its class
//! [`representative`](PageItem::representative). There's no grouping choice;
//! dedup follows from the canonical equivalence.
//!
//! ## Backlinks
//!
//! Backlinks ("which facts mention this subject?") are an index, one per
//! subject kind. The lookup lives directly on the per-subject view trait
//! (`all_facts_about_entity`, etc.).

use std::collections::BTreeSet;

use oxilangtag::LanguageTag;

use crate::facts::citations::ExternalReference;
use crate::facts::ids::FactId;

// ============================================================================
// Spatial / temporal query types
// ============================================================================

/// Re-export of [`crate::geo::Bbox`], where the bounding-box type lives. Keeps
/// the `crate::facts::schema::Bbox` import path working.
pub use crate::geo::Bbox;
pub use crate::geo::BboxError;

/// Re-export of [`crate::date::TimeRange`], the primitive interval over
/// [`crate::date::DateBound`]. Keeps the `crate::facts::schema::TimeRange`
/// import path working.
pub use crate::date::TimeRange;
pub use crate::date::TimeRangeError;

// ============================================================================
// Per-subject streams
// ============================================================================

/// Which index to walk for entity-scoped queries. Consumed by the entity view
/// trait's `walk_entities`, which class-scopes the result by the canonical
/// `SameEntity` equivalence.
#[derive(Debug)]
pub enum EntityStream<'a> {
    /// Walk every entity-touching fact in `fact_id` order, subject to
    /// snapshot + retraction scoping.
    All,
    /// Walk facts whose location index entry intersects `bbox`.
    InBbox(&'a Bbox),
    /// Walk facts whose date index entry intersects `range`.
    InTimeRange(&'a TimeRange),
    /// Walk facts/classes that satisfy both `bbox` and `range`. The
    /// intersection is class-level — see the archive design doc for the
    /// rationale.
    InBboxAndTimeRange {
        bbox: &'a Bbox,
        range: &'a TimeRange,
    },
    /// Walk [`super::attribute::Fact::Name`] facts whose name and language
    /// match.
    ByName {
        name: &'a str,
        /// The BCP-47 language tag of the name.
        language: &'a LanguageTag<String>,
    },
    /// Walk [`super::attribute::Fact::ExternalReference`] facts whose reference
    /// equals the supplied one. A SQL backend implements this as an indexed
    /// scan against the externalrefs index.
    ByExternalReference { reference: &'a ExternalReference },
}

/// Which index to walk for event-scoped queries. Consumed by the event view
/// trait's `walk_events`, which class-scopes the result by the canonical
/// `SameEvent` equivalence.
///
/// Events carry only a temporal index today; spatial extent and naming are
/// entity-level concepts. A future event-attribute grammar adding spatial or
/// naming would land new variants here.
#[derive(Debug)]
pub enum EventStream<'a> {
    /// Walk every event-touching fact in `fact_id` order.
    All,
    /// Walk facts whose date index entry intersects `range`.
    InTimeRange(&'a TimeRange),
}

/// Which index to walk for image-scoped queries. Consumed by the image view
/// trait's `walk_images`, which class-scopes the result by the canonical
/// `SameArtifact` equivalence.
///
/// Pictures and maps carry capture date / capture location, so both spatial
/// and temporal filters are meaningful. Naming and external references are
/// entity-level concepts, so the image grammar indexes only date and location.
#[derive(Debug)]
pub enum ImageStream<'a> {
    /// Walk every image-touching fact in `fact_id` order.
    All,
    /// Walk facts whose location index entry intersects `bbox`.
    InBbox(&'a Bbox),
    /// Walk facts whose date index entry intersects `range`.
    InTimeRange(&'a TimeRange),
    /// Walk facts/classes that satisfy both `bbox` and `range`.
    InBboxAndTimeRange {
        bbox: &'a Bbox,
        range: &'a TimeRange,
    },
}

// ============================================================================
// Page result
// ============================================================================

/// One result row from a `walk_*` method. Walks are class-scoped by the subject
/// kind's canonical equivalence, so every row carries the equivalence-class
/// [`representative`](Self::representative) the introducing fact belongs to.
///
/// `F` is the stored-fact payload type (typically
/// [`super::submit::StoredFact<EntId, EvtId, ImgId>`] for some backend's id
/// types). `S` is the subject type (entity / event / image id). Both are
/// ordinary type parameters so `#[derive]` emits the right bounds.
#[derive(Debug, Clone, PartialEq)]
pub struct PageItem<F, S> {
    pub fact_id: FactId,
    pub fact: F,
    /// The canonical equivalence-class representative this row's
    /// introducing fact belongs to.
    pub representative: S,
}

/// A page of walk results. `truncated = true` means more rows exist past
/// this page; the caller should issue the next request with the cursor
/// derived from the last item's `fact_id`.
#[derive(Debug, Clone, PartialEq)]
pub struct FactPage<F, S> {
    pub items: Vec<PageItem<F, S>>,
    /// Whether more pages remain past this one.
    pub truncated: bool,
}

// ============================================================================
// Equivalence class
// ============================================================================

/// The equivalence class of a subject at some snapshot — the canonical
/// representative plus every member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EquivClass<S: Ord> {
    /// The canonical representative of the class.
    pub representative: S,
    /// Every member of the class (including the representative).
    pub members: BTreeSet<S>,
}

// ============================================================================
// Edge subgraph
// ============================================================================

/// A page of a closed-subgraph walk over a directed-edge relation (the
/// canonical `Topological` relation over entities today; events / images have
/// no edge relations).
///
/// Each call returns a partial closure of the connected component reachable
/// from the seed: the subjects visited so far and the edge facts among them.
/// `truncated = true` means more rows remain past this page. The walk cursor is
/// an inclusive lower bound (filters `id >= cursor`), so the caller paginates
/// by passing the id one past the highest [`FactId`] in `edge_facts` as the
/// next `cursor`; passing the highest id itself would re-return that last row
/// and never advance. The caller stops once `truncated = false`.
#[derive(Debug, Clone, PartialEq)]
pub struct EdgeSubgraph<S, F> {
    /// Subjects discovered in the connected component through this page.
    pub subjects: Vec<S>,
    /// Edge facts among `subjects` in this page, ordered by ascending
    /// `FactId`.
    pub edge_facts: Vec<(FactId, F)>,
    /// Whether more pages remain past this one.
    pub truncated: bool,
}
