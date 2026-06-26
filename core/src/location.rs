//! Location types with uncertainty support.
//!
//! Three types model locations at different stages of resolution:
//!
//! - [`Location`] — resolved geometry (circles, unions, the empty location, the
//!   unbounded one). A bounded join lattice: `Empty` (⊥) and `Unbounded` (⊤)
//!   bracket it, `one_of` is the join, and its
//!   [`JoinSemilattice`](crate::lattice::JoinSemilattice) impl folds a stream
//!   into a canonical union. The constructors canonicalize — flattening nested
//!   unions and dropping a member a sibling subsumes.
//! - [`UnresolvedLocation`] — may contain symbolic [`LocationReference`]s that
//!   need external resolution (geocoding, OSM lookup, etc.). It carries the same
//!   join one level up via [`UnresolvedLocation::one_of`] and its own
//!   [`JoinSemilattice`](crate::lattice::JoinSemilattice) impl.
//! - [`LocationReference`] — a symbolic pointer to a location in an external
//!   system (OSM, OHM, named place, address, or "near" any of these).
//!
//! Resolution maps `LocationReference` → `Location` via a caller-provided
//! closure.
//!
//! # Entity vs. region scope
//!
//! Entity-scale things — individual buildings, the Forbidden City as a complex,
//! a fortress, a citadel — are first-class entities with their own
//! [`crate::facts::ids::EntityId`]. Containment between them is expressed by
//! [`crate::facts::attribute::Fact::Relationship`] with
//! [`crate::facts::attribute::EntityRelationType::Contains`], not by a location
//! reference.
//!
//! Region-scale things — cities, neighborhoods, contested geographical areas
//! like "Manhattan" or "Newark" — are not entities in the fact-store grammar.
//! They live as opaque names inside [`LocationReference::NamedPlace`], resolved
//! against an external gazetteer at projection time. The grammar doesn't
//! enforce this split structurally; it's a convention the submission and
//! projection layers both follow.

use std::cmp::Ordering;
use std::fmt;

use chronoscope_macros::grammar_type;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::geo::{GeoPoint, GeoPointError, Meters, SphereCap, SpherePoint};
use crate::ids::{OhmId, OsmElementType, OsmId};

/// Sanity bound on a circle's uncertainty radius: a circle wider than this
/// almost certainly signals a unit slip or a bad resolution, not a real claim.
/// Set well above any real coordinate uncertainty and under a hemisphere, the
/// radius below which [`Location::denotes_empty`]'s witness lemma holds — that
/// lemma extracts an extreme point from a cap-intersection, which exists only
/// for geodesically-convex (sub-hemisphere) caps.
pub const MAX_UNCERTAINTY_RADIUS: Meters = Meters(5_000_000.0);

/// Errors from location construction or validation.
///
/// Coordinate validation (range / finiteness / negative-zero normalization)
/// lives on [`GeoPoint`]; a circle's center error surfaces through the
/// [`Self::Center`] wrapper. `LocationError`'s own variants are the
/// location-specific checks: the radius bounds and the minimum-entry count for
/// `OneOf`/`AllOf`.
///
/// `PartialEq` only — the radius variants carry pre-validation `f64` that may be
/// NaN/Inf. Errors aren't part of the content-addressed-fact graph, so missing
/// `Eq`/`Ord` doesn't ripple.
#[derive(Debug, Clone, PartialEq)]
pub enum LocationError {
    /// `OneOf` / `AllOf` requires at least 2 entries.
    TooFewEntries { count: usize },
    /// The circle's center coordinate failed [`GeoPoint`] validation.
    Center(GeoPointError),
    /// Radius was negative.
    NegativeRadius { radius: f64 },
    /// Radius was `NaN` or infinite.
    NonFiniteRadius { radius: f64 },
    /// Radius exceeded [`MAX_UNCERTAINTY_RADIUS`].
    RadiusTooLarge { radius: f64 },
}

impl fmt::Display for LocationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooFewEntries { count } => {
                write!(f, "OneOf/AllOf requires at least 2 entries, got {count}")
            }
            Self::Center(e) => write!(f, "circle center: {e}"),
            Self::NegativeRadius { radius } => {
                write!(f, "radius must be non-negative, got {radius}")
            }
            Self::NonFiniteRadius { radius } => {
                write!(f, "radius must be finite, got {radius}")
            }
            Self::RadiusTooLarge { radius } => write!(
                f,
                "radius {radius} exceeds the {} m sanity bound",
                MAX_UNCERTAINTY_RADIUS.0
            ),
        }
    }
}

impl std::error::Error for LocationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Center(e) => Some(e),
            _ => None,
        }
    }
}

impl From<GeoPointError> for LocationError {
    fn from(e: GeoPointError) -> Self {
        Self::Center(e)
    }
}

/// Both [`Location`] and [`UnresolvedLocation`] carry the same two combinators —
/// a disjunction (`OneOf`) and a conjunction (`AllOf`) — over a list of members.
/// [`Members`] is the one sealed wrapper they share: a `Vec` reachable only
/// through [`Members::canonicalize`], which takes the per-level canonicalizer as
/// an argument. A raw `OneOf`/`AllOf` therefore cannot be assembled outside a
/// canonicalizing path, so every stored combinator is canonical. The type is
/// `pub` (read-only externally via [`Members::as_slice`]); only the inner `Vec`
/// is sealed.
///
/// The canonicalizers themselves stay per-level — the resolved ones do geometric
/// containment-collapse, the unresolved ones don't (references are opaque) — and
/// live in the parallel [`canonical::resolved`] / [`canonical::unresolved`]
/// submodules so the "same two combinators at two levels" reads side by side.
///
/// `Members` serializes transparently as the bare inner `Vec`, so the wire shape
/// and `JsonSchema` surface match a plain list; deserialization routes through
/// the canonicalizers via the host enums' hand-written `Deserialize`.
mod canonical {
    use schemars::JsonSchema;
    use serde::Serialize;

    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
    #[serde(transparent)]
    #[schemars(transparent)]
    pub struct Members<T>(Vec<T>);

    impl<T> Members<T> {
        /// Seal a member list, canonicalizing it through `f` first. The
        /// canonicalizer is the sole entry point — there is no constructor that
        /// skips it — so the sealed `Vec` is always in canonical form.
        pub(super) fn canonicalize(items: Vec<T>, f: impl FnOnce(Vec<T>) -> Vec<T>) -> Self {
            Self(f(items))
        }

        /// The canonical members, in sorted order.
        pub fn as_slice(&self) -> &[T] {
            &self.0
        }

        /// The number of members.
        pub fn len(&self) -> usize {
            self.0.len()
        }

        /// Whether the member list is empty. A valid `OneOf`/`AllOf` always
        /// holds ≥2; only ⊥/⊤-shaped inputs canonicalize down to empty.
        pub fn is_empty(&self) -> bool {
            self.0.is_empty()
        }
    }
}

pub use canonical::Members;

// ==================== Resolved Location ====================

/// Resolved location geometry.
///
/// No external references — purely geometric. A bounded join lattice:
/// [`Empty`](Self::Empty) (⊥) and [`Unbounded`](Self::Unbounded) (⊤) bracket it,
/// [`one_of`](Self::one_of) joins by union-with-subsumption, and the
/// [`JoinSemilattice`](crate::lattice::JoinSemilattice) impl folds a stream to
/// the canonical join.
///
/// `Eq`/`Ord`/`Hash` are hand-implemented because the `Circle` variant carries
/// an [`Meters`] radius wrapping an `f64` (needed for `BTreeSet<SubmitFact>`
/// dedup of facts that transitively reach `Location`). The center's coordinate
/// handling is [`GeoPoint`]'s; these impls delegate the center to it and reach
/// the radius `.0` for `total_cmp`/`to_bits`. The smart constructor
/// [`Location::circle`] rejects NaN/Inf radii and normalizes `-0.0` so the
/// manual `Hash`/`Ord` stay consistent with the derived `PartialEq`.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Location {
    /// The empty location (⊥): no place at all. The bottom of the lattice — it
    /// drops out of a union and is contained by every other location. Reached
    /// only as a join seed or a degenerate read-side artifact; the submit
    /// boundary forbids storing it, the same way it forbids storing an empty
    /// date interval.
    Empty,
    /// A point with radius of uncertainty (a spherical cap on the earth's
    /// surface).
    Circle { center: GeoPoint, radius: Meters },
    /// Disjoint or partially-overlapping shapes: "one of these is true."
    /// Parallels [`UnresolvedLocation::OneOf`] at the resolved level.
    /// Flattened when nested. Requires ≥2 members.
    ///
    /// Construct via [`Location::one_of`]; `members` is sealed canonical.
    OneOf { members: Members<Location> },
    /// The shared region of several shapes: "all of these hold at once." The
    /// meet dual of [`OneOf`](Self::OneOf). Flattened when nested. Requires
    /// ≥2 members.
    ///
    /// Construct via [`Location::all_of`]; `members` is sealed canonical.
    AllOf { members: Members<Location> },
    /// No geometric information (resolution failed, or unknown). The top of the
    /// lattice (⊤): it contains every other location and absorbs a union.
    Unbounded,
}

impl Eq for Location {}

impl PartialOrd for Location {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Location {
    fn cmp(&self, other: &Self) -> Ordering {
        // Variant discriminant first, then payload. `Circle`'s f64s use
        // `total_cmp` (NaN/Inf rejected at construction).
        fn variant_index(loc: &Location) -> u8 {
            match loc {
                Location::Empty => 0,
                Location::Circle { .. } => 1,
                Location::OneOf { .. } => 2,
                Location::AllOf { .. } => 3,
                Location::Unbounded => 4,
            }
        }
        let self_idx = variant_index(self);
        let other_idx = variant_index(other);
        if self_idx != other_idx {
            return self_idx.cmp(&other_idx);
        }
        match (self, other) {
            (Self::Empty, Self::Empty) => Ordering::Equal,
            (
                Self::Circle {
                    center: c1,
                    radius: r1,
                },
                Self::Circle {
                    center: c2,
                    radius: r2,
                },
                // `center` orders via `GeoPoint`'s `total_cmp`-based `Ord`; the
                // radius `f64` uses `total_cmp` for the same NaN-free reason.
            ) => c1.cmp(c2).then_with(|| r1.0.total_cmp(&r2.0)),
            (Self::OneOf { members: a }, Self::OneOf { members: b }) => a.cmp(b),
            (Self::AllOf { members: a }, Self::AllOf { members: b }) => a.cmp(b),
            (Self::Unbounded, Self::Unbounded) => Ordering::Equal,
            // Mixed variants are handled by the discriminant check above; these
            // arms are unreachable but spelled out so adding a variant forces a
            // decision.
            (Self::Empty, _)
            | (Self::Circle { .. }, _)
            | (Self::OneOf { .. }, _)
            | (Self::AllOf { .. }, _)
            | (Self::Unbounded, _) => Ordering::Equal,
        }
    }
}

impl std::hash::Hash for Location {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Self::Circle { center, radius } => {
                // `center` hashes via `GeoPoint`'s `to_bits`-based `Hash`;
                // the radius hashes by bits for the same reason.
                center.hash(state);
                radius.0.to_bits().hash(state);
            }
            Self::OneOf { members } => members.hash(state),
            Self::AllOf { members } => members.hash(state),
            Self::Empty | Self::Unbounded => {}
        }
    }
}

