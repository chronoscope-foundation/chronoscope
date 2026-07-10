//! Query types and per-subject stream enums.
//!
//! The fact-store grammar has a closed set of subject kinds (entities,
//! lifetime events, images). Each has one canonical equivalence — `SameEntity`
//! for entities, `SameEvent` for events, `SameArtifact` for images — and
//! entities additionally have one canonical directed-edge relation
//! (`Topological`). These relations are implicit: there's no per-subject
//! relation enum to pass, because there's nothing to choose between. The query
//! types here describe what to walk (the [`EntityStream`] /
//! [`ImageStream`] indices) and what comes back — [`FactPage`] / [`PageItem`]
//! for the backlink walks, [`ClassPage`] / [`ClassRow`] for the class walks,
//! and [`EquivClass`] for a single subject's class.
//!
//! ## Subject kinds
//!
//! Two stream enums, one per indexed subject kind:
//!
//! - **Entities** — [`EntityStream`]. Walked over the canonical `SameEntity`
//!   equivalence.
//! - **Images** — [`ImageStream`]. Walked over the canonical `SameArtifact`
//!   equivalence. No edge relations today.
//!
//! ## Walk semantics
//!
//! A class `walk_*` pages `(representative, fact_id)` rows ([`ClassRow`]),
//! ordered by `(representative, fact_id)` so a class's rows stay contiguous
//! across page boundaries — a consumer keying by representative sees each
//! class's rows arrive as one run. A fact naming two members of one class
//! contributes one row; the representative comes from the subject kind's
//! canonical equivalence, so there's no grouping choice.
//!
//! ## Backlinks
//!
//! Backlinks ("which facts mention this subject?") are an index, one per
//! subject kind. The lookup lives directly on the per-subject view trait
//! (`all_facts_about_entity`, etc.).

use std::collections::BTreeSet;

use unicode_normalization::UnicodeNormalization;
use url::Url;

use crate::grammar::citations::{ExternalReference, Language};
use crate::grammar::ids::FactId;

// ============================================================================
// Spatial / temporal query types
// ============================================================================

/// Re-export of [`crate::geo::Bbox`], where the bounding-box type lives. Keeps
/// the `crate::store::schema::Bbox` import path working.
pub use crate::geo::Bbox;
pub use crate::geo::BboxError;

/// Re-export of [`crate::date::TimeRange`], the primitive interval over
/// [`crate::date::DateBound`]. Keeps the `crate::store::schema::TimeRange`
/// import path working.
pub use crate::date::TimeRange;
pub use crate::date::TimeRangeError;

// ============================================================================
// Per-subject streams
// ============================================================================

/// Normalize a name for [`EntityStream::ByName`] comparison: trim, Unicode
/// lowercase, then NFC (lowercasing can denormalize, so NFC runs last). The
/// comparison is normalized-exact — both the query key and each stored
/// [`crate::grammar::attribute::Fact::Name`] pass through here, so the matcher's
/// anchor keys and a backend's scan cannot drift.
///
/// Case-folding is a match-recall heuristic, not case correctness — the
/// stored name keeps its own casing. The store carries every language, so a
/// tag that folds imperfectly (Turkish dotted-I, say) only costs a missed
/// candidate, never a wrong or unsound result.
pub fn normalize_name(name: &str) -> String {
    name.trim().to_lowercase().nfc().collect()
}

/// Which index to walk for entity-scoped queries. Consumed by the entity view
/// trait's `walk_entity_classes`, which class-scopes the result by the
/// canonical `SameEntity` equivalence.
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
    /// Walk [`crate::grammar::attribute::Fact::Name`] facts whose name and language
    /// match. Names compare through [`normalize_name`]; the language tag is an
    /// exact compare.
    ByName {
        name: &'a str,
        /// The BCP-47 language tag of the name, in canonical form.
        language: &'a Language,
    },
    /// Walk [`crate::grammar::attribute::Fact::ExternalReference`] facts whose reference
    /// equals the supplied one. A SQL backend implements this as an indexed
    /// scan against the externalrefs index.
    ByExternalReference { reference: &'a ExternalReference },
}

