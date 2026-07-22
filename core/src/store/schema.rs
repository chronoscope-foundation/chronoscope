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

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
use url::Url;

use crate::geo::{
    GeoPoint, QuadLevel, QuadTileRange, ViewportTilesError, split_level, viewport_tiles,
};
use crate::grammar::citations::{ExternalReference, Language};
use crate::grammar::ids::FactId;

// ============================================================================
// Spatial / temporal query types
// ============================================================================

/// Re-export of [`crate::geo::Viewport`], where the bounding-box type lives. Keeps
/// the `crate::store::schema::Viewport` import path working.
pub use crate::geo::Viewport;
pub use crate::geo::ViewportError;

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
    /// Walk facts whose location index entry intersects `viewport`.
    InViewport(&'a Viewport),
    /// Walk facts whose date index entry intersects `range`.
    InTimeRange(&'a TimeRange),
    /// Walk facts/classes that satisfy both `viewport` and `range`. The
    /// intersection is class-level — see the archive design doc for the
    /// rationale.
    InViewportAndTimeRange {
        viewport: &'a Viewport,
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
    /// Walk facts whose location index entry intersects `viewport`.
    InViewport(&'a Viewport),
    /// Walk facts whose date index entry intersects `range`.
    InTimeRange(&'a TimeRange),
    /// Walk facts/classes that satisfy both `viewport` and `range`.
    InViewportAndTimeRange {
        viewport: &'a Viewport,
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
// Depiction walk
// ============================================================================

/// One page of an entity's depiction walk: raw depiction facts ([`PageItem`])
/// under their depicted-image `SameArtifact` rep, ordered `(image_rep, fact_id)`
/// so an image's facts are contiguous. A page carries whole images — every
/// depiction fact of each image it touches — and [`next_class`](Self::next_class)
/// resumes at the next distinct image, so an image never straddles a page
/// boundary. `Rep` is the image id, `F` the stored-fact payload, `Cur` the
/// backend's class cursor over `Rep`.
#[derive(Debug, Clone, PartialEq)]
pub struct DepictionPage<F, Rep, Cur> {
    pub rows: Vec<PageItem<F, Rep>>,
    /// Resume at the next distinct image, past every fact of this page's last
    /// image. `None` once the walk is exhausted.
    pub next_class: Option<Cur>,
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
// Viewport clustering
// ============================================================================

/// The most tiles one clustering read enumerates at a level — a marker budget
/// far under the shared [`viewport_tiles`] OOM
/// guard. A viewport that trips it asked for a level too fine for its span.
pub const CLUSTER_TILE_CAP: usize = 256;

/// The viewport's clustering tiles at `level`, capped at [`CLUSTER_TILE_CAP`].
/// Both backends decompose a viewport through this, so they enumerate the same
/// tiles under the same cap.
pub fn cluster_tile_ranges(
    viewport: &Viewport,
    level: QuadLevel,
) -> Result<Vec<QuadTileRange>, ViewportTilesError> {
    let tiles = viewport_tiles(viewport, level)?;
    if tiles.len() > CLUSTER_TILE_CAP {
        return Err(ViewportTilesError::TooManyTiles {
            count: u64::try_from(tiles.len()).unwrap_or(u64::MAX),
        });
    }
    Ok(tiles)
}

/// The per-tile candidate bound a clustering read truncates to — by the
/// `(quadkey, fact_id)` order — before retraction and representative
/// resolution. A shared semantic parameter: both backends truncate to the
/// same top-N, so `cluster_entities_in_viewport` returns the same cells.
///
/// The cut is over located facts, so a tile holding at most N of them is
/// exact — the static-architecture corpus today, one location per entity.
/// Past N the lowest `(quadkey, fact_id)` facts win: a distinct entity sorting
/// higher can drop out, misreading a `Cluster` as a `Singleton` or `Colocated`
/// or truncating a `Colocated` group's members, and a tile whose low facts are
/// all retracted can lose its live entities. The cached-entity layer clusters
/// one row per entity and retires the bound.
pub const CLUSTER_TILE_N: usize = 32;

/// How many levels below a container tile the per-tile clustering read
/// (`cluster_tile_cells`) folds its cells. A container `(z, x, y)` folds one
/// cell per non-empty sub-tile at `z + CELL_DEPTH`, so the container carries up
/// to `4^CELL_DEPTH` cells. `saturating` collapses the depth as `z` nears the
/// finest level, so `z = 24` folds the container as a single cell. Tunable —
/// larger trades more cells per fetch for finer client-side declutter.
pub const CELL_DEPTH: u8 = 3;

/// The tuning knob must stay under the fan-out backstop: a `CELL_DEPTH` past
/// [`MAX_CELL_DEPTH`](crate::geo::MAX_CELL_DEPTH) would be silently clamped by
/// [`TileId::child_ranges`](crate::geo::TileId::child_ranges), so the per-tile
/// read would fold fewer levels than configured. Caught at compile time.
const _: () = assert!(CELL_DEPTH <= crate::geo::MAX_CELL_DEPTH);

/// How a clustering read orders candidates within a tile before its top-N cut
/// and representative choice. One variant today: [`Unranked`](Self::Unranked)
/// is ascending `(quadkey, fact_id)`. Future ranks (by recency, by class size)
/// slot in here as the read's ordering key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RankKey {
    /// Ascending `(quadkey, fact_id)` — the tile's lowest-Morton fact wins the
    /// representative.
    Unranked,
}

/// How a tile's surviving entities resolve into one marker.
#[derive(Debug, Clone, PartialEq)]
pub enum CellKind<Id> {
    /// One distinct entity in the tile.
    Singleton,
    /// Two or more distinct entities spread across more than one finest tile —
    /// zooming splits them. `split_level` is the finest level at which the
    /// survivors fall into more than one tile, so a client zooming there sees
    /// the cell subdivide.
    Cluster { split_level: QuadLevel },
    /// Two or more distinct entities sharing one finest (level-24) tile —
    /// unsplittable by zoom. Carries every member for the disambiguation picker.
    Colocated { members: Vec<Id> },
}

/// One clustered map marker: a tile's surviving located entities folded to a
/// single cell. `Id` is the store's entity id kind.
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterCell<Id> {
    /// The `(quadkey, fact_id)`-minimal survivor's entity — the marker's id and
    /// reported position.
    pub representative: Id,
    /// The representative fact's position.
    pub point: GeoPoint,
    /// Whether the cell is one entity, a splittable cluster, or a co-located group.
    pub kind: CellKind<Id>,
}

/// Fold a tile's surviving located facts — each `(quadkey, fact_id,
/// representative, point)` — into one [`ClusterCell`]. The `(quadkey,
/// fact_id)`-minimal survivor supplies the representative and the reported
/// point. The `kind` classifies the tile three ways: one distinct
/// representative is a [`Singleton`](CellKind::Singleton); two or more sharing a
/// single level-24 quadkey are [`Colocated`](CellKind::Colocated), carrying the
/// sorted distinct representatives; otherwise the tile is a
/// [`Cluster`](CellKind::Cluster), carrying the [`split_level`] at which the
/// survivors' quadkeys first fall into separate tiles. Empty input yields no
/// cell. Both backends fold through this, so their cells agree.
///
/// [`split_level`]: crate::geo::split_level
pub fn fold_cluster_cell<Id: Copy + Ord>(
    survivors: &[(i64, FactId, Id, GeoPoint)],
) -> Option<ClusterCell<Id>> {
    let &(_, _, representative, point) = survivors
        .iter()
        .min_by_key(|(quadkey, fid, _, _)| (*quadkey, *fid))?;
    let reps: BTreeSet<Id> = survivors.iter().map(|(_, _, rep, _)| *rep).collect();
    // Extremes of the survivors' quadkeys — duplicates don't move them, so this
    // matches the min/max of the distinct set. A total fold over the non-empty
    // slice, so there's no `Option` to discharge.
    let (min_quadkey, max_quadkey) = survivors
        .iter()
        .fold((i64::MAX, i64::MIN), |(lo, hi), &(q, _, _, _)| {
            (lo.min(q), hi.max(q))
        });
    let kind = if reps.len() < 2 {
        CellKind::Singleton
    } else if min_quadkey == max_quadkey {
        // Every survivor shares one finest tile — a co-located group, not a
        // spread zooming could split.
        CellKind::Colocated {
            members: reps.into_iter().collect(),
        }
    } else {
        CellKind::Cluster {
            split_level: split_level(min_quadkey, max_quadkey),
        }
    };
    Some(ClusterCell {
        representative,
        point,
        kind,
    })
}