impl<'de> Deserialize<'de> for Location {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        #[serde(deny_unknown_fields)]
        enum Raw {
            Empty,
            Circle { center: GeoPoint, radius: Meters },
            OneOf { members: Vec<Location> },
            AllOf { members: Vec<Location> },
            Unbounded,
        }

        let raw = Raw::deserialize(deserializer)?;
        match raw {
            Raw::Empty => Ok(Location::Empty),
            // `center` is validated by `GeoPoint`'s own `Deserialize`;
            // `circle` adds the radius checks.
            Raw::Circle { center, radius } => {
                Location::circle(center, radius).map_err(serde::de::Error::custom)
            }
            Raw::OneOf { members } => Location::one_of(members).map_err(serde::de::Error::custom),
            Raw::AllOf { members } => Location::all_of(members).map_err(serde::de::Error::custom),
            Raw::Unbounded => Ok(Location::Unbounded),
        }
    }
}

impl Location {
    /// Create a validated `Circle` location from an already-validated
    /// [`GeoPoint`] center and a radius.
    ///
    /// The center carries [`GeoPoint`]'s guarantees (in-range, finite,
    /// negative-zero-normalized). This constructor adds the radius checks:
    /// finite, non-negative, and within [`MAX_UNCERTAINTY_RADIUS`]. `-0.0`
    /// radius is normalized to `+0.0` so the manual `Hash`/`Ord` (bit-level /
    /// `total_cmp`) stay consistent with the derived `PartialEq`.
    pub fn circle(center: GeoPoint, radius: Meters) -> Result<Self, LocationError> {
        if !radius.0.is_finite() {
            return Err(LocationError::NonFiniteRadius { radius: radius.0 });
        }
        if radius.0 < 0.0 {
            return Err(LocationError::NegativeRadius { radius: radius.0 });
        }
        if radius.0 > MAX_UNCERTAINTY_RADIUS.0 {
            return Err(LocationError::RadiusTooLarge { radius: radius.0 });
        }
        // Normalize `-0.0` to `+0.0`: `-0.0 + 0.0 == +0.0`, and adding
        // `0.0` is a no-op for every other finite value.
        let radius = Meters(radius.0 + 0.0);
        Ok(Self::Circle { center, radius })
    }

    /// Create a zero-radius `Circle` at a [`GeoPoint`] — a point taken at
    /// face value, with no explicit precision.
    ///
    /// Infallible: the center is already validated and a `0.0` radius
    /// always passes the radius checks. The lattice treats this as a
    /// zero-radius circle for geometric operations.
    pub fn point(center: GeoPoint) -> Self {
        Self::Circle {
            center,
            radius: Meters(0.0),
        }
    }

    /// Create a validated, canonical `OneOf`.
    ///
    /// Canonicalizes the members so one logical union has a single wire form:
    /// nested `OneOf`s flatten, a member geometrically contained by a sibling
    /// drops as redundant, and the survivors sort and dedup. The `≥2` minimum
    /// is checked on the canonical result, so a union that collapses to one
    /// member (all-equal, or one subsuming the rest) is rejected.
    pub fn one_of(children: Vec<Location>) -> Result<Self, LocationError> {
        let members = Members::canonicalize(children, resolved::canonical_one_of);
        if members.len() < 2 {
            return Err(LocationError::TooFewEntries {
                count: members.len(),
            });
        }
        Ok(Self::OneOf { members })
    }

    /// Create a validated, canonical `AllOf` — the meet dual of
    /// [`one_of`](Self::one_of).
    ///
    /// Canonicalizes the members so one logical intersection has a single wire
    /// form: nested `AllOf`s flatten, `Unbounded` (⊤) members drop, an `Empty`
    /// (⊥) member collapses the whole meet, a member that contains a distinct
    /// sibling drops as redundant, and the survivors sort and dedup. The `≥2`
    /// minimum is checked on the canonical result, so an intersection that
    /// collapses to one member (all-equal, or one contained by the rest) is
    /// rejected.
    pub fn all_of(children: Vec<Location>) -> Result<Self, LocationError> {
        let members = Members::canonicalize(children, resolved::canonical_all_of);
        if members.len() < 2 {
            return Err(LocationError::TooFewEntries {
                count: members.len(),
            });
        }
        Ok(Self::AllOf { members })
    }

    /// Check if this location's region contains another's entirely.
    ///
    /// Private — drives the union canonicalization's redundant-member collapse
    /// (a member contained by a sibling carries no information).
    fn contains(&self, other: &Self) -> bool {
        match (self, other) {
            // Unbounded contains everything
            (Self::Unbounded, _) => true,
            // Nothing (except Unbounded) contains Unbounded
            (_, Self::Unbounded) => false,
            // Everything contains Empty (⊥); Empty contains nothing but itself,
            // already settled by the line above.
            (_, Self::Empty) => true,
            // Empty contains nothing but Empty, handled above.
            (Self::Empty, _) => false,
            // Circle containment: `self`'s cap swallows `other`'s.
            (
                Self::Circle {
                    center: c1,
                    radius: r1,
                },
                Self::Circle {
                    center: c2,
                    radius: r2,
                },
            ) => SphereCap::new((*c1).into(), *r1).contains(&SphereCap::new((*c2).into(), *r2)),
            // An intersection sits below each of its members, so self contains
            // it once self contains any one member. Checked before the OneOf
            // arms so an `AllOf` object never falls into them.
            (_, Self::AllOf { members }) => members.as_slice().iter().any(|m| self.contains(m)),
            // An intersection contains other only when every member does. The
            // earlier arm has already settled an `AllOf` other.
            (Self::AllOf { members }, _) => members.as_slice().iter().all(|m| m.contains(other)),
            // OneOf vs OneOf: self contains other if every child of other
            // is contained by some child of self.
            (
                Self::OneOf {
                    members: self_children,
                },
                Self::OneOf {
                    members: other_children,
                },
            ) => other_children
                .as_slice()
                .iter()
                .all(|oc| self_children.as_slice().iter().any(|sc| sc.contains(oc))),
            // OneOf contains X if any child contains X
            (Self::OneOf { members }, other_loc) => {
                members.as_slice().iter().any(|c| c.contains(other_loc))
            }
            // X contains OneOf if X contains every child
            (_, Self::OneOf { members }) => members.as_slice().iter().all(|c| self.contains(c)), // Circle doesn't contain Unbounded (handled above)
        }
    }

    /// Whether `point` lies in the region this location denotes. The set-level
    /// semantics behind the lattice: `Circle` is a closed spherical cap (the rim
    /// counts), `OneOf` is the union, `AllOf` the intersection, `Empty` the
    /// empty set, `Unbounded` the whole sphere.
    pub fn covers(&self, point: &GeoPoint) -> bool {
        self.covers_point(&(*point).into())
    }

    /// Whether `pt` (already a unit vector) lies in the region. Recurses the
    /// location directly: a `Circle` rebuilds its [`SphereCap`] inline and tests
    /// membership, a `OneOf` holds if any child does, an `AllOf` if every child
    /// does, `Empty` never, `Unbounded` always.
    fn covers_point(&self, pt: &SpherePoint) -> bool {
        match self {
            Self::Empty => false,
            Self::Unbounded => true,
            Self::Circle { center, radius } => SphereCap::new((*center).into(), *radius).covers(pt),
            Self::OneOf { members } => members.as_slice().iter().any(|m| m.covers_point(pt)),
            Self::AllOf { members } => {
                // Constructors enforce ≥2 members; an empty `AllOf` (the empty
                // intersection = whole sphere) would wrongly read as covered via
                // `all()` over no members.
                debug_assert!(!members.is_empty(), "AllOf holds ≥2 members");
                members.as_slice().iter().all(|m| m.covers_point(pt))
            }
        }
    }

    /// Push every [`SphereCap`] in this subtree into `out` — the candidate caps a
    /// containing `AllOf` draws its witness points from.
    fn gather_caps(&self, out: &mut Vec<SphereCap>) {
        match self {
            Self::Empty | Self::Unbounded => {}
            Self::Circle { center, radius } => out.push(SphereCap::new((*center).into(), *radius)),
            Self::OneOf { members } | Self::AllOf { members } => {
                members.as_slice().iter().for_each(|m| m.gather_caps(out));
            }
        }
    }

    /// Visit every `Circle` reachable in the expression, in tree order — the
    /// same enumeration [`Self::gather_caps`] performs to build caps. The test
    /// sampling oracle reuses this walk so its circle set can't drift from the
    /// routine's.
    #[cfg(test)]
    fn for_each_circle(&self, f: &mut impl FnMut(&GeoPoint, Meters)) {
        match self {
            Self::Empty | Self::Unbounded => {}
            Self::Circle { center, radius } => f(center, *radius),
            Self::OneOf { members } => {
                members.as_slice().iter().for_each(|m| m.for_each_circle(f));
            }
            Self::AllOf { members } => {
                members.as_slice().iter().for_each(|m| m.for_each_circle(f));
            }
        }
    }

    /// Whether the region this location denotes is the empty set.
    ///
    /// Structural recursion over the location. `Empty` is empty; `Unbounded` and
    /// any `Circle` (a zero radius still covers its center) are non-empty. A
    /// `OneOf` is empty iff every branch is — a union vanishes only when all its
    /// parts do. The interesting case is an `AllOf` of disjoint shapes, which
    /// canonicalization keeps rather than collapse: a meet of two far-apart
    /// circles denotes nothing even though neither member does.
    ///
    /// # Method
    ///
    /// Candidate-point coverage, polynomial in the cap count, no DNF. For an
    /// `AllOf`, gather the caps in this node's subtree and test each cap center
    /// and every pairwise rim crossing against this whole `AllOf`; the
    /// intersection is non-empty iff some candidate is covered.
    ///
    /// Correctness. An `AllOf`'s region is a finite union of geodesically-convex
    /// pieces, each an intersection of closed caps (distributing its inner unions
    /// out yields a DNF whose terms are cap-intersections; we never materialize
    /// it). For caps no larger than a hemisphere, a non-empty closed convex
    /// cap-intersection contains an extreme point of itself, and the extreme
    /// points of a cap-intersection are cap centers and pairwise rim crossings —
    /// a tangency is a single feasible point kept by the inclusive cap
    /// membership. Each piece's caps are a subset of this node's gathered caps,
    /// so its witness sits among the node's candidates. Hence some candidate is
    /// covered exactly when this `AllOf` is non-empty. Scoping candidate
    /// generation to each `AllOf` node still captures a cross-branch witness like
    /// the `A∩C` crossing in `(A∪B)∩(C∪D)`: the node's subtree gathers `A` and
    /// `C` both.
    ///
    /// The geometry is exact on the unit sphere — seam- and pole-free, with no
    /// projection and no radius-scale assumption.
    pub fn denotes_empty(&self) -> bool {
        match self {
            Self::Empty => true,
            Self::Unbounded | Self::Circle { .. } => false,
            Self::OneOf { members } => members.as_slice().iter().all(Self::denotes_empty),
            Self::AllOf { members } => {
                // Constructors enforce ≥2 members; an empty `AllOf` (the empty
                // intersection = whole sphere) gathers no candidates and would
                // wrongly read as empty.
                debug_assert!(!members.is_empty(), "AllOf holds ≥2 members");
                let mut caps: Vec<SphereCap> = Vec::new();
                self.gather_caps(&mut caps);
                let mut candidates: Vec<SpherePoint> = caps.iter().map(SphereCap::center).collect();
                for i in 0..caps.len() {
                    for j in (i + 1)..caps.len() {
                        candidates.extend(caps[i].boundary_intersections(&caps[j]));
                    }
                }
                !candidates.iter().any(|pt| self.covers_point(pt))
            }
        }
    }
}