/// Which index to walk for image-scoped queries. Consumed by the image view
/// trait's `walk_image_classes`, which class-scopes the result by the
/// canonical `SameArtifact` equivalence.
///
/// Pictures and maps carry capture date / capture location, so both spatial
/// and temporal filters are meaningful. Naming and external references are
/// entity-level concepts, so the image grammar indexes date, location, and the
/// byte-level source URL.
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
    /// Walk [`crate::grammar::image::Fact::Source`] facts whose source URL equals the
    /// supplied one — an exact value compare, the image matcher's anchor
    /// query. A SQL backend implements this as an indexed scan.
    BySourceUrl { url: &'a Url },
}

// ============================================================================
// Page result
// ============================================================================

/// One result row from a backlink walk (`all_facts_about_*`): a stored fact
/// with the subject it was walked under as its
/// [`representative`](Self::representative).
///
/// `F` is the stored-fact payload type (typically
/// [`StoredFact<R>`](crate::submit::StoredFact) for some backend's id scheme).
/// `S` is the subject type (entity / event / image id). Both are ordinary type
/// parameters so `#[derive]` emits the right bounds.
#[derive(Debug, Clone, PartialEq)]
pub struct PageItem<F, S> {
    pub fact_id: FactId,
    pub fact: F,
    /// The canonical equivalence-class representative this row's
    /// introducing fact belongs to.
    pub representative: S,
}

/// A page of walk results. `next_cursor` is an opaque resume token:
/// `Some(token)` means thread it back as the walk's next `after` and more rows
/// may exist; `None` means the walk is exhausted. A page can carry zero items
/// yet still point at a next cursor, so page size is never a completion signal —
/// only the token is. `Cur` is the store's own cursor type ([`FactStore::Cursor`]);
/// the consumer threads the token back verbatim without inspecting it, so its
/// value and inclusive/exclusive polarity are the walk's own business.
///
/// [`FactStore::Cursor`]: crate::store::FactStore::Cursor
#[derive(Debug, Clone, PartialEq)]
pub struct FactPage<F, S, Cur> {
    pub items: Vec<PageItem<F, S>>,
    /// Opaque resume token: thread back as the walk's next cursor, or `None`
    /// when the walk is exhausted.
    pub next_cursor: Option<Cur>,
}

impl<F, S, Cur> FactPage<F, S, Cur> {
    /// The rows and resume token as a tuple, for `paginate`'s tuple form.
    pub(crate) fn into_parts(self) -> (Vec<PageItem<F, S>>, Option<Cur>) {
        (self.items, self.next_cursor)
    }
}

// ============================================================================
// Class walk
// ============================================================================

/// One fact's membership in a class walk: the equivalence-class representative
/// its subject resolved to, and the fact's id. Carries no fact content — a
/// class walk pages membership, not facts.
///
/// `Rep` is the subject type (entity / event / image id), an ordinary type
/// parameter so `#[derive]` emits the right bounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassRow<Rep> {
    /// The canonical equivalence-class representative this fact fell under.
    pub representative: Rep,
    pub fact_id: FactId,
}

/// A page of class-walk rows, ordered by `(representative, fact_id)`. Two opaque
/// resume tokens page the same walk at different granularities: [`next`](Self::next)
/// steps one row, [`next_class`](Self::next_class) steps one representative. A
/// caller threads whichever it wants back verbatim as the walk's next `after`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassPage<Rep, Cur> {
    pub rows: Vec<ClassRow<Rep>>,
    /// Resume at the next row — more of the last row's representative, or the
    /// first row of the next one. The row cursor the matcher's drain follows.
    /// `None` once the walk is exhausted.
    pub next: Option<Cur>,
    /// Resume at the next representative, skipping the last row's
    /// representative's remaining rows, so a rep-enumerating consumer sees each
    /// representative once. `None` when the last row's representative is the
    /// final class.
    pub next_class: Option<Cur>,
}

impl<Rep, Cur> ClassPage<Rep, Cur> {
    /// The rows and row cursor as a tuple, for `paginate`'s tuple form. The
    /// class cursor is read off [`Self::next_class`] directly.
    pub(crate) fn into_parts(self) -> (Vec<ClassRow<Rep>>, Option<Cur>) {
        (self.rows, self.next)
    }
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