/// Whether a resolved or unresolved location's geometry is decidably empty,
/// still pending external resolution, or known consistent.
///
/// Defined here as `denotes_empty`'s first consumer; downstream layers that
/// classify a location's resolution status can share it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictStatus {
    /// Fully resolved and denotes a non-empty region. `Consistent` implies fully
    /// resolved — `conflict_status` returns `Pending` while any reference
    /// remains.
    Consistent,
    /// The most-permissive resolution is already empty — no resolution rescues
    /// it.
    Conflict,
    /// An unresolved reference remains; resolution could still empty it.
    Pending,
}

/// The join half of the resolved location lattice. ⊥ is [`Empty`](Location::Empty);
/// the join unions and canonicalizes, so `Unbounded` (⊤) absorbs the union and
/// `Empty` drops out through the subsumption pass. Canonicalization is confluent
/// and idempotent, so the default [`join_all`](crate::lattice::JoinSemilattice::join_all)
/// fold yields the same canonical [`OneOf`](Location::OneOf) as collecting and
/// canonicalizing in one pass: a singleton folds to itself, ≥2 to their union.
impl crate::lattice::JoinSemilattice for Location {
    fn bottom() -> Self {
        Self::Empty
    }

    fn join(&self, other: &Self) -> Self {
        resolved::one_of_from_members(resolved::canonical_one_of(vec![
            self.clone(),
            other.clone(),
        ]))
    }
}

/// The meet half of the resolved location lattice, the dual of the join above.
/// ⊤ is [`Unbounded`](Location::Unbounded); the meet intersects and
/// canonicalizes, so `Empty` (⊥) collapses the whole meet and `Unbounded` (⊤)
/// drops out as the meet identity. Two disjoint shapes stay a symbolic
/// [`AllOf`](Location::AllOf); detecting that such a region is actually empty is
/// a separate concern. Canonicalization is confluent and idempotent, so the
/// default [`meet_all`](crate::lattice::MeetSemilattice::meet_all) fold matches a
/// one-pass intersect-and-canonicalize.
impl crate::lattice::MeetSemilattice for Location {
    fn top() -> Self {
        Self::Unbounded
    }

    fn meet(&self, other: &Self) -> Self {
        resolved::all_of_from_members(resolved::canonical_all_of(vec![
            self.clone(),
            other.clone(),
        ]))
    }
}

/// Resolved-level canonicalization: the geometric half. Both combinators flatten
/// their nesting and collapse by containment — a `OneOf` drops a member a sibling
/// subsumes, an `AllOf` drops a member that contains a distinct sibling — because
/// every member is concrete geometry whose subsumption is decidable. Parallels
/// the reference-opaque [`unresolved`] module one level up; the only structural
/// difference between the two is this containment collapse.
mod resolved {
    use super::{Location, Members};

    /// Canonicalize the members of a [`Location::OneOf`] so one logical union
    /// has a single wire form: flatten nested unions, drop any member
    /// geometrically contained by a distinct sibling (redundant in a union),
    /// then sort and dedup. Confluent — the result is independent of input
    /// order.
    pub(super) fn canonical_one_of(children: Vec<Location>) -> Vec<Location> {
        let mut flat: Vec<Location> = Vec::new();
        for child in children {
            match child {
                Location::OneOf { members } => flat.extend_from_slice(members.as_slice()),
                other => flat.push(other),
            }
        }
        // Drop a member subsumed by a distinct sibling. Containment is
        // reflexive, so when two members contain each other (geometrically
        // equal), keep the earlier index and let `dedup` clear the rest after
        // the sort.
        let mut kept: Vec<Location> = (0..flat.len())
            .filter(|&i| {
                let m = &flat[i];
                !flat
                    .iter()
                    .enumerate()
                    .any(|(j, other)| i != j && other.contains(m) && (!m.contains(other) || j < i))
            })
            .map(|i| flat[i].clone())
            .collect();
        kept.sort();
        kept.dedup();
        kept
    }

    /// Rebuild a [`Location`] from already-canonical `OneOf` members: no member
    /// is `Empty` (⊥), so `0 → Empty`, `1 → that member`, `≥2 → OneOf`. The total
    /// join's final step, where the validating [`Location::one_of`] would instead
    /// reject a sub-`2` collapse.
    pub(super) fn one_of_from_members(mut members: Vec<Location>) -> Location {
        match members.len() {
            0 => Location::Empty,
            1 => members.remove(0),
            _ => Location::OneOf {
                members: Members::canonicalize(members, canonical_one_of),
            },
        }
    }

    /// Canonicalize the members of a [`Location::AllOf`] so one logical
    /// intersection has a single wire form: flatten nested intersections,
    /// collapse to `[Empty]` if any member is `Empty` (⊥), drop every
    /// `Unbounded` (⊤) member as the meet identity, drop any member that contains
    /// a distinct sibling (redundant — the contained sibling is tighter), then
    /// sort and dedup. Confluent — the result is independent of input order.
    pub(super) fn canonical_all_of(children: Vec<Location>) -> Vec<Location> {
        let mut flat: Vec<Location> = Vec::new();
        for child in children {
            match child {
                Location::AllOf { members } => flat.extend_from_slice(members.as_slice()),
                other => flat.push(other),
            }
        }
        if flat.iter().any(|m| matches!(m, Location::Empty)) {
            return vec![Location::Empty];
        }
        flat.retain(|m| !matches!(m, Location::Unbounded));
        // Drop a member that contains a distinct sibling (the container is
        // redundant in an intersection — the tighter, contained sibling stands).
        // Dual of the union's subsumption drop. Containment is reflexive, so when
        // two members contain each other (geometrically equal), keep the earlier
        // index and let `dedup` clear the rest after the sort.
        let mut kept: Vec<Location> = (0..flat.len())
            .filter(|&i| {
                let m = &flat[i];
                !flat
                    .iter()
                    .enumerate()
                    .any(|(j, other)| i != j && m.contains(other) && (!other.contains(m) || j < i))
            })
            .map(|i| flat[i].clone())
            .collect();
        kept.sort();
        kept.dedup();
        kept
    }

    /// Rebuild a [`Location`] from already-canonical `AllOf` members:
    /// `0 → Unbounded` (the empty meet is ⊤), `1 → that member`, `≥2 → AllOf`. An
    /// `Empty` collapse from [`canonical_all_of`] surfaces as the single-member
    /// `Empty`. The total meet's final step, where the validating
    /// [`Location::all_of`] would instead reject a sub-`2` collapse.
    pub(super) fn all_of_from_members(mut members: Vec<Location>) -> Location {
        match members.len() {
            0 => Location::Unbounded,
            1 => members.remove(0),
            _ => Location::AllOf {
                members: Members::canonicalize(members, canonical_all_of),
            },
        }
    }
}

// ==================== Unresolved Location ====================

/// A location that may contain unresolved symbolic references.
///
/// Adjacently-tagged serde (`tag` + `content`) because `Resolved` wraps a
/// [`Location`] with its own internal `type` tag — internal tagging on both
/// levels would produce duplicate `type` fields.
///
/// `Eq`/`Ord` propagate through [`Location`]'s hand-implemented Ord; see that
/// type's doc-comment for the f64 handling.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum UnresolvedLocation {
    /// Already resolved to geometry.
    Resolved(Location),
    /// Symbolic reference needing external resolution.
    Reference(LocationReference),
    /// One of these, don't know which (conflicting sources).
    /// Flattened when nested. Requires ≥2 entries.
    ///
    /// Construct via [`UnresolvedLocation::one_of`]; the entry list is sealed
    /// canonical.
    OneOf(Members<UnresolvedLocation>),
    /// All of these at once — the meet dual of [`OneOf`](Self::OneOf).
    /// Flattened when nested. Requires ≥2 entries.
    ///
    /// Construct via [`UnresolvedLocation::all_of`]; the entry list is sealed
    /// canonical.
    AllOf(Members<UnresolvedLocation>),
}

impl UnresolvedLocation {
    /// Create a validated, canonical `OneOf`.
    ///
    /// Canonicalizes the entries so one logical disjunction has a single wire
    /// form: nested `OneOf`s flatten, then the entries sort and dedup. The `≥2`
    /// minimum is checked on the canonical result, so an all-equal disjunction
    /// (which dedups to one entry) is rejected.
    pub fn one_of(entries: Vec<UnresolvedLocation>) -> Result<Self, LocationError> {
        let entries = Members::canonicalize(entries, unresolved::canonical_one_of);
        if entries.len() < 2 {
            return Err(LocationError::TooFewEntries {
                count: entries.len(),
            });
        }
        Ok(Self::OneOf(entries))
    }

    /// Create a validated, canonical `AllOf` — the meet dual of
    /// [`one_of`](Self::one_of).
    ///
    /// Canonicalizes the entries so one logical conjunction has a single wire
    /// form: nested `AllOf`s flatten, then the entries sort and dedup. The `≥2`
    /// minimum is checked on the canonical result, so an all-equal conjunction
    /// (which dedups to one entry) is rejected.
    pub fn all_of(entries: Vec<UnresolvedLocation>) -> Result<Self, LocationError> {
        let entries = Members::canonicalize(entries, unresolved::canonical_all_of);
        if entries.len() < 2 {
            return Err(LocationError::TooFewEntries {
                count: entries.len(),
            });
        }
        Ok(Self::AllOf(entries))
    }

    /// The geometric conflict status: decidably [`Conflict`], still [`Pending`]
    /// resolution, or known [`Consistent`].
    ///
    /// Projecting to the most-permissive resolved skeleton (every `Reference`
    /// becomes ⊤) and asking whether that skeleton denotes nothing is the
    /// decision: if the loosest possible resolution is already empty, no actual
    /// resolution can rescue it, so the location conflicts. Otherwise a remaining
    /// reference leaves it pending — a tighter resolution might still empty it —
    /// and a fully-resolved non-empty skeleton is consistent.
    ///
    /// [`Conflict`]: ConflictStatus::Conflict
    /// [`Pending`]: ConflictStatus::Pending
    /// [`Consistent`]: ConflictStatus::Consistent
    pub fn conflict_status(&self) -> ConflictStatus {
        // A `Resolved` leaf already holds the geometry the skeleton would be, so
        // test it on the borrow rather than cloning it into a fresh skeleton.
        // The skeleton differs from the value only where a `Reference` becomes
        // ⊤, so it's only worth building when one is present.
        let empty = match self {
            Self::Resolved(loc) => loc.denotes_empty(),
            _ => self.permissive_skeleton().denotes_empty(),
        };
        if empty {
            ConflictStatus::Conflict
        } else if self.has_reference() {
            ConflictStatus::Pending
        } else {
            ConflictStatus::Consistent
        }
    }

    /// The most-permissive resolved skeleton: every `Reference` becomes ⊤
    /// ([`Location::Unbounded`]), `Resolved` keeps its geometry, `OneOf` maps to
    /// a union and `AllOf` to an intersection. Built through the canonical
    /// constructors so the result stays canonical.
    fn permissive_skeleton(&self) -> Location {
        match self {
            Self::Resolved(loc) => loc.clone(),
            Self::Reference(_) => Location::Unbounded,
            Self::OneOf(entries) => {
                let members = entries.as_slice().iter().map(Self::permissive_skeleton);
                resolved::one_of_from_members(resolved::canonical_one_of(members.collect()))
            }
            Self::AllOf(entries) => {
                let members = entries.as_slice().iter().map(Self::permissive_skeleton);
                resolved::all_of_from_members(resolved::canonical_all_of(members.collect()))
            }
        }
    }

    /// Whether any unresolved `Reference` remains anywhere in the value.
    fn has_reference(&self) -> bool {
        match self {
            Self::Resolved(_) => false,
            Self::Reference(_) => true,
            Self::OneOf(entries) => entries.as_slice().iter().any(Self::has_reference),
            Self::AllOf(entries) => entries.as_slice().iter().any(Self::has_reference),
        }
    }
}

/// The join half of the unresolved location lattice, mirroring [`Location`]'s
/// one level up. ⊥ is `Resolved(Empty)`; the join unions and canonicalizes, so
/// the canonicalizer's ⊤/⊥ pass drops `Resolved(Empty)` entries and lets a
/// `Resolved(Unbounded)` (⊤) absorb the disjunction. References stay symbolic —
/// no containment collapse runs, only the `OneOf` flatten/sort/dedup. The
/// canonicalization is confluent and idempotent, so the default
/// [`join_all`](crate::lattice::JoinSemilattice::join_all) fold yields the same
/// canonical [`OneOf`](UnresolvedLocation::OneOf) as a one-pass collect: a
/// singleton folds to itself, ≥2 to their disjunction.
impl crate::lattice::JoinSemilattice for UnresolvedLocation {
    fn bottom() -> Self {
        Self::Resolved(Location::Empty)
    }

    fn join(&self, other: &Self) -> Self {
        let mut entries = unresolved::canonical_one_of(vec![self.clone(), other.clone()]);
        match entries.len() {
            0 => Self::bottom(),
            1 => entries.remove(0),
            _ => Self::OneOf(Members::canonicalize(entries, unresolved::canonical_one_of)),
        }
    }
}

/// The meet half of the unresolved location lattice, the dual of the join above.
/// ⊤ is `Resolved(Unbounded)`; the meet conjoins and canonicalizes, so the
/// canonicalizer's ⊤/⊥ pass drops `Resolved(Unbounded)` entries as the meet
/// identity and lets a `Resolved(Empty)` (⊥) absorb the conjunction. References
/// stay symbolic — no containment collapse, only the `AllOf` flatten/sort/dedup.
/// Confluent and idempotent, so the default
/// [`meet_all`](crate::lattice::MeetSemilattice::meet_all) fold matches a
/// one-pass collect.
impl crate::lattice::MeetSemilattice for UnresolvedLocation {
    fn top() -> Self {
        Self::Resolved(Location::Unbounded)
    }

    fn meet(&self, other: &Self) -> Self {
        let mut entries = unresolved::canonical_all_of(vec![self.clone(), other.clone()]);
        match entries.len() {
            0 => Self::top(),
            1 => entries.remove(0),
            _ => Self::AllOf(Members::canonicalize(entries, unresolved::canonical_all_of)),
        }
    }
}

/// Unresolved-level canonicalization, parallel to [`resolved`] one level up. Same
/// flatten and ⊤/⊥ absorption, but no containment collapse: the entries may be
/// symbolic references whose geometry isn't known, so subsumption isn't
/// decidable. The ⊤/⊥ bounds are definitional, not geometric, so they still
/// apply.
mod unresolved {
    use super::{Location, UnresolvedLocation};

    /// Canonicalize the entries of an [`UnresolvedLocation::OneOf`]: flatten
    /// nested `OneOf`s, apply the ⊤/⊥ lattice bounds, then sort and dedup so one
    /// logical disjunction has a single wire form. Confluent — independent of
    /// input order.
    ///
    /// A `Resolved(Unbounded)` (⊤) entry absorbs the disjunction to
    /// `[Resolved(Unbounded)]`, and every `Resolved(Empty)` (⊥) entry drops as
    /// the join identity.
    pub(super) fn canonical_one_of(entries: Vec<UnresolvedLocation>) -> Vec<UnresolvedLocation> {
        let mut flat: Vec<UnresolvedLocation> = Vec::new();
        for entry in entries {
            match entry {
                UnresolvedLocation::OneOf(inner) => flat.extend_from_slice(inner.as_slice()),
                other => flat.push(other),
            }
        }
        if flat
            .iter()
            .any(|e| matches!(e, UnresolvedLocation::Resolved(Location::Unbounded)))
        {
            return vec![UnresolvedLocation::Resolved(Location::Unbounded)];
        }
        flat.retain(|e| !matches!(e, UnresolvedLocation::Resolved(Location::Empty)));
        flat.sort();
        flat.dedup();
        flat
    }

    /// Canonicalize the entries of an [`UnresolvedLocation::AllOf`]: flatten
    /// nested `AllOf`s, apply the ⊤/⊥ lattice bounds, then sort and dedup so one
    /// logical conjunction has a single wire form. Confluent — independent of
    /// input order.
    ///
    /// The dual of `OneOf`'s: a `Resolved(Empty)` (⊥) entry absorbs the
    /// conjunction to `[Resolved(Empty)]`, and every `Resolved(Unbounded)` (⊤)
    /// entry drops as the meet identity.
    pub(super) fn canonical_all_of(entries: Vec<UnresolvedLocation>) -> Vec<UnresolvedLocation> {
        let mut flat: Vec<UnresolvedLocation> = Vec::new();
        for entry in entries {
            match entry {
                UnresolvedLocation::AllOf(inner) => flat.extend_from_slice(inner.as_slice()),
                other => flat.push(other),
            }
        }
        if flat
            .iter()
            .any(|e| matches!(e, UnresolvedLocation::Resolved(Location::Empty)))
        {
            return vec![UnresolvedLocation::Resolved(Location::Empty)];
        }
        flat.retain(|e| !matches!(e, UnresolvedLocation::Resolved(Location::Unbounded)));
        flat.sort();
        flat.dedup();
        flat
    }
}

impl<'de> Deserialize<'de> for UnresolvedLocation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "type", content = "value", rename_all = "snake_case")]
        #[serde(deny_unknown_fields)]
        enum Raw {
            Resolved(Location),
            Reference(LocationReference),
            OneOf(Vec<UnresolvedLocation>),
            AllOf(Vec<UnresolvedLocation>),
        }

        let raw = Raw::deserialize(deserializer)?;
        match raw {
            Raw::Resolved(loc) => Ok(Self::Resolved(loc)),
            Raw::Reference(r) => Ok(Self::Reference(r)),
            Raw::OneOf(entries) => Self::one_of(entries).map_err(serde::de::Error::custom),
            Raw::AllOf(entries) => Self::all_of(entries).map_err(serde::de::Error::custom),
        }
    }
}

// ==================== Location Reference ====================

/// A symbolic reference to a location in an external system.
///
/// These need external resolution (geocoding, OSM/OHM lookup, etc.) to produce
/// a [`Location`]. References are region-scale: entity-scale containment lives
/// on [`crate::facts::attribute::Fact::Relationship`], not here. See the
/// module-level "Entity vs. region scope" note.
#[serde_with::skip_serializing_none]
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LocationReference {
    /// OpenStreetMap element reference.
    #[serde(rename = "osm_reference")]
    Osm {
        osm_type: OsmElementType,
        osm_id: OsmId,
    },
    /// `OpenHistoricalMap` element reference.
    #[serde(rename = "ohm_reference")]
    Ohm { ohm_id: OhmId },
    /// Human-readable place name (e.g., "Paris", "Brooklyn Bridge").
    NamedPlace { name: String },
    /// Street address (e.g., "123 Main St, Springfield").
    Address { address_text: String },
    /// Near some other reference, with optional qualitative distance.
    /// "Near Paris", "near 1600 Penn Ave" all use this.
    Near {
        reference: Box<LocationReference>,
        distance: Option<Distance>,
    },
}

// ==================== Supporting Types ====================

/// Elevation representation with different reference systems.
// TODO: "Current" in CurrentGroundOffset is awkward — ground level changes over time
// (e.g., landfill, excavation, natural erosion). May need temporal qualification later.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Elevation {
    /// Offset from ground level (0 = ground, negative = below ground).
    CurrentGroundOffset { meters: i32 },
    /// Offset from sea level (positive = above, negative = below).
    SeaLevelOffset { meters: i32 },
}

/// Qualitative distance descriptions.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Distance {
    Adjacent,
    AcrossStreet,
    WalkingDistance,
    SameNeighborhood,
    SameDistrict,
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::{resolved, unresolved};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Build a [`GeoPoint`] for test fixtures, surfacing construction
    /// failure as the test's error rather than a panic.
    fn gp(lat: f64, lon: f64) -> Result<GeoPoint, GeoPointError> {
        GeoPoint::new(lat, lon)
    }

    // --- Location (resolved) tests ---

    #[test]
    fn valid_circle() -> TestResult {
        let loc = Location::circle(gp(40.7505, -73.9934)?, Meters(10.0));
        assert!(loc.is_ok());
        Ok(())
    }

    #[test]
    fn valid_point() -> TestResult {
        assert!(matches!(
            Location::point(gp(40.7505, -73.9934)?),
            Location::Circle { radius, .. } if radius.0 == 0.0
        ));
        Ok(())
    }

    #[test]
    fn circle_rejects_negative_radius() -> TestResult {
        let loc = Location::circle(gp(0.0, 0.0)?, Meters(-1.0));
        assert!(matches!(loc, Err(LocationError::NegativeRadius { .. })));
        Ok(())
    }

    #[test]
    fn circle_rejects_non_finite_radius() -> TestResult {
        let loc = Location::circle(gp(0.0, 0.0)?, Meters(f64::INFINITY));
        assert!(matches!(loc, Err(LocationError::NonFiniteRadius { .. })));
        Ok(())
    }

    #[test]
    fn circle_rejects_radius_over_sanity_bound() -> TestResult {
        let over = Meters(MAX_UNCERTAINTY_RADIUS.0 + 1.0);
        assert!(matches!(
            Location::circle(gp(0.0, 0.0)?, over),
            Err(LocationError::RadiusTooLarge { .. })
        ));
        // The bound itself is admissible.
        assert!(Location::circle(gp(0.0, 0.0)?, MAX_UNCERTAINTY_RADIUS).is_ok());
        Ok(())
    }

    #[test]
    fn circle_deserialize_rejects_out_of_range_center() {
        // The center routes through `GeoPoint`'s validating `Deserialize`,
        // so an out-of-range coordinate is rejected at the `Location`
        // boundary as a `Center` error rather than reaching the interior.
        let result: Result<Location, _> = serde_json::from_str(
            r#"{"type":"circle","center":{"lat":91.0,"lon":0.0},"radius":10.0}"#,
        );
        assert!(
            result.is_err(),
            "out-of-range center must fail at deserialize"
        );
    }

    #[test]
    fn circle_boundary_values() -> TestResult {
        assert!(Location::circle(gp(90.0, 180.0)?, Meters(0.0)).is_ok());
        assert!(Location::circle(gp(-90.0, -180.0)?, Meters(0.0)).is_ok());
        Ok(())
    }

    #[test]
    fn circle_negative_zero_radius_is_consistent_across_eq_hash_ord() -> TestResult {
        use std::collections::HashSet;
        use std::hash::{Hash, Hasher};

        // A `-0.0` radius must normalize to `+0.0` so the two forms are
        // indistinguishable under Eq, Hash, and Ord — the contract the
        // manual Hash/Ord impls would otherwise violate. (Center
        // normalization is `GeoPoint`'s, covered in `geo.rs`; here the
        // same center is reused so the radius is the only variable.)
        let center = gp(0.0, 0.0)?;
        let neg = Location::circle(center, Meters(-0.0))?;
        let pos = Location::circle(center, Meters(0.0))?;

        assert_eq!(neg, pos, "negative and positive zero radius must be equal");

        let mut hasher_neg = std::collections::hash_map::DefaultHasher::new();
        let mut hasher_pos = std::collections::hash_map::DefaultHasher::new();
        neg.hash(&mut hasher_neg);
        pos.hash(&mut hasher_pos);
        assert_eq!(
            hasher_neg.finish(),
            hasher_pos.finish(),
            "equal circles must hash equally"
        );

        let mut set = HashSet::new();
        set.insert(neg.clone());
        set.insert(pos.clone());
        assert_eq!(set.len(), 1, "the two zero-forms must dedup to one entry");

        assert_eq!(
            neg.cmp(&pos),
            std::cmp::Ordering::Equal,
            "equal circles must order Equal"
        );
        Ok(())
    }

    // --- UnresolvedLocation tests ---

    #[test]
    fn one_of_serde_roundtrip() -> TestResult {
        let loc = UnresolvedLocation::one_of(vec![
            UnresolvedLocation::Reference(LocationReference::NamedPlace {
                name: "Paris".to_string(),
            }),
            UnresolvedLocation::Reference(LocationReference::Address {
                address_text: "123 Main St".to_string(),
            }),
        ])?;

        let json = serde_json::to_string(&loc)?;
        let deserialized: UnresolvedLocation = serde_json::from_str(&json)?;
        assert_eq!(loc, deserialized);
        Ok(())
    }

    #[test]
    fn one_of_rejects_single_entry() {
        // A single-entry OneOf on the wire must be rejected (minimum 2 entries).
        // The sealed `OneOf` can't be built in-memory below the minimum, so the
        // degenerate shape is supplied as raw wire bytes.
        let json = r#"{"type":"one_of","value":[{"type":"reference","value":{"type":"named_place","name":"Paris"}}]}"#;
        assert!(serde_json::from_str::<UnresolvedLocation>(json).is_err());
    }

    #[test]
    fn resolved_serde_roundtrip() -> TestResult {
        let loc =
            UnresolvedLocation::Resolved(Location::circle(gp(48.8584, 2.2945)?, Meters(10.0))?);
        let json = serde_json::to_string(&loc)?;
        let deserialized: UnresolvedLocation = serde_json::from_str(&json)?;
        assert_eq!(loc, deserialized);
        Ok(())
    }

    #[test]
    fn near_reference_serde_roundtrip() -> TestResult {
        let loc = UnresolvedLocation::Reference(LocationReference::Near {
            reference: Box::new(LocationReference::NamedPlace {
                name: "Paris".to_string(),
            }),
            distance: Some(Distance::WalkingDistance),
        });
        let json = serde_json::to_string(&loc)?;
        let deserialized: UnresolvedLocation = serde_json::from_str(&json)?;
        assert_eq!(loc, deserialized);
        Ok(())
    }

    // --- resolved / unresolved OneOf canonicalization ---

    #[test]
    fn one_of_member_order_is_canonical() -> TestResult {
        // Two orderings of the same disjoint circles produce one wire form.
        let paris = Location::circle(gp(48.8, 2.3)?, Meters(10.0))?;
        let london = Location::circle(gp(51.5, -0.1)?, Meters(10.0))?;
        let forward = Location::one_of(vec![paris.clone(), london.clone()])?;
        let reversed = Location::one_of(vec![london, paris])?;
        assert_eq!(
            serde_json::to_string(&forward)?,
            serde_json::to_string(&reversed)?
        );
        Ok(())
    }

    #[test]
    fn one_of_flattens_nested() -> TestResult {
        // A nested OneOf child flattens — the only useful part of the removed
        // `merge` now lives in the constructor.
        let a = Location::circle(gp(48.8, 2.3)?, Meters(10.0))?;
        let b = Location::circle(gp(51.5, -0.1)?, Meters(10.0))?;
        let c = Location::circle(gp(40.7, -74.0)?, Meters(10.0))?;
        let nested = Location::one_of(vec![Location::one_of(vec![a, b])?, c])?;
        assert!(matches!(nested, Location::OneOf { ref members } if members.len() == 3));
        Ok(())
    }

    #[test]
    fn one_of_drops_subsumed_member() -> TestResult {
        // A small circle inside a big one is redundant in a union, so it drops;
        // what survives is the big circle plus a disjoint third, leaving two
        // members.
        let big = Location::circle(gp(0.0, 0.0)?, Meters(90_000.0))?;
        let small = Location::circle(gp(0.5, 0.5)?, Meters(10.0))?; // ~79 km out, inside big
        let elsewhere = Location::circle(gp(40.7, -74.0)?, Meters(10.0))?;
        let union = Location::one_of(vec![big.clone(), small, elsewhere.clone()])?;
        let Location::OneOf { members } = &union else {
            return Err("expected a union".into());
        };
        assert_eq!(members.len(), 2);
        assert!(members.as_slice().contains(&big) && members.as_slice().contains(&elsewhere));
        Ok(())
    }

    #[test]
    fn one_of_all_subsumed_collapses_and_is_rejected() -> TestResult {
        // Every member contained by one big circle leaves a single survivor;
        // a one-member union is degenerate and rejected.
        let big = Location::circle(gp(0.0, 0.0)?, Meters(90_000.0))?;
        let small = Location::circle(gp(0.5, 0.5)?, Meters(10.0))?;
        let result = Location::one_of(vec![big, small]);
        assert!(matches!(result, Err(LocationError::TooFewEntries { .. })));
        Ok(())
    }

    #[test]
    fn one_of_entry_order_is_canonical() -> TestResult {
        let paris = UnresolvedLocation::Reference(LocationReference::NamedPlace {
            name: "Paris".to_string(),
        });
        let address = UnresolvedLocation::Reference(LocationReference::Address {
            address_text: "123 Main St".to_string(),
        });
        let forward = UnresolvedLocation::one_of(vec![paris.clone(), address.clone()])?;
        let reversed = UnresolvedLocation::one_of(vec![address, paris])?;
        assert_eq!(
            serde_json::to_string(&forward)?,
            serde_json::to_string(&reversed)?
        );
        Ok(())
    }

    #[test]
    fn one_of_flattens_nested_and_dedups() -> TestResult {
        let paris = UnresolvedLocation::Reference(LocationReference::NamedPlace {
            name: "Paris".to_string(),
        });
        let london = UnresolvedLocation::Reference(LocationReference::NamedPlace {
            name: "London".to_string(),
        });
        // A nested OneOf plus a duplicate of one of its entries flatten and
        // dedup to {London, Paris}.
        let nested = UnresolvedLocation::one_of(vec![
            UnresolvedLocation::one_of(vec![paris.clone(), london])?,
            paris,
        ])?;
        let UnresolvedLocation::OneOf(entries) = &nested else {
            return Err("expected a OneOf".into());
        };
        assert_eq!(entries.len(), 2);
        Ok(())
    }

    #[test]
    fn one_of_all_equal_rejected() -> TestResult {
        let paris = UnresolvedLocation::Reference(LocationReference::NamedPlace {
            name: "Paris".to_string(),
        });
        let result = UnresolvedLocation::one_of(vec![paris.clone(), paris]);
        assert!(matches!(result, Err(LocationError::TooFewEntries { .. })));
        Ok(())
    }

    // --- ⊤/⊥ in a OneOf disjunction ---

    #[test]
    fn one_of_join_with_unbounded_collapses_to_unbounded() {
        use crate::lattice::JoinSemilattice;
        // ⊤ absorbs the disjunction: joining a symbolic reference with
        // `Resolved(Unbounded)` leaves just ⊤, even unresolved.
        let paris = UnresolvedLocation::Reference(LocationReference::NamedPlace {
            name: "Paris".to_string(),
        });
        let top = UnresolvedLocation::Resolved(Location::Unbounded);
        assert_eq!(paris.join(&top), top);
    }

    #[test]
    fn one_of_join_drops_empty_entry() {
        use crate::lattice::JoinSemilattice;
        // ⊥ is the join identity: joining a reference with `Resolved(Empty)`
        // drops the empty entry, leaving the reference alone.
        let paris = UnresolvedLocation::Reference(LocationReference::NamedPlace {
            name: "Paris".to_string(),
        });
        let bottom = UnresolvedLocation::Resolved(Location::Empty);
        assert_eq!(paris.join(&bottom), paris);
    }

    // --- canonicalization property tests ---

    use proptest::prelude::*;

    fn arb_circle() -> impl Strategy<Value = Location> {
        (-90.0f64..=90.0, -180.0f64..=180.0, 0.0f64..10000.0).prop_filter_map(
            "valid circle",
            |(lat, lon, r)| {
                let center = GeoPoint::new(lat, lon).ok()?;
                Location::circle(center, Meters(r)).ok()
            },
        )
    }

    proptest! {
        /// `one_of` is confluent: any permutation of the same members yields
        /// one wire form (the determinism the unsorted constructor broke).
        /// Restricted to circles of equal radius so no member subsumes another,
        /// keeping the member count permutation-invariant.
        #[test]
        fn prop_one_of_order_independent(
            circles in prop::collection::vec(
                (-90.0f64..=90.0, -180.0f64..=180.0).prop_filter_map(
                    "distinct-center circle",
                    |(lat, lon)| {
                        let center = GeoPoint::new(lat, lon).ok()?;
                        Location::circle(center, Meters(1.0)).ok()
                    },
                ),
                2..=5,
            ),
            rotate in 0usize..5,
        ) {
            let forward = Location::one_of(circles.clone());
            let mut rotated = circles;
            let len = rotated.len();
            rotated.rotate_right(rotate % len);
            let other = Location::one_of(rotated);
            // Both constructions agree on success/shape regardless of order.
            match (forward, other) {
                (Ok(a), Ok(b)) => prop_assert_eq!(a, b),
                (Err(_), Err(_)) => {}
                (a, b) => prop_assert!(false, "order changed validity: {:?} vs {:?}", a, b),
            }
        }
    }

    // Keep `arb_circle` exercised even when only the proptest above runs.
    proptest! {
        #[test]
        fn prop_circle_round_trips(c in arb_circle()) {
            let json = serde_json::to_string(&c)?;
            let back: Location = serde_json::from_str(&json)?;
            prop_assert_eq!(back, c);
        }
    }

    // --- resolved / unresolved AllOf canonicalization ---

    #[test]
    fn all_of_drops_container_keeps_contained() -> TestResult {
        use crate::lattice::MeetSemilattice;
        // A big circle around a small one at the same center is redundant in an
        // intersection — the small one is the tighter constraint, so the big
        // one drops and the meet collapses to the small circle alone.
        let big = Location::circle(gp(0.0, 0.0)?, Meters(90_000.0))?;
        let small = Location::circle(gp(0.0, 0.0)?, Meters(10.0))?;
        let result = Location::all_of(vec![big, small.clone()]);
        // One survivor → rejected as degenerate, the dual of union's collapse.
        assert!(matches!(result, Err(LocationError::TooFewEntries { .. })));
        // Through the lattice meet, the same collapse yields the lone survivor.
        assert_eq!(
            small.meet(&Location::circle(gp(0.0, 0.0)?, Meters(90_000.0))?),
            small
        );
        Ok(())
    }

    #[test]
    fn meet_unbounded_is_identity() -> TestResult {
        use crate::lattice::MeetSemilattice;
        let c = Location::circle(gp(48.8, 2.3)?, Meters(10.0))?;
        assert_eq!(c.meet(&Location::Unbounded), c);
        Ok(())
    }

    #[test]
    fn meet_empty_is_empty() -> TestResult {
        use crate::lattice::MeetSemilattice;
        let c = Location::circle(gp(48.8, 2.3)?, Meters(10.0))?;
        assert_eq!(c.meet(&Location::Empty), Location::Empty);
        Ok(())
    }

    #[test]
    fn meet_disjoint_circles_stays_symbolic() -> TestResult {
        use crate::lattice::MeetSemilattice;
        // Two far-apart circles meet to a symbolic `AllOf`; recognizing that the
        // region is actually empty is a separate concern.
        let paris = Location::circle(gp(48.8, 2.3)?, Meters(10.0))?;
        let tokyo = Location::circle(gp(35.6, 139.7)?, Meters(10.0))?;
        let meet = paris.meet(&tokyo);
        assert!(matches!(meet, Location::AllOf { ref members } if members.len() == 2));
        Ok(())
    }

    #[test]
    fn all_of_flattens_nested() -> TestResult {
        let a = Location::circle(gp(48.8, 2.3)?, Meters(10.0))?;
        let b = Location::circle(gp(51.5, -0.1)?, Meters(10.0))?;
        let c = Location::circle(gp(40.7, -74.0)?, Meters(10.0))?;
        let nested = Location::all_of(vec![Location::all_of(vec![a, b])?, c])?;
        assert!(matches!(nested, Location::AllOf { ref members } if members.len() == 3));
        Ok(())
    }

    #[test]
    fn all_of_member_order_is_canonical() -> TestResult {
        let paris = Location::circle(gp(48.8, 2.3)?, Meters(10.0))?;
        let london = Location::circle(gp(51.5, -0.1)?, Meters(10.0))?;
        let forward = Location::all_of(vec![paris.clone(), london.clone()])?;
        let reversed = Location::all_of(vec![london, paris])?;
        assert_eq!(
            serde_json::to_string(&forward)?,
            serde_json::to_string(&reversed)?
        );
        Ok(())
    }

    #[test]
    fn all_of_serde_roundtrip() -> TestResult {
        let paris = Location::circle(gp(48.8, 2.3)?, Meters(10.0))?;
        let tokyo = Location::circle(gp(35.6, 139.7)?, Meters(10.0))?;
        let loc = Location::all_of(vec![paris, tokyo])?;
        let json = serde_json::to_string(&loc)?;
        let back: Location = serde_json::from_str(&json)?;
        assert_eq!(loc, back);
        Ok(())
    }

    #[test]
    fn all_of_flattens_and_dedups() -> TestResult {
        let paris = UnresolvedLocation::Reference(LocationReference::NamedPlace {
            name: "Paris".to_string(),
        });
        let london = UnresolvedLocation::Reference(LocationReference::NamedPlace {
            name: "London".to_string(),
        });
        let nested = UnresolvedLocation::all_of(vec![
            UnresolvedLocation::all_of(vec![paris.clone(), london])?,
            paris,
        ])?;
        let UnresolvedLocation::AllOf(entries) = &nested else {
            return Err("expected an AllOf".into());
        };
        assert_eq!(entries.len(), 2);
        Ok(())
    }

    #[test]
    fn all_of_meet_with_empty_collapses_to_empty() {
        use crate::lattice::MeetSemilattice;
        // ⊥ absorbs the conjunction: meeting a reference with `Resolved(Empty)`
        // leaves just ⊥, even unresolved — the dual of `OneOf` ⊤-absorption.
        let paris = UnresolvedLocation::Reference(LocationReference::NamedPlace {
            name: "Paris".to_string(),
        });
        let bottom = UnresolvedLocation::Resolved(Location::Empty);
        assert_eq!(paris.meet(&bottom), bottom);
    }

    #[test]
    fn all_of_meet_drops_unbounded_entry() {
        use crate::lattice::MeetSemilattice;
        // ⊤ is the meet identity: meeting a reference with `Resolved(Unbounded)`
        // drops the unbounded entry, leaving the reference alone.
        let paris = UnresolvedLocation::Reference(LocationReference::NamedPlace {
            name: "Paris".to_string(),
        });
        let top = UnresolvedLocation::Resolved(Location::Unbounded);
        assert_eq!(paris.meet(&top), paris);
    }

    // --- Lattice law harness ---
    //
    // Resolved `Location` runs the core laws structurally (==) and the
    // order/lattice laws denotationally: a disjoint `meet` is kept as a
    // symbolic `AllOf`, so distributivity rearranges the symbolic shape while
    // denoting the same region. `denotes_same` decides equivalence by sampling
    // `covers` over a point set drawn from both expressions.

    use crate::geo::EARTH_RADIUS_M;

    /// Every `(center, radius)` circle reachable in `loc`, gathered through the
    /// production [`Location::for_each_circle`] walk so the sampling oracle can't
    /// drift from the routine it checks. A circle's rim is the most
    /// coverage-sensitive locus, so its neighborhood is where an off-center law
    /// violation shows up.
    fn circles_of(loc: &Location, out: &mut Vec<(GeoPoint, f64)>) {
        loc.for_each_circle(&mut |center, radius| out.push((*center, radius.0)));
    }

    /// The point reached by walking `angle` radians along the great circle from
    /// `center` on `bearing` (radians, clockwise from north) — the standard
    /// spherical direct solution, total and free of seam/pole bias. Sampling by
    /// geodesic offset (not a lat/lon box) keeps the point cloud even at every
    /// latitude, where a `cos(lat)` longitude span would compress to a sliver
    /// near the poles.
    fn offset_point(center: &GeoPoint, angle: f64, bearing: f64) -> Option<GeoPoint> {
        let lat1 = center.lat().to_radians();
        let lon1 = center.lon().to_radians();
        let lat2 = (lat1.sin() * angle.cos() + lat1.cos() * angle.sin() * bearing.cos())
            .clamp(-1.0, 1.0)
            .asin();
        let lon2 = lon1
            + (bearing.sin() * angle.sin() * lat1.cos())
                .atan2(angle.cos() - lat1.sin() * lat2.sin());
        // Wrap longitude back into [-180, 180]; latitude is already in range.
        let mut lon_deg = lon2.to_degrees();
        lon_deg = (lon_deg + 540.0).rem_euclid(360.0) - 180.0;
        GeoPoint::new(lat2.to_degrees(), lon_deg).ok()
    }

    /// A geodesic point cloud around each circle: its center plus a fan of
    /// offsets at a ring of bearings and radial steps reaching just past the
    /// rim. The angular reach is `radius / EARTH_RADIUS` scaled by `SPAN`, so the
    /// samples straddle the rim at any latitude.
    fn sphere_samples(
        circles: &[(GeoPoint, f64)],
        span: f64,
        rings: usize,
        spokes: usize,
    ) -> Vec<GeoPoint> {
        let mut points: Vec<GeoPoint> = Vec::new();
        for (center, r) in circles {
            points.push(*center);
            let max_angle = (r / EARTH_RADIUS_M * span).max(1e-9);
            for ring in 1..=rings {
                let angle = max_angle * ring as f64 / rings as f64;
                for spoke in 0..spokes {
                    let bearing = std::f64::consts::TAU * spoke as f64 / spokes as f64;
                    if let Some(p) = offset_point(center, angle, bearing) {
                        points.push(p);
                    }
                }
            }
        }
        points
    }

    /// Two locations denote the same region iff they agree on `covers` across a
    /// deterministic geodesic point cloud drawn from both expressions. Sampling
    /// per circle keeps the points clustered on the rims even when `arb_location`
    /// draws globe-spanning centers — a coarse global grid would step right over
    /// the ≤10 km disks and only ever sample the centers, where distributivity
    /// rearrangements trivially agree. A real law violation has a difference
    /// region of non-zero area straddling some rim, which the clustered offsets
    /// catch.
    fn denotes_same(a: &Location, b: &Location) -> bool {
        let mut circles = Vec::new();
        circles_of(a, &mut circles);
        circles_of(b, &mut circles);

        // Origin neighborhood when neither expression has a circle (both ⊥/⊤),
        // so `covers` still gets exercised.
        if circles.is_empty()
            && let Ok(origin) = GeoPoint::new(0.0, 0.0)
        {
            circles.push((origin, 1000.0));
        }

        // 1.5× the radius reaches just past each rim, so points land on both
        // sides of it.
        let points = sphere_samples(&circles, 1.5, 8, 12);
        points.iter().all(|p| a.covers(p) == b.covers(p))
    }

    #[test]
    fn denotes_same_discriminates_distinct_regions() -> TestResult {
        // The oracle must reject genuinely different regions, else the order
        // laws would pass vacuously. A circle and a far-disjoint circle cover
        // different points; the same circle covers identically.
        let paris = Location::circle(gp(48.8, 2.3)?, Meters(5000.0))?;
        let paris_again = Location::circle(gp(48.8, 2.3)?, Meters(5000.0))?;
        let tokyo = Location::circle(gp(35.6, 139.7)?, Meters(5000.0))?;
        assert!(denotes_same(&paris, &paris_again));
        assert!(!denotes_same(&paris, &tokyo));
        // A union is strictly larger than one of its disjoint members.
        let union = Location::one_of(vec![paris.clone(), tokyo])?;
        assert!(!denotes_same(&paris, &union));
        Ok(())
    }

    /// An `UncertainDate`-style generator for the resolved `Location` lattice:
    /// the ⊥/⊤ extremes, circles, and the two combinators built from a small
    /// pool of circles so unions and intersections actually overlap and the
    /// containment/identity collapses fire.
    fn arb_location() -> impl Strategy<Value = Location> {
        let leaf = prop_oneof![
            8 => arb_circle(),
            1 => Just(Location::Empty),
            1 => Just(Location::Unbounded),
        ];
        leaf.prop_recursive(3, 16, 4, |inner| {
            let combinable = prop::collection::vec(inner, 2..=4);
            prop_oneof![
                // The total combinators: a sub-2 canonical collapse yields the
                // lone survivor (or ⊥/⊤) instead of the validating constructor's
                // error, keeping the generator total across the whole lattice.
                combinable
                    .clone()
                    .prop_map(|cs| resolved::one_of_from_members(resolved::canonical_one_of(cs))),
                combinable
                    .prop_map(|cs| resolved::all_of_from_members(resolved::canonical_all_of(cs))),
            ]
        })
    }

    crate::lattice_laws!(location_laws, Location, arb_location(), denotes_same);

    /// A generator across the `UnresolvedLocation` lattice: resolved geometry,
    /// opaque references, and the two combinators. Built so the ⊤/⊥ resolved
    /// extremes and duplicate entries recur, exercising the identity-absorption
    /// and dedup paths.
    fn arb_unresolved_location() -> impl Strategy<Value = UnresolvedLocation> {
        let references =
            prop_oneof![Just("Paris"), Just("London"), Just("Tokyo"),].prop_map(|name| {
                UnresolvedLocation::Reference(LocationReference::NamedPlace {
                    name: name.to_string(),
                })
            });
        let leaf = prop_oneof![
            5 => references,
            3 => arb_circle().prop_map(UnresolvedLocation::Resolved),
            1 => Just(UnresolvedLocation::Resolved(Location::Empty)),
            1 => Just(UnresolvedLocation::Resolved(Location::Unbounded)),
        ];
        leaf.prop_recursive(3, 16, 4, |inner| {
            let combinable = prop::collection::vec(inner, 2..=4);
            prop_oneof![
                combinable.clone().prop_map(|es| {
                    let mut entries = unresolved::canonical_one_of(es);
                    match entries.len() {
                        0 => UnresolvedLocation::Resolved(Location::Empty),
                        1 => entries.remove(0),
                        _ => UnresolvedLocation::OneOf(Members::canonicalize(
                            entries,
                            unresolved::canonical_one_of,
                        )),
                    }
                }),
                combinable.prop_map(|es| {
                    let mut entries = unresolved::canonical_all_of(es);
                    match entries.len() {
                        0 => UnresolvedLocation::Resolved(Location::Unbounded),
                        1 => entries.remove(0),
                        _ => UnresolvedLocation::AllOf(Members::canonicalize(
                            entries,
                            unresolved::canonical_all_of,
                        )),
                    }
                }),
            ]
        })
    }

    /// Resolve every `Reference` to a fixed `Location`, mapping `OneOf` to a
    /// union and `AllOf` to an intersection — a lattice homomorphism into the
    /// resolved lattice. The generator's only references are the three named
    /// places; each gets a distinct disjoint circle, so resolution preserves the
    /// structure the symbolic laws need to denote.
    fn resolve(loc: &UnresolvedLocation) -> Location {
        match loc {
            UnresolvedLocation::Resolved(l) => l.clone(),
            UnresolvedLocation::Reference(LocationReference::NamedPlace { name }) => {
                let (lat, lon) = match name.as_str() {
                    "Paris" => (48.8, 2.3),
                    "London" => (51.5, -0.1),
                    _ => (35.6, 139.7), // Tokyo and any other reference
                };
                GeoPoint::new(lat, lon)
                    .ok()
                    .and_then(|c| Location::circle(c, Meters(5000.0)).ok())
                    .unwrap_or(Location::Unbounded)
            }
            UnresolvedLocation::Reference(_) => Location::Unbounded,
            UnresolvedLocation::OneOf(entries) => {
                let members = entries.as_slice().iter().map(resolve).collect();
                resolved::one_of_from_members(resolved::canonical_one_of(members))
            }
            UnresolvedLocation::AllOf(entries) => {
                let members = entries.as_slice().iter().map(resolve).collect();
                resolved::all_of_from_members(resolved::canonical_all_of(members))
            }
        }
    }

    /// Two unresolved locations are equivalent iff resolving each through the
    /// fixed [`resolve`] sweep yields resolved forms that denote the same region.
    /// Resolution is a lattice homomorphism, so the symbolic
    /// absorption/distributivity laws hold up to it — this is what finally
    /// asserts them for the symbolic combinator, the structural `==` couldn't.
    fn resolve_then_denote(a: &UnresolvedLocation, b: &UnresolvedLocation) -> bool {
        denotes_same(&resolve(a), &resolve(b))
    }

    #[test]
    fn resolve_then_denote_discriminates() -> TestResult {
        // The equivalence must reject genuinely different symbolic forms, else
        // the laws pass vacuously. Paris alone differs from Paris-or-Tokyo.
        let paris = UnresolvedLocation::Reference(LocationReference::NamedPlace {
            name: "Paris".to_string(),
        });
        let tokyo = UnresolvedLocation::Reference(LocationReference::NamedPlace {
            name: "Tokyo".to_string(),
        });
        let union = UnresolvedLocation::one_of(vec![paris.clone(), tokyo])?;
        assert!(resolve_then_denote(&paris, &paris));
        assert!(!resolve_then_denote(&paris, &union));
        Ok(())
    }

    crate::lattice_laws!(
        unresolved_location_laws,
        UnresolvedLocation,
        arb_unresolved_location(),
        resolve_then_denote
    );

    // --- denotes_empty: geometric emptiness ---

    /// A dense sampling oracle for emptiness, evaluated with the spherical
    /// [`Location::covers`] — the actual denotation, independent of
    /// `denotes_empty`'s candidate routine. The feasible region, if any, sits
    /// inside some cap, so fan a geodesic point cloud over each cap's own
    /// neighborhood; the region is non-empty iff some sampled point is covered.
    /// Sampling by geodesic offset (not a lat/lon box) keeps the cloud even at
    /// every latitude, so a high-latitude or seam-straddling region isn't
    /// under-sampled by a `cos(lat)`-compressed longitude span.
    fn samples_empty(loc: &Location, grid_steps: usize) -> bool {
        match loc {
            Location::Empty => return true,
            Location::Unbounded => return false,
            _ => {}
        }
        let mut circles: Vec<(GeoPoint, f64)> = Vec::new();
        circles_of(loc, &mut circles);
        if circles.is_empty() {
            return true;
        }
        // Reach to the rim (span 1.0) with a dense ring/spoke fan; the witness,
        // if any, lies within a cap, so its own neighborhood is enough.
        sphere_samples(&circles, 1.0, grid_steps, grid_steps)
            .iter()
            .all(|p| !loc.covers(p))
    }

    proptest! {
        /// The candidate-point routine agrees with a dense geodesic point cloud
        /// over the same region. The witness lemma makes the agreement exact: a
        /// non-empty region has a witness among the candidates, so any
        /// disagreement is a real bug, not a sampling artifact. (Sampling can
        /// only *miss* a sliver and call a non-empty region empty; it never
        /// invents coverage. So the one-sided risk is the routine reporting empty
        /// while sampling finds a point — caught here.)
        #[test]
        fn prop_denotes_empty_matches_dense_grid(loc in arb_location()) {
            let routine = loc.denotes_empty();
            let grid = samples_empty(&loc, 200);
            // The grid can false-negative on a tiny feasible sliver the routine
            // catches exactly; treat only the opposite disagreement as a failure.
            if !grid {
                prop_assert!(
                    !routine,
                    "grid found a covered point but routine reported empty: {loc:?}"
                );
            }
        }
    }

    #[test]
    fn denotes_empty_extremes() {
        assert!(Location::Empty.denotes_empty());
        assert!(!Location::Unbounded.denotes_empty());
    }

    #[test]
    fn single_circle_is_non_empty() -> TestResult {
        // A circle always covers its own center, zero radius included.
        assert!(!Location::circle(gp(40.0, -74.0)?, Meters(100.0))?.denotes_empty());
        assert!(!Location::point(gp(40.0, -74.0)?).denotes_empty());
        Ok(())
    }

    #[test]
    fn disjoint_intersection_denotes_empty() -> TestResult {
        // Two circles whose centers are far apart relative to their radii share
        // no point; the symbolic intersection denotes nothing.
        let paris = Location::circle(gp(48.8566, 2.3522)?, Meters(1000.0))?;
        let tokyo = Location::circle(gp(35.6762, 139.6503)?, Meters(1000.0))?;
        let meet = Location::all_of(vec![paris, tokyo])?;
        assert!(meet.denotes_empty());
        Ok(())
    }

    #[test]
    fn overlapping_intersection_is_non_empty() -> TestResult {
        // Two circles ~500 m apart with 5 km radii overlap broadly; their
        // intersection is a lens, far from empty.
        let a = Location::circle(gp(40.0, -74.0)?, Meters(5000.0))?;
        let b = Location::circle(gp(40.005, -74.0)?, Meters(5000.0))?;
        let meet = Location::all_of(vec![a, b])?;
        assert!(!meet.denotes_empty());
        Ok(())
    }

    #[test]
    fn nested_intersection_is_non_empty() -> TestResult {
        // A small circle inside a large one (canonicalization keeps the tighter
        // one alone, but build the meet directly): every point of the small disk
        // is covered, so the region is the small disk — non-empty.
        let big = Location::circle(gp(40.0, -74.0)?, Meters(10_000.0))?;
        let small = Location::circle(gp(40.001, -74.001)?, Meters(100.0))?;
        // Containment collapses this to the lone small circle; assert that the
        // collapsed result still denotes a place.
        let meet = small.clone();
        assert!(!meet.denotes_empty());
        // And the genuine container/contained pair, evaluated before collapse,
        // agrees through the grid.
        assert!(
            !Location::AllOf {
                members: Members::canonicalize(vec![big, small], resolved::canonical_all_of),
            }
            .denotes_empty()
        );
        Ok(())
    }

    #[test]
    fn three_circle_chain_no_common_point_is_empty() -> TestResult {
        // A line of three circles where each consecutive pair overlaps but the
        // ends are far apart: there is no point common to all three. Spacing 8 km
        // with 5 km radii — neighbors overlap (gap 8 < 10), ends are 16 km apart
        // (> 10), and the middle disk doesn't reach into both ends at once.
        let a = Location::circle(gp(40.0, -74.0)?, Meters(5000.0))?;
        let b = Location::circle(gp(40.0719, -74.0)?, Meters(5000.0))?; // ~8 km north of a
        let c = Location::circle(gp(40.1438, -74.0)?, Meters(5000.0))?; // ~8 km north of b
        let meet = Location::AllOf {
            members: Members::canonicalize(vec![a, b, c], resolved::canonical_all_of),
        };
        assert!(meet.denotes_empty());
        Ok(())
    }

    #[test]
    fn three_circle_chain_with_common_point_is_non_empty() -> TestResult {
        // The same three circles tightened together (2 km spacing, 5 km radii):
        // all three now overlap a shared core, so the meet is non-empty.
        let a = Location::circle(gp(40.0, -74.0)?, Meters(5000.0))?;
        let b = Location::circle(gp(40.018, -74.0)?, Meters(5000.0))?; // ~2 km north
        let c = Location::circle(gp(40.036, -74.0)?, Meters(5000.0))?; // ~4 km north of a
        let meet = Location::AllOf {
            members: Members::canonicalize(vec![a, b, c], resolved::canonical_all_of),
        };
        assert!(!meet.denotes_empty());
        Ok(())
    }

    #[test]
    fn one_of_disjoint_circles_is_non_empty() -> TestResult {
        // A union of two disjoint circles covers each one's center; emptiness is
        // an intersection phenomenon, not a union one.
        let paris = Location::circle(gp(48.8566, 2.3522)?, Meters(1000.0))?;
        let tokyo = Location::circle(gp(35.6762, 139.6503)?, Meters(1000.0))?;
        assert!(!Location::one_of(vec![paris, tokyo])?.denotes_empty());
        Ok(())
    }

    #[test]
    fn antimeridian_intersection_is_non_empty() -> TestResult {
        // Two 20 km circles either side of the antimeridian (±179.95°, ~11 km
        // apart on the sphere) overlap broadly. Native spherical geometry has no
        // seam, so the meet reads non-empty — a regression guard against any
        // longitude-wrapping creeping back in.
        let west = Location::circle(gp(0.0, 179.95)?, Meters(20_000.0))?;
        let east = Location::circle(gp(0.0, -179.95)?, Meters(20_000.0))?;
        let meet = Location::all_of(vec![west, east])?;
        assert!(
            !meet.denotes_empty(),
            "overlap across the seam must not read empty"
        );

        let unresolved = UnresolvedLocation::all_of(vec![
            UnresolvedLocation::Resolved(Location::circle(gp(0.0, 179.95)?, Meters(20_000.0))?),
            UnresolvedLocation::Resolved(Location::circle(gp(0.0, -179.95)?, Meters(20_000.0))?),
        ])?;
        assert_ne!(unresolved.conflict_status(), ConflictStatus::Conflict);
        Ok(())
    }

    #[test]
    fn near_pole_intersection_is_non_empty() -> TestResult {
        // Two circles near the north pole at opposite longitudes whose disks
        // overlap across it (centers ~2.2 km apart on the sphere). Native
        // spherical geometry keeps them honestly close, with no pole
        // singularity — a regression guard against any `cos(lat)` metric.
        let a = Location::circle(gp(89.99, 0.0)?, Meters(5_000.0))?;
        let b = Location::circle(gp(89.99, 180.0)?, Meters(5_000.0))?;
        let meet = Location::all_of(vec![a, b])?;
        assert!(
            !meet.denotes_empty(),
            "overlap across the pole must not read empty"
        );
        Ok(())
    }

    #[test]
    fn far_disjoint_intersection_stays_empty() -> TestResult {
        // The fix must not flip genuinely-disjoint regions to non-empty: two
        // small circles a continent apart still share no point.
        let paris = Location::circle(gp(48.8566, 2.3522)?, Meters(1000.0))?;
        let tokyo = Location::circle(gp(35.6762, 139.6503)?, Meters(1000.0))?;
        let meet = Location::all_of(vec![paris, tokyo])?;
        assert!(meet.denotes_empty());
        Ok(())
    }

    // --- conflict_status: 3-valued ---

    #[test]
    fn resolved_disjoint_all_of_conflicts() -> TestResult {
        // Two fully-resolved far-apart circles: the skeleton is the disjoint
        // intersection, which denotes nothing, so the status is Conflict.
        let paris =
            UnresolvedLocation::Resolved(Location::circle(gp(48.8566, 2.3522)?, Meters(1000.0))?);
        let tokyo =
            UnresolvedLocation::Resolved(Location::circle(gp(35.6762, 139.6503)?, Meters(1000.0))?);
        let loc = UnresolvedLocation::all_of(vec![paris, tokyo])?;
        assert_eq!(loc.conflict_status(), ConflictStatus::Conflict);
        Ok(())
    }

    #[test]
    fn all_of_with_reference_is_pending() -> TestResult {
        // A circle conjoined with an unresolved reference: the permissive
        // skeleton replaces the reference with ⊤, leaving the non-empty circle,
        // so it isn't a Conflict — but the reference could still empty it, so
        // Pending.
        let circle =
            UnresolvedLocation::Resolved(Location::circle(gp(40.0, -74.0)?, Meters(1000.0))?);
        let reference = UnresolvedLocation::Reference(LocationReference::NamedPlace {
            name: "Paris".to_string(),
        });
        let loc = UnresolvedLocation::all_of(vec![circle, reference])?;
        assert_eq!(loc.conflict_status(), ConflictStatus::Pending);
        Ok(())
    }

    #[test]
    fn resolved_overlapping_all_of_is_consistent() -> TestResult {
        // Two fully-resolved overlapping circles: the skeleton is a non-empty
        // intersection with no remaining reference, so Consistent.
        let a = UnresolvedLocation::Resolved(Location::circle(gp(40.0, -74.0)?, Meters(5000.0))?);
        let b = UnresolvedLocation::Resolved(Location::circle(gp(40.005, -74.0)?, Meters(5000.0))?);
        let loc = UnresolvedLocation::all_of(vec![a, b])?;
        assert_eq!(loc.conflict_status(), ConflictStatus::Consistent);
        Ok(())
    }

    #[test]
    fn lone_reference_is_pending() {
        // An unresolved reference alone projects to ⊤ (non-empty) and still
        // carries a reference, so Pending.
        let loc = UnresolvedLocation::Reference(LocationReference::NamedPlace {
            name: "Paris".to_string(),
        });
        assert_eq!(loc.conflict_status(), ConflictStatus::Pending);
    }

    #[test]
    fn resolved_empty_conflicts() {
        // A syntactic ⊥ is the degenerate Conflict the submit guard must keep
        // rejecting.
        let loc = UnresolvedLocation::Resolved(Location::Empty);
        assert_eq!(loc.conflict_status(), ConflictStatus::Conflict);
    }

    // --- deeply-nested deserialize is bounded by serde's recursion limit ---

    #[test]
    fn deeply_nested_location_rejected_at_deserialize() {
        // A wire-depth attack (alternating one_of/all_of nested far past
        // serde_json's ~128 recursion limit) must fail at `from_str` rather than
        // overflow the stack. The circle cap bounds breadth; serde's recursion
        // limit bounds depth, so the parser rejects the nesting before any of our
        // geometry runs.
        let depth = 600;
        let mut json = String::new();
        for level in 0..depth {
            let tag = if level % 2 == 0 { "one_of" } else { "all_of" };
            json.push_str(&format!(r#"{{"type":"{tag}","members":["#));
        }
        json.push_str(r#"{"type":"circle","center":{"lat":0.0,"lon":0.0},"radius":1.0}"#);
        for _ in 0..depth {
            json.push_str("]}");
        }
        let result: Result<Location, _> = serde_json::from_str(&json);
        assert!(
            result.is_err(),
            "deeply-nested location must be rejected at deserialize, not overflow"
        );
    }

    // --- denotes_empty over genuinely-overlapping 3+ circle clusters ---

    /// Circles deliberately clustered so they mutually overlap: centers within a
    /// shared ~3 km local patch near the equator and radii in a comparable 2–6 km
    /// band, so the meets actually intersect (unlike `arb_circle`'s globe-scattered
    /// ≤10 km draws, which almost never overlap). Exercises the 3+-cap witness
    /// path the routine's pairwise-rim-crossing candidates must cover.
    fn arb_clustered_circle() -> impl Strategy<Value = Location> {
        (-0.03f64..=0.03, -0.03f64..=0.03, 2000.0f64..=6000.0).prop_filter_map(
            "clustered circle",
            |(dlat, dlon, r)| {
                let center = GeoPoint::new(dlat, dlon).ok()?;
                Location::circle(center, Meters(r)).ok()
            },
        )
    }

    proptest! {
        /// `denotes_empty` agrees with the dense spherical sampler on `AllOf` and
        /// mixed `OneOf`/`AllOf` expressions over genuinely-overlapping clustered
        /// circles. The sampler can only false-negative a sliver, so the only
        /// real disagreement — routine empty, grid covered — is the failure.
        #[test]
        fn prop_denotes_empty_overlapping_cluster(
            circles in prop::collection::vec(arb_clustered_circle(), 3..=5),
        ) {
            // An intersection of all the overlapping circles, plus a mixed
            // expression that disjoins the first into the conjunction so the
            // cross-branch witness path is exercised too. The total
            // `*_from_members` helpers keep a containment collapse from producing
            // a sub-2 combinator.
            let all_of =
                resolved::all_of_from_members(resolved::canonical_all_of(circles.clone()));
            let head = circles[0].clone();
            let tail: Vec<Location> = circles[1..].to_vec();
            let mixed_inner = resolved::all_of_from_members(resolved::canonical_all_of(tail));
            let mixed =
                resolved::one_of_from_members(resolved::canonical_one_of(vec![head, mixed_inner]));
            for loc in [all_of, mixed] {
                let routine = loc.denotes_empty();
                let grid = samples_empty(&loc, 200);
                if !grid {
                    prop_assert!(
                        !routine,
                        "grid found a covered point but routine reported empty: {loc:?}"
                    );
                }
            }
        }
    }

    /// An equilateral triangle of circles near the equator: three centers at the
    /// triangle's vertices (side `s` meters) with equal radius `r`, built through
    /// the local flat approximation (~111 195 m per degree). Returned in canonical
    /// `AllOf` form. The caller picks `s`, `r` to land on either side of the
    /// "pairwise overlap, empty triple" boundary.
    fn triangle_all_of(side_m: f64, radius_m: f64) -> Result<Location, Box<dyn std::error::Error>> {
        const M_PER_DEG: f64 = 111_195.0;
        // Centroid at the origin; vertices at the circumradius `s/√3` on bearings
        // 90°/210°/330°, near the equator where a degree of lat and of lon are
        // the same length.
        let circumradius_deg = side_m / 3.0f64.sqrt() / M_PER_DEG;
        let mut circles = Vec::with_capacity(3);
        for k in 0..3 {
            let theta = std::f64::consts::FRAC_PI_2 + std::f64::consts::TAU * k as f64 / 3.0;
            let lat = circumradius_deg * theta.sin();
            let lon = circumradius_deg * theta.cos();
            circles.push(Location::circle(gp(lat, lon)?, Meters(radius_m))?);
        }
        Ok(Location::AllOf {
            members: Members::canonicalize(circles, resolved::canonical_all_of),
        })
    }

    #[test]
    fn reuleaux_pairwise_overlap_empty_triple_is_empty() -> TestResult {
        // Side 9 km, radius 5 km: each pair overlaps (9 < 2·5), but the
        // circumradius 9/√3 ≈ 5.196 km exceeds the 5 km radius, so the centroid —
        // the only point that could lie in all three — sits just outside every
        // circle. The triple intersection is empty though no point of it is a cap
        // center or a single pairwise rim crossing: the Reuleaux witness path.
        let r = 5_000.0;
        let s = 9_000.0;
        let Location::AllOf { members } = triangle_all_of(s, r)? else {
            return Err("expected an AllOf".into());
        };
        let caps = members.as_slice();
        // Each pair genuinely overlaps — assert it so the test can't pass on a
        // degenerate all-disjoint triangle.
        for i in 0..caps.len() {
            for j in (i + 1)..caps.len() {
                let pair = Location::AllOf {
                    members: Members::canonicalize(
                        vec![caps[i].clone(), caps[j].clone()],
                        resolved::canonical_all_of,
                    ),
                };
                assert!(!pair.denotes_empty(), "pair {i},{j} must overlap");
            }
        }
        let triple = Location::AllOf {
            members: Members::canonicalize(caps.to_vec(), resolved::canonical_all_of),
        };
        assert!(
            triple.denotes_empty(),
            "pairwise-overlapping triangle with empty triple must read empty"
        );
        // Cross-check against the dense sampler: it must agree the triple is empty.
        assert!(
            samples_empty(&triple, 200),
            "grid disagrees: triple non-empty"
        );
        Ok(())
    }

    #[test]
    fn reuleaux_tighter_triple_is_non_empty() -> TestResult {
        // Same 9 km triangle, radius widened to 7 km: now the circumradius
        // 5.196 km is inside the 7 km radius, so the centroid lies in all three
        // circles and the triple intersection is a genuine Reuleaux-shaped core.
        let r = 7_000.0;
        let s = 9_000.0;
        let triple = triangle_all_of(s, r)?;
        assert!(
            !triple.denotes_empty(),
            "tighter triangle with a shared core must read non-empty"
        );
        assert!(!samples_empty(&triple, 200), "grid disagrees: triple empty");
        Ok(())
    }
}
