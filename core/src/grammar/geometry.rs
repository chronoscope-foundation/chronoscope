//! Geometry primitives referenced by depiction and feature facts.
//!
//! [`Region`] is a binary mask over an image's pixel grid, stored as canonical
//! row-major run lengths. It is the localization SAM-family segmentation models
//! produce, operable in place: area, `IoU`, and a tight bounding box.
//!
//! [`ProportionalCoord`] is one validated `0.0..=1.0` coordinate component; it
//! holds the finite-check, range-check, and negative-zero normalization that
//! [`ProportionalRect`] and [`ProportionalPoint`] share, so that machinery
//! lives in one place.
//!
//! [`ProportionalRect`] is an axis-aligned rectangle in `0.0..=1.0`
//! proportional coordinates, given as two corner [`ProportionalPoint`]s in
//! canonical order. Shared between [`ImageGeometry::BBox`] and
//! [`crate::grammar::composites::SubimageRegion::Rect`] — same validation, same
//! wire shape.
//!
//! [`ProportionalPolyline`] is a validated trace in `0.0..=1.0` proportional
//! coordinates, the vector path-marking shape — a road or boundary traced on a
//! sheet in the image's own frame.
//!
//! [`ImageGeometry`] is the image-space localization sum a depiction carries —
//! a pixel-precise [`Region`] mask, an axis-aligned [`ProportionalRect`] bbox, or
//! a [`ProportionalPolyline`] trace. Each form arrives from a real pipeline:
//! SAM-family models emit region masks, VLMs that produce normalized bbox output
//! (Gemma 3, Florence-2) emit bboxes, and a path-marking pass emits polylines.
//! All are anchored to the image's own pixel / proportional frame; a geographic
//! reading is a property of the image's projection, derived downstream.

use std::ops::Range;

use chronoscope_macros::grammar_type;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::finite::Finite;

/// Largest side length a [`Dimensions`] grid may declare. It bounds `width *
/// height` (and thus every run value and any densified buffer) well under
/// `u32::MAX`, so the product forms safely even on a 32-bit `usize` target like
/// the wasm client that parses stored facts. Real imagery is downscaled below
/// this before analysis; the cap exists to reject a hostile fact, not to
/// constrain a legitimate one.
pub const MAX_REGION_DIM: u32 = 16_384;

/// A validated pixel grid: positive `width` and `height`, each within
/// [`MAX_REGION_DIM`], so `width * height` forms safely in `u64` on any target.
/// The grid a [`Region`] carries and the pair a [`GridMismatch`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
pub struct Dimensions {
    width: u32,
    height: u32,
}

impl Dimensions {
    /// Validate a grid: both sides positive and within [`MAX_REGION_DIM`].
    pub fn new(width: u32, height: u32) -> Result<Self, RegionError> {
        if width == 0 || height == 0 {
            return Err(RegionError::ZeroDimension { width, height });
        }
        if width > MAX_REGION_DIM || height > MAX_REGION_DIM {
            return Err(RegionError::DimensionTooLarge {
                width,
                height,
                max: MAX_REGION_DIM,
            });
        }
        Ok(Self { width, height })
    }

    /// Grid width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Grid height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Total pixel count. The [`MAX_REGION_DIM`] cap keeps `width * height`
    /// within `u32`.
    pub fn pixel_count(&self) -> u32 {
        self.width * self.height
    }
}

impl std::fmt::Display for Dimensions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}x{}", self.width, self.height)
    }
}

impl<'de> Deserialize<'de> for Dimensions {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            width: u32,
            height: u32,
        }
        let wire = Wire::deserialize(deserializer)?;
        Dimensions::new(wire.width, wire.height).map_err(serde::de::Error::custom)
    }
}

/// A binary mask over an image's pixel grid, stored as row-major run lengths.
///
/// Runs alternate background then foreground, read in raster order (left to
/// right, top to bottom). A leading `0` marks a mask that starts foreground;
/// every later run is at least `1`, and the runs sum to the grid's pixel count.
/// Those rules make the encoding canonical: each dense mask has exactly one run
/// list, so a mask's value equality and ordering follow what it denotes rather
/// than how it was built. This is the localization SAM-family segmentation
/// models produce.
///
/// The grid rides on the value as a [`Dimensions`], so
/// [`intersection_over_union`] and [`bounding_rect`] are defined without an
/// ambient image: two masks with equal runs on differently shaped grids denote
/// different pixel sets and compare unequal, which is correct. `Eq`/`Hash`/`Ord`
/// derive over the fields, and the derived `Ord` is deterministic, so the field
/// order is part of the wire contract.
///
/// [`intersection_over_union`]: Region::intersection_over_union
/// [`bounding_rect`]: Region::bounding_rect
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
pub struct Region {
    dimensions: Dimensions,
    runs: Vec<u32>,
}

impl Region {
    /// Assemble a region from a validated grid and row-major run lengths,
    /// holding the canonical form. This is the untrusted-run-list path:
    /// deserialization routes here and it re-imposes the canonical invariant,
    /// while [`Region::from_dense`] builds canonical runs directly and skips it.
    /// That is why the run-shape errors surface only here.
    pub(crate) fn new(dimensions: Dimensions, runs: Vec<u32>) -> Result<Self, RegionError> {
        Self::validate_runs(&runs, u64::from(dimensions.pixel_count()))?;
        Ok(Self { dimensions, runs })
    }

    /// Encode a dense row-major mask, one `bool` per pixel with foreground
    /// `true`. The slice length must be the grid's pixel count.
    ///
    /// A mask with no foreground is `Ok(None)`, not a region: a `Region` always
    /// holds at least one pixel, so callers filter the empty case here rather than
    /// carrying an empty mask that has no centroid and no bounding box. A length
    /// that does not match the grid stays a loud `Err` at this boundary.
    pub fn from_dense(
        dimensions: Dimensions,
        pixels: &[bool],
    ) -> Result<Option<Self>, RegionError> {
        if pixels.len() != dimensions.pixel_count() as usize {
            return Err(RegionError::LengthMismatch {
                dimensions,
                len: pixels.len(),
            });
        }

        // Maximal runs with a leading zero iff the mask opens foreground: the
        // canonical form by construction.
        let mut runs = Vec::new();
        let mut foreground = false;
        let mut run: u32 = 0;
        for &pixel in pixels {
            if pixel == foreground {
                run += 1;
            } else {
                runs.push(run);
                foreground = pixel;
                run = 1;
            }
        }
        runs.push(run);
        let region = Self { dimensions, runs };
        Ok((!region.is_empty()).then_some(region))
    }

    /// Assemble a region from its foreground pixels' linear (row-major) indices,
    /// the inverse of [`Region::foreground_pixels`]. `pixels` must be strictly
    /// ascending and each below the grid's pixel count. An empty slice is
    /// `Ok(None)`, mirroring [`Region::from_dense`]: a `Region` always holds at
    /// least one pixel.
    ///
    /// This lets a caller carve a subset of one region's pixels and rebuild a
    /// region from exactly those, staying proportional to that subset rather than
    /// densifying the grid. It is a `pub` boundary crossed from outside the crate,
    /// so it validates the indices rather than trusting them, then hands the
    /// assembled runs to `Region::new` for the canonical-form backstop.
    pub fn from_pixels(
        dimensions: Dimensions,
        pixels: &[usize],
    ) -> Result<Option<Self>, RegionError> {
        if pixels.is_empty() {
            return Ok(None);
        }
        // Strictly ascending, each in range: the invariant that makes the run
        // build below sound — every background gap and the trailing gap are
        // non-negative, and consecutive indices merge into one foreground run so
        // no interior gap is zero-length.
        for i in 1..pixels.len() {
            if pixels[i] <= pixels[i - 1] {
                return Err(RegionError::PixelsNotAscending {
                    pixel: pixels[i],
                    previous: pixels[i - 1],
                });
            }
        }
        let count = dimensions.pixel_count() as usize;
        let last = pixels[pixels.len() - 1];
        if last >= count {
            return Err(RegionError::PixelOutOfRange {
                pixel: last,
                pixel_count: dimensions.pixel_count(),
            });
        }

        // A background gap then a foreground run per maximal consecutive group,
        // the canonical alternating form `from_dense` produces.
        let mut runs: Vec<u32> = Vec::new();
        let mut cursor = 0usize;
        let mut i = 0usize;
        while i < pixels.len() {
            let start = pixels[i];
            runs.push((start - cursor) as u32);
            let mut j = i;
            while j + 1 < pixels.len() && pixels[j + 1] == pixels[j] + 1 {
                j += 1;
            }
            runs.push((pixels[j] - start + 1) as u32);
            cursor = pixels[j] + 1;
            i = j + 1;
        }
        let trailing = count - cursor;
        if trailing > 0 {
            runs.push(trailing as u32);
        }

        Ok(Some(Self::new(dimensions, runs)?))
    }

    /// The mask's pixel grid.
    pub fn dimensions(&self) -> Dimensions {
        self.dimensions
    }

    /// The mask's grid width in pixels.
    pub fn width(&self) -> u32 {
        self.dimensions.width()
    }

    /// The mask's grid height in pixels.
    pub fn height(&self) -> u32 {
        self.dimensions.height()
    }

    /// Foreground pixel count. Foreground runs sit at the odd indices, since the
    /// list opens on background.
    pub fn area(&self) -> u64 {
        self.runs
            .iter()
            .skip(1)
            .step_by(2)
            .map(|&r| u64::from(r))
            .sum()
    }

    /// Whether the mask has no foreground pixels.
    pub fn is_empty(&self) -> bool {
        self.area() == 0
    }

    /// Decode to a dense row-major mask, one `bool` per pixel. Allocates the
    /// full pixel count, bounded by [`MAX_REGION_DIM`].
    pub fn to_dense(&self) -> Vec<bool> {
        let total = (self.width() as usize) * (self.height() as usize);
        let mut pixels = Vec::with_capacity(total);
        let mut foreground = false;
        for &run in &self.runs {
            pixels.resize(pixels.len() + run as usize, foreground);
            foreground = !foreground;
        }
        pixels
    }

    /// Intersection over union with another mask on the same grid (`IoU`: shared
    /// foreground area over combined foreground area). `1.0` for equal masks,
    /// `0.0` for disjoint ones. Errors if the grids differ, since the metric is
    /// undefined across grids.
    pub fn intersection_over_union(&self, other: &Region) -> Result<f64, GridMismatch> {
        if self.dimensions != other.dimensions {
            return Err(GridMismatch {
                a: self.dimensions,
                b: other.dimensions,
            });
        }
        let (self_area, other_area) = (self.area(), other.area());
        // A `Region` is never empty, so both areas are positive and the union
        // below is never zero.
        let intersection = self.foreground_overlap(other);
        let union = self_area + other_area - intersection;
        Ok(intersection as f64 / union as f64)
    }

    /// Tight foreground bounding box in `0.0..=1.0` proportional image
    /// coordinates, or `None` when the mask is empty.
    pub fn bounding_rect(&self) -> Option<ProportionalRect> {
        let mut runs = self.foreground_row_runs();
        let (top, first) = runs.next()?;
        let (mut min_col, mut max_end, mut bottom) = (first.start, first.end, top);
        // Raster order puts the first run on the top row and the last on the
        // bottom one, so the rows fall out of the order and only the columns
        // take a min and max.
        for (row, columns) in runs {
            min_col = min_col.min(columns.start);
            max_end = max_end.max(columns.end);
            bottom = row;
        }
        // Half-open pixel spans to the proportional box: columns
        // `[min_col, max_end)` and rows `[top, bottom + 1)`, each over its
        // dimension. Every operand lies in `[0, 1]`, so the only failure
        // `ProportionalRect::new` could report is unreachable here.
        ProportionalRect::new(
            f64::from(min_col) / f64::from(self.width()),
            f64::from(top) / f64::from(self.height()),
            f64::from(max_end) / f64::from(self.width()),
            f64::from(bottom + 1) / f64::from(self.height()),
        )
        .ok()
    }

    /// The foreground centroid as a [`ProportionalPoint`], the mean of the
    /// foreground pixel positions.
    pub fn centroid(&self) -> ProportionalPoint {
        let (mut sum_col, mut sum_row) = (0u64, 0u64);
        for (row, columns) in self.foreground_row_runs() {
            let (start, len) = (
                u64::from(columns.start),
                u64::from(columns.end - columns.start),
            );
            // Columns `start .. start + len`: a `len`-term arithmetic series. The
            // product is of consecutive integers, so the halving is exact.
            sum_col += len * start + len * (len - 1) / 2;
            sum_row += u64::from(row) * len;
        }
        // Mean pixel over the grid extent. A column is in `[0, width)`, so the
        // ratio is in `[0, 1)`; rows likewise.
        let area = self.area() as f64;
        let x = sum_col as f64 / (area * f64::from(self.width()));
        let y = sum_row as f64 / (area * f64::from(self.height()));
        ProportionalPoint {
            x: ProportionalCoord::new_unchecked(x),
            y: ProportionalCoord::new_unchecked(y),
        }
    }

    /// Shared foreground pixel count with `other`, assuming equal grids: merge
    /// the two foreground interval lists, which are sorted by linear index.
    fn foreground_overlap(&self, other: &Region) -> u64 {
        let (a, b) = (self.foreground_intervals(), other.foreground_intervals());
        let (mut ai, mut bi) = (a.into_iter(), b.into_iter());
        let (mut a_next, mut b_next) = (ai.next(), bi.next());
        let mut overlap = 0u64;
        while let (Some((a0, a1)), Some((b0, b1))) = (a_next, b_next) {
            let (lo, hi) = (a0.max(b0), a1.min(b1));
            if lo < hi {
                overlap += hi - lo;
            }
            if a1 <= b1 {
                a_next = ai.next();
            } else {
                b_next = bi.next();
            }
        }
        overlap
    }

    /// The foreground pixels' linear (row-major) indices, in raster order,
    /// without densifying the whole grid: the runs already carry the foreground
    /// as intervals, so a caller assigning per-pixel ownership walks only the
    /// pixels a region actually covers.
    pub fn foreground_pixels(&self) -> impl Iterator<Item = usize> {
        self.foreground_intervals()
            .into_iter()
            .flat_map(|(start, end)| (start as usize)..(end as usize))
    }

    /// The foreground as `(row, columns)` runs in raster order, each lying
    /// within one row: a stored run that wraps past a row's end splits there. A
    /// consumer that works row by row pays per run here rather than per pixel.
    pub fn foreground_row_runs(&self) -> impl Iterator<Item = (u32, Range<u32>)> {
        let width = u64::from(self.width());
        self.foreground_intervals()
            .into_iter()
            .flat_map(move |(start, end)| {
                // A foreground run is never empty, so `end - 1` is its last
                // pixel. Every row and column fits in `u32` under the
                // `MAX_REGION_DIM` cap.
                (start / width..=(end - 1) / width).map(move |row| {
                    let row_start = row * width;
                    let from = start.max(row_start) - row_start;
                    let to = end.min(row_start + width) - row_start;
                    (row as u32, from as u32..to as u32)
                })
            })
    }

    /// Foreground runs as half-open `[start, end)` intervals in linear index
    /// space, in raster order.
    fn foreground_intervals(&self) -> Vec<(u64, u64)> {
        let mut intervals = Vec::with_capacity(self.runs.len() / 2 + 1);
        let mut cursor: u64 = 0;
        for (index, &run) in self.runs.iter().enumerate() {
            let len = u64::from(run);
            if index % 2 == 1 {
                intervals.push((cursor, cursor + len));
            }
            cursor += len;
        }
        intervals
    }

    fn validate_runs(runs: &[u32], area: u64) -> Result<(), RegionError> {
        if runs.is_empty() {
            return Err(RegionError::EmptyRuns);
        }
        // Only the leading background run may be zero; a zero anywhere else
        // splits a run and gives the same mask a second encoding.
        if let Some(offset) = runs.iter().skip(1).position(|&run| run == 0) {
            return Err(RegionError::NonCanonicalZero { index: offset + 1 });
        }
        let sum: u64 = runs.iter().map(|&run| u64::from(run)).sum();
        if sum != area {
            return Err(RegionError::RunSumMismatch { sum, area });
        }
        // Foreground runs sit at the odd indices. A run list with none is an
        // all-background mask, which is no region: a `Region` always holds at
        // least one pixel, so `centroid` and `intersection_over_union` divide by a
        // positive area. Reject it here so no construction path — `from_dense`
        // aside, which returns `None` — can mint an empty one.
        let foreground: u64 = runs
            .iter()
            .skip(1)
            .step_by(2)
            .map(|&run| u64::from(run))
            .sum();
        if foreground == 0 {
            return Err(RegionError::NoForeground);
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for Region {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // The wire form is the grid and the runs; the grid self-validates on
        // the way in and `new` re-imposes the canonical run invariant, so a
        // corrupted fact fails at the boundary rather than reaching the interior.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            dimensions: Dimensions,
            runs: Vec<u32>,
        }
        let wire = Wire::deserialize(deserializer)?;
        Region::new(wire.dimensions, wire.runs).map_err(serde::de::Error::custom)
    }
}

/// Why a [`Region`] could not be assembled.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum RegionError {
    /// A grid dimension was zero, so the mask has no pixels to describe.
    #[error("region grid must be non-zero, got {width}x{height}")]
    ZeroDimension { width: u32, height: u32 },
    /// A grid dimension exceeded [`MAX_REGION_DIM`].
    #[error("region grid {width}x{height} exceeds the {max}px per-side limit")]
    DimensionTooLarge { width: u32, height: u32, max: u32 },
    /// A dense mask's length disagreed with its declared grid.
    #[error("dense mask has {len} pixels, not the {dimensions} the grid declares")]
    LengthMismatch { dimensions: Dimensions, len: usize },
    /// The run list was empty; a non-zero grid has at least one run.
    #[error("region run list is empty")]
    EmptyRuns,
    /// A run past the first was zero, which would alias another encoding.
    #[error("region run at index {index} is zero; only the leading run may be")]
    NonCanonicalZero { index: usize },
    /// The runs did not sum to the grid's pixel count.
    #[error("region runs sum to {sum}, not the {area} the grid declares")]
    RunSumMismatch { sum: u64, area: u64 },
    /// The run list has no foreground, so it is an empty mask, which is not a
    /// region.
    #[error("region has no foreground pixels")]
    NoForeground,
    /// A pixel index slice given to [`Region::from_pixels`] was not strictly
    /// ascending, so it does not name a canonical run list.
    #[error(
        "region pixel {pixel} does not exceed the previous {previous}; indices must strictly ascend"
    )]
    PixelsNotAscending { pixel: usize, previous: usize },
    /// A pixel index given to [`Region::from_pixels`] lay outside the grid.
    #[error("region pixel {pixel} is outside the {pixel_count}-pixel grid")]
    PixelOutOfRange { pixel: usize, pixel_count: u32 },
}

/// Two [`Region`]s compared across different grids, where `IoU` is undefined.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("region grids differ: {a} vs {b}")]
pub struct GridMismatch {
    /// One mask's grid.
    pub a: Dimensions,
    /// The other mask's grid.
    pub b: Dimensions,
}

/// One coordinate component in `0.0..=1.0` proportional image space — the
/// shared building block of [`ProportionalRect`] and [`ProportionalPoint`].
///
/// The inner value is a `Finite`, which rejects `NaN`/`±∞` and normalizes
/// `-0.0` to `+0.0`. That is what lets `Eq`, `Hash`, and `Ord` derive and stay
/// consistent with `PartialEq` (under which `-0.0 == +0.0`), so every type built
/// from coordinates derives its own `Eq`/`Hash`/`Ord` through this one. The
/// smart constructor [`ProportionalCoord::new`] adds the `0.0..=1.0` range check.
///
/// `#[serde(transparent)]` keeps the wire form a bare number: a coordinate is
/// indistinguishable from the `f64` it wraps. Deserialization routes that number
/// through [`ProportionalCoord::new`], so a non-finite or out-of-range
/// coordinate fails at the wire boundary, and every type built from coordinates
/// inherits that guard by deserializing its fields as `ProportionalCoord`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(transparent)]
#[schemars(transparent)]
pub struct ProportionalCoord(Finite);

impl<'de> Deserialize<'de> for ProportionalCoord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = f64::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl ProportionalCoord {
    /// Construct a proportional coordinate. The value must be finite and in
    /// `0.0..=1.0`; `-0.0` is normalized to `+0.0`.
    pub fn new(value: f64) -> Result<Self, ProportionalCoordError> {
        let finite = Finite::new(value).ok_or(ProportionalCoordError::NotFinite { value })?;
        if !(0.0..=1.0).contains(&value) {
            return Err(ProportionalCoordError::OutOfBounds { value });
        }
        Ok(Self(finite))
    }

    /// Mint a coordinate from trusted arithmetic already known finite and in
    /// `0.0..=1.0` — a ratio of a foreground count to a grid extent. Mirrors
    /// [`Finite::new_unchecked`]; the `debug_assert` catches a broken promise in
    /// dev. Module-private, so external construction still validates through
    /// [`new`](Self::new).
    fn new_unchecked(value: f64) -> Self {
        debug_assert!(
            (0.0..=1.0).contains(&value),
            "ProportionalCoord::new_unchecked outside 0.0..=1.0"
        );
        Self(Finite::new_unchecked(value))
    }

    /// The coordinate value, in `0.0..=1.0`.
    pub fn get(self) -> f64 {
        self.0.get()
    }
}

/// Errors from [`ProportionalCoord::new`].
///
/// `PartialEq` only — the variants carry the pre-validation `f64`, which may be
/// `NaN`, so a total ordering would be dishonest, and errors aren't part of the
/// content-addressed fact graph.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ProportionalCoordError {
    /// The coordinate was not finite (`NaN` or infinite).
    #[error("proportional coordinate must be finite, got {value}")]
    NotFinite { value: f64 },
    /// The coordinate lay outside the `0.0..=1.0` unit range.
    #[error("proportional coordinate must lie in 0.0..=1.0, got {value}")]
    OutOfBounds { value: f64 },
}

/// An axis-aligned proportional rectangle in `0.0..=1.0` coordinates, given by
/// two corner points in canonical order (`min.x <= max.x`, `min.y <= max.y`).
///
/// The shared rect payload of [`ImageGeometry::BBox`] and
/// [`crate::grammar::composites::SubimageRegion::Rect`].
/// [`ProportionalRect::from_corners`] takes two corners in any order and
/// normalizes them; coincident corners give a zero-area rect, the honest shape
/// for a VLM pointing at a small feature. Both corners already lie in `[0, 1]`,
/// so the rect is always in-bounds and assembly itself can't fail.
///
/// Both corners are [`ProportionalPoint`], so `Eq`/`Hash`/`Ord` derive through
/// their negative-zero-normalizing, NaN-free guarantee. Derived `Ord` compares
/// `min` then `max`; that order feeds content-address determinism, so changing
/// it changes every `CommitId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
pub struct ProportionalRect {
    min: ProportionalPoint,
    max: ProportionalPoint,
}

impl ProportionalRect {
    /// Assemble a rect from two corner points in any order, normalizing per
    /// axis to canonical order (`min.x <= max.x`, `min.y <= max.y`). Coincident
    /// corners yield a zero-area rect. Infallible — both corners are already
    /// valid in-range points.
    pub fn from_corners(a: ProportionalPoint, b: ProportionalPoint) -> Self {
        Self {
            min: ProportionalPoint {
                x: a.x.min(b.x),
                y: a.y.min(b.y),
            },
            max: ProportionalPoint {
                x: a.x.max(b.x),
                y: a.y.max(b.y),
            },
        }
    }

    /// The whole image: the unit rect from `(0, 0)` to `(1, 1)`. Infallible,
    /// since the unit square's corners are known valid, so it is the natural way
    /// to name a subimage that is the entire frame.
    pub fn full() -> Self {
        let zero = ProportionalCoord(Finite::new_unchecked(0.0));
        let one = ProportionalCoord(Finite::new_unchecked(1.0));
        Self {
            min: ProportionalPoint { x: zero, y: zero },
            max: ProportionalPoint { x: one, y: one },
        }
    }

    /// Construct a rect from two corners given as raw floats, in any order.
    /// Each coordinate routes through [`ProportionalCoord::new`] for the
    /// finite + range check and the `-0.0` normalization, so an invalid
    /// coordinate is the only failure mode; corner assembly cannot fail.
    pub fn new(ax: f64, ay: f64, bx: f64, by: f64) -> Result<Self, ProportionalCoordError> {
        let a = ProportionalPoint::new(ax, ay)?;
        let b = ProportionalPoint::new(bx, by)?;
        Ok(Self::from_corners(a, b))
    }

    /// The corner with the smaller coordinates.
    pub fn min_corner(&self) -> ProportionalPoint {
        self.min
    }

    /// The corner with the larger coordinates.
    pub fn max_corner(&self) -> ProportionalPoint {
        self.max
    }

    /// The smaller x coordinate, in `0.0..=1.0`.
    pub fn x(&self) -> f64 {
        self.min.x()
    }

    /// The smaller y coordinate, in `0.0..=1.0`.
    pub fn y(&self) -> f64 {
        self.min.y()
    }

    /// Width in proportional units; zero for a degenerate rect.
    pub fn width(&self) -> f64 {
        self.max.x() - self.min.x()
    }

    /// Height in proportional units; zero for a degenerate rect.
    pub fn height(&self) -> f64 {
        self.max.y() - self.min.y()
    }
}

impl<'de> Deserialize<'de> for ProportionalRect {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Each corner validates through `ProportionalPoint`/`ProportionalCoord`
        // as it materializes; `from_corners` then normalizes the axis order.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            min: ProportionalPoint,
            max: ProportionalPoint,
        }
        let w = Wire::deserialize(deserializer)?;
        Ok(Self::from_corners(w.min, w.max))
    }
}

/// A point in `0.0..=1.0` proportional image coordinates — the vertex type of
/// [`ProportionalPolyline`].
///
/// Both components are [`ProportionalCoord`], so `Eq`/`Hash`/`Ord` derive
/// through its negative-zero-normalizing, NaN-free guarantee, and `Deserialize`
/// derives each field through the coordinate's wire-boundary guard.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct ProportionalPoint {
    x: ProportionalCoord,
    y: ProportionalCoord,
}

impl ProportionalPoint {
    /// Construct a proportional point. Both components must be finite and in
    /// `0.0..=1.0`; `-0.0` is normalized to `+0.0`.
    pub fn new(x: f64, y: f64) -> Result<Self, ProportionalCoordError> {
        Ok(Self {
            x: ProportionalCoord::new(x)?,
            y: ProportionalCoord::new(y)?,
        })
    }

    /// Horizontal coordinate in `0.0..=1.0`.
    pub fn x(&self) -> f64 {
        self.x.get()
    }

    /// Vertical coordinate in `0.0..=1.0`.
    pub fn y(&self) -> f64 {
        self.y.get()
    }
}

/// Upper bound on a [`ProportionalPolyline`]'s vertex count. Legitimate road
/// and boundary traces run to many vertices, so the cap is generous; it bounds a
/// stored trace's size and the cost of per-vertex validation, the same way
/// [`MAX_LOCATION_CIRCLES`] caps an already-deserialized location. Wire-size is
/// bounded upstream by the HTTP body limit, not by this cap.
///
/// [`MAX_LOCATION_CIRCLES`]: crate::submit::pipeline::MAX_LOCATION_CIRCLES
pub const MAX_POLYLINE_POINTS: usize = 10_000;

/// A polyline traced in `0.0..=1.0` proportional image coordinates — a path on
/// the image's own frame, such as a road or boundary traced across a map sheet.
///
/// The smart constructor [`ProportionalPolyline::new`] requires between two and
/// [`MAX_POLYLINE_POINTS`] points, each finite and within the unit square.
/// Deserialization validates each vertex through [`ProportionalCoord`] and
/// enforces the same count bounds, so a too-short, over-long, or out-of-range
/// trace fails at the wire boundary. `Eq`/`Hash`/`Ord` derive through
/// [`ProportionalPoint`], and so through [`ProportionalCoord`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
pub struct ProportionalPolyline {
    points: Vec<ProportionalPoint>,
}

impl ProportionalPolyline {
    /// Construct a proportional polyline from raw `(x, y)` pairs. Requires
    /// between two and [`MAX_POLYLINE_POINTS`] points, each finite and in
    /// `0.0..=1.0`.
    pub fn new(points: Vec<(f64, f64)>) -> Result<Self, ProportionalPolylineError> {
        if points.len() < 2 {
            return Err(ProportionalPolylineError::TooFewPoints {
                count: points.len(),
            });
        }
        if points.len() > MAX_POLYLINE_POINTS {
            return Err(ProportionalPolylineError::TooManyPoints {
                count: points.len(),
                limit: MAX_POLYLINE_POINTS,
            });
        }
        let points = points
            .into_iter()
            .map(|(x, y)| ProportionalPoint::new(x, y))
            .collect::<Result<Vec<_>, _>>()
            .map_err(ProportionalPolylineError::Coord)?;
        Ok(Self { points })
    }

    /// The polyline's vertices in order; always at least two.
    pub fn points(&self) -> &[ProportionalPoint] {
        &self.points
    }
}

impl<'de> Deserialize<'de> for ProportionalPolyline {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Each vertex validates through `ProportionalPoint`/`ProportionalCoord`
        // as the `Vec` materializes; only the count bounds remain to enforce.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            points: Vec<ProportionalPoint>,
        }
        let Wire { points } = Wire::deserialize(deserializer)?;
        let count = points.len();
        if count < 2 {
            return Err(serde::de::Error::custom(
                ProportionalPolylineError::TooFewPoints { count },
            ));
        }
        if count > MAX_POLYLINE_POINTS {
            return Err(serde::de::Error::custom(
                ProportionalPolylineError::TooManyPoints {
                    count,
                    limit: MAX_POLYLINE_POINTS,
                },
            ));
        }
        Ok(Self { points })
    }
}

/// Errors from [`ProportionalPolyline::new`].
///
/// `PartialEq` only — a wrapped [`ProportionalCoordError`] carries a
/// pre-validation `f64` that may be `NaN`, so a total ordering would be
/// dishonest, and errors aren't part of the content-addressed fact graph.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ProportionalPolylineError {
    /// A vertex coordinate was invalid (non-finite or out of range).
    #[error("proportional polyline vertex invalid: {0}")]
    Coord(#[source] ProportionalCoordError),
    /// The trace named fewer than two points.
    #[error("proportional polyline needs at least two points, got {count}")]
    TooFewPoints { count: usize },
    /// The trace named more than [`MAX_POLYLINE_POINTS`] points.
    #[error("proportional polyline exceeds the {limit}-point limit, got {count}")]
    TooManyPoints { count: usize, limit: usize },
}

/// Image-space localization geometry for a depiction — where in the image's own
/// pixel / proportional frame an entity sits.
///
/// A pixel-precise [`Region`] mask, an axis-aligned [`ProportionalRect`] bbox, or
/// a [`ProportionalPolyline`] trace. Masks and bboxes suit areal features, the
/// polyline a linear feature traced across the sheet. All are anchored to the
/// image's own frame; a geographic reading is a property of the image's
/// projection, derived downstream.
///
/// Construct a bbox via [`ImageGeometry::bbox`]; deserialization routes the
/// bbox and polyline payloads through their validating wire boundaries, so
/// out-of-range coordinates fail at parse time.
#[grammar_type]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ImageGeometry {
    /// Pixel-precise binary mask over the image's own pixel grid.
    Region {
        /// The run-length-encoded region mask.
        region: Region,
    },
    /// Axis-aligned proportional bounding box in `0.0..=1.0` coordinates.
    /// Resolution-independent — survives rescans and downsamples of the image.
    #[serde(rename = "bbox")]
    BBox {
        /// The proportional bounding rectangle.
        rect: ProportionalRect,
    },
    /// Proportional polyline trace, for roads, paths, or boundaries.
    Polyline {
        /// The proportional polyline trace.
        polyline: ProportionalPolyline,
    },
}

impl ImageGeometry {
    /// Construct an axis-aligned proportional bbox from two corners given as
    /// raw floats, in any order. See [`ProportionalRect::new`] for the
    /// coordinate validation.
    pub fn bbox(ax: f64, ay: f64, bx: f64, by: f64) -> Result<Self, ProportionalCoordError> {
        Ok(Self::BBox {
            rect: ProportionalRect::new(ax, ay, bx, by)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use super::*;
    use proptest::prelude::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn arb_dense() -> impl Strategy<Value = (Dimensions, Vec<bool>)> {
        (1u32..=64, 1u32..=64)
            .prop_flat_map(|(width, height)| {
                let count = (width * height) as usize;
                (
                    Just(width),
                    Just(height),
                    prop::collection::vec(any::<bool>(), count),
                )
            })
            .prop_filter_map("grid within bounds", |(width, height, pixels)| {
                Dimensions::new(width, height)
                    .ok()
                    .map(|grid| (grid, pixels))
            })
    }

    fn arb_two_masks() -> impl Strategy<Value = (Dimensions, Vec<bool>, Vec<bool>)> {
        (1u32..=64, 1u32..=64)
            .prop_flat_map(|(width, height)| {
                let count = (width * height) as usize;
                (
                    Just(width),
                    Just(height),
                    prop::collection::vec(any::<bool>(), count),
                    prop::collection::vec(any::<bool>(), count),
                )
            })
            .prop_filter_map("grid within bounds", |(width, height, a, b)| {
                Dimensions::new(width, height).ok().map(|grid| (grid, a, b))
            })
    }

    #[test]
    fn region_from_dense_round_trips_every_opening() -> TestResult {
        // Background-start, foreground-start (the leading-zero case), all
        // background, all foreground: each opening the run list must represent.
        let grid = Dimensions::new(2, 2)?;
        for pixels in [
            vec![false, true, true, false],
            vec![true, false, false, false],
            vec![false, false, false, false],
            vec![true, true, true, true],
        ] {
            match Region::from_dense(grid, &pixels)? {
                Some(region) => assert_eq!(region.to_dense(), pixels),
                None => assert!(
                    pixels.iter().all(|&pixel| !pixel),
                    "from_dense is None only for an all-background mask"
                ),
            }
        }
        Ok(())
    }

    #[test]
    fn region_rejects_interior_zero_run() -> TestResult {
        // `[1, 0, 3]` denotes the same 2x2 mask as `[4]`; only one may exist.
        assert!(matches!(
            Region::new(Dimensions::new(2, 2)?, vec![1, 0, 3]),
            Err(RegionError::NonCanonicalZero { index: 1 })
        ));
        Ok(())
    }

    #[test]
    fn region_rejects_run_sum_mismatch() -> TestResult {
        assert!(matches!(
            Region::new(Dimensions::new(2, 2)?, vec![1, 1]),
            Err(RegionError::RunSumMismatch { sum: 2, area: 4 })
        ));
        Ok(())
    }

    #[test]
    fn region_rejects_an_all_background_run_list() -> TestResult {
        // A single background run over the whole grid has no foreground: an empty
        // mask, which is not a region. `Deserialize` routes through `new`, so the
        // wire boundary rejects it too, keeping every `Region` non-empty for
        // `centroid` and `intersection_over_union`.
        assert!(matches!(
            Region::new(Dimensions::new(2, 2)?, vec![4]),
            Err(RegionError::NoForeground)
        ));
        Ok(())
    }

    #[test]
    fn dimensions_reject_degenerate_and_oversized_grids() {
        assert!(matches!(
            Dimensions::new(0, 4),
            Err(RegionError::ZeroDimension { .. })
        ));
        assert!(matches!(
            Dimensions::new(MAX_REGION_DIM + 1, 1),
            Err(RegionError::DimensionTooLarge { .. })
        ));
    }

    #[test]
    fn region_from_dense_rejects_length_mismatch() -> TestResult {
        assert!(matches!(
            Region::from_dense(Dimensions::new(2, 2)?, &[true, false]),
            Err(RegionError::LengthMismatch { len: 2, .. })
        ));
        Ok(())
    }

    #[test]
    fn region_iou_scores_overlap() -> TestResult {
        let grid = Dimensions::new(2, 2)?;
        let full = Region::from_dense(grid, &[true, true, true, true])?.ok_or("non-empty")?;
        let left = Region::from_dense(grid, &[true, false, true, false])?.ok_or("non-empty")?;
        let right = Region::from_dense(grid, &[false, true, false, true])?.ok_or("non-empty")?;
        // An all-background mask is not a region.
        assert!(Region::from_dense(grid, &[false, false, false, false])?.is_none());

        assert_eq!(full.intersection_over_union(&full)?, 1.0);
        assert_eq!(left.intersection_over_union(&right)?, 0.0);
        // left is 2 of full's 4 pixels: intersection 2, union 4.
        assert_eq!(left.intersection_over_union(&full)?, 0.5);
        Ok(())
    }

    #[test]
    fn region_iou_rejects_grid_mismatch() -> TestResult {
        let a = Region::from_dense(Dimensions::new(2, 2)?, &[true, true, true, true])?
            .ok_or("non-empty")?;
        let b = Region::from_dense(Dimensions::new(1, 4)?, &[true, true, true, true])?
            .ok_or("non-empty")?;
        assert!(matches!(
            a.intersection_over_union(&b),
            Err(GridMismatch { .. })
        ));
        Ok(())
    }

    #[test]
    fn region_bounding_rect_is_tight() -> TestResult {
        // One foreground pixel at row 1, col 1 of a 4x4 grid: the tight box is
        // that single quarter-by-quarter cell.
        let grid = Dimensions::new(4, 4)?;
        let mut pixels = vec![false; 16];
        pixels[5] = true;
        let region = Region::from_dense(grid, &pixels)?.ok_or("non-empty")?;
        let rect = region.bounding_rect().ok_or("expected a box")?;
        assert_eq!((rect.x(), rect.y()), (0.25, 0.25));
        assert_eq!((rect.width(), rect.height()), (0.25, 0.25));
        Ok(())
    }

    #[test]
    fn region_centroid_is_the_foreground_mean() -> TestResult {
        // Two pixels: (col 0, row 0) and (col 2, row 1) on a 4x2 grid. Mean pixel
        // is (col 1, row 0.5); proportional over the 4x2 grid, (0.25, 0.25).
        let grid = Dimensions::new(4, 2)?;
        let mut pixels = vec![false; 8];
        pixels[0] = true;
        pixels[6] = true;
        let region = Region::from_dense(grid, &pixels)?.ok_or("non-empty")?;
        let centroid = region.centroid();
        assert_eq!((centroid.x(), centroid.y()), (0.25, 0.25));
        Ok(())
    }

    #[test]
    fn region_foreground_pixels_lists_only_foreground_indices() -> TestResult {
        // 3x2 grid, foreground at linear indices 1, 2, 4.
        let grid = Dimensions::new(3, 2)?;
        let region = Region::from_dense(grid, &[false, true, true, false, true, false])?
            .ok_or("non-empty")?;
        assert_eq!(
            region.foreground_pixels().collect::<Vec<_>>(),
            vec![1, 2, 4]
        );
        Ok(())
    }

    #[test]
    fn region_foreground_row_runs_split_stored_runs_at_row_ends() -> TestResult {
        // 4x4 grid, foreground at linear 2..11 and 13: one stored run wrapping
        // from row 0 through a full row 1 into row 2, then a lone pixel.
        let wrapping = Region::from_pixels(
            Dimensions::new(4, 4)?,
            &(2..11).chain([13]).collect::<Vec<_>>(),
        )?
        .ok_or("non-empty")?;
        assert_eq!(
            wrapping.foreground_row_runs().collect::<Vec<_>>(),
            vec![(0, 2..4), (1, 0..4), (2, 0..3), (3, 1..2)]
        );

        // Opening foreground at pixel 0 (a leading zero-length background run),
        // with two runs sharing row 0.
        let opening = Region::from_dense(
            Dimensions::new(3, 2)?,
            &[true, false, true, true, true, false],
        )?
        .ok_or("non-empty")?;
        assert_eq!(
            opening.foreground_row_runs().collect::<Vec<_>>(),
            vec![(0, 0..1), (0, 2..3), (1, 0..2)]
        );

        // The full frame is one stored run: one full-width run per row.
        let full = Region::from_dense(Dimensions::new(3, 2)?, &[true; 6])?.ok_or("non-empty")?;
        assert_eq!(
            full.foreground_row_runs().collect::<Vec<_>>(),
            vec![(0, 0..3), (1, 0..3)]
        );
        Ok(())
    }

    #[test]
    fn region_from_pixels_matches_from_dense_on_a_holey_set() -> TestResult {
        // Foreground at 1, 2, 4 on a 6-pixel grid: an interior gap at 3 and a
        // trailing gap at 5. The run form must be the exact one `from_dense`
        // builds from the equivalent dense mask.
        let grid = Dimensions::new(6, 1)?;
        let dense = vec![false, true, true, false, true, false];
        let from_pixels = Region::from_pixels(grid, &[1, 2, 4])?.ok_or("non-empty")?;
        assert_eq!(from_pixels.to_dense(), dense);
        assert_eq!(
            from_pixels,
            Region::from_dense(grid, &dense)?.ok_or("non-empty")?
        );
        Ok(())
    }

    #[test]
    fn region_from_pixels_round_trips_leading_and_trailing_edges() -> TestResult {
        let grid = Dimensions::new(6, 1)?;
        // Pixel 0 foreground: the leading background run is zero-length, the one
        // place a zero run is canonical.
        let leading = vec![true, true, false, false, false, false];
        assert_eq!(
            Region::from_pixels(grid, &[0, 1])?.ok_or("non-empty")?,
            Region::from_dense(grid, &leading)?.ok_or("non-empty")?
        );
        // Foreground ends at the last pixel: no trailing background run.
        let trailing = vec![false, false, false, false, true, true];
        assert_eq!(
            Region::from_pixels(grid, &[4, 5])?.ok_or("non-empty")?,
            Region::from_dense(grid, &trailing)?.ok_or("non-empty")?
        );
        Ok(())
    }

    #[test]
    fn region_from_pixels_empty_is_none() -> TestResult {
        assert!(Region::from_pixels(Dimensions::new(6, 1)?, &[])?.is_none());
        Ok(())
    }

    #[test]
    fn region_from_pixels_rejects_malformed_indices() -> TestResult {
        let grid = Dimensions::new(6, 1)?;
        assert!(matches!(
            Region::from_pixels(grid, &[2, 1]),
            Err(RegionError::PixelsNotAscending {
                pixel: 1,
                previous: 2
            })
        ));
        assert!(matches!(
            Region::from_pixels(grid, &[1, 1]),
            Err(RegionError::PixelsNotAscending {
                pixel: 1,
                previous: 1
            })
        ));
        assert!(matches!(
            Region::from_pixels(grid, &[10]),
            Err(RegionError::PixelOutOfRange {
                pixel: 10,
                pixel_count: 6
            })
        ));
        Ok(())
    }

    proptest! {
        /// Encoding a dense mask then decoding it returns the original, for any
        /// grid and content: the run form is a total, faithful codec.
        #[test]
        fn region_dense_round_trips((grid, pixels) in arb_dense()) {
            match Region::from_dense(grid, &pixels).map_err(|e| TestCaseError::fail(e.to_string()))? {
                Some(region) => prop_assert_eq!(region.to_dense(), pixels),
                None => prop_assert!(pixels.iter().all(|&pixel| !pixel)),
            }
        }

        /// IoU is symmetric, lands in `[0, 1]`, and scores a mask against itself
        /// as exactly 1.
        #[test]
        fn region_iou_symmetric_and_unit_ranged(
            (grid, a_bits, b_bits) in arb_two_masks(),
        ) {
            // A region is never empty; an all-background sample has no region and
            // no IoU to check, so skip it.
            let Some(a) = Region::from_dense(grid, &a_bits)
                .map_err(|e| TestCaseError::fail(e.to_string()))?
            else {
                return Ok(());
            };
            let Some(b) = Region::from_dense(grid, &b_bits)
                .map_err(|e| TestCaseError::fail(e.to_string()))?
            else {
                return Ok(());
            };
            let ab = a
                .intersection_over_union(&b)
                .map_err(|e| TestCaseError::fail(e.to_string()))?;
            let ba = b
                .intersection_over_union(&a)
                .map_err(|e| TestCaseError::fail(e.to_string()))?;
            prop_assert_eq!(ab, ba);
            prop_assert!((0.0..=1.0).contains(&ab));
            let aa = a
                .intersection_over_union(&a)
                .map_err(|e| TestCaseError::fail(e.to_string()))?;
            prop_assert_eq!(aa, 1.0);
        }

        /// IoU agrees with a brute-force count over the dense masks. The fast
        /// path merges run intervals; the oracle scans pixels. Any bug in the
        /// merge (a wrong advance, an off-by-one on `[start, end)`) diverges here.
        #[test]
        fn region_iou_matches_dense_oracle(
            (grid, a_bits, b_bits) in arb_two_masks(),
        ) {
            // A region is never empty; skip an all-background sample.
            let Some(a) = Region::from_dense(grid, &a_bits)
                .map_err(|e| TestCaseError::fail(e.to_string()))?
            else {
                return Ok(());
            };
            let Some(b) = Region::from_dense(grid, &b_bits)
                .map_err(|e| TestCaseError::fail(e.to_string()))?
            else {
                return Ok(());
            };
            let intersection = a_bits.iter().zip(&b_bits).filter(|(x, y)| **x && **y).count();
            // Both masks are non-empty, so the union is positive.
            let union = a_bits.iter().zip(&b_bits).filter(|(x, y)| **x || **y).count();
            let expected = intersection as f64 / union as f64;
            let actual = a
                .intersection_over_union(&b)
                .map_err(|e| TestCaseError::fail(e.to_string()))?;
            prop_assert_eq!(actual, expected);
        }
    }

    // Coordinate values that keep `-0.0` reachable: a plain `0.0..` float
    // strategy almost never samples exactly `-0.0`, so the bug this guards
    // against (negative-zero inconsistency between `PartialEq` and `Hash`/`Ord`)
    // would slip past. Mixing the two zero forms in explicitly forces the laws
    // to exercise them.
    fn arb_coord() -> impl Strategy<Value = f64> {
        prop_oneof![
            4 => 0.0f64..0.5,
            1 => Just(-0.0f64),
            1 => Just(0.0f64),
        ]
    }

    fn arb_rect() -> impl Strategy<Value = ProportionalRect> {
        (arb_coord(), arb_coord(), arb_coord(), arb_coord()).prop_filter_map(
            "corner coordinates lie in the unit square",
            |(ax, ay, bx, by)| ProportionalRect::new(ax, ay, bx, by).ok(),
        )
    }

    fn hash_of<T: std::hash::Hash>(value: &T) -> u64 {
        use std::hash::Hasher;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }

    proptest! {
        /// `ProportionalCoord` folds `-0.0` into `+0.0` and rejects NaN/Inf, so
        /// its `Hash`/`Ord` must stay consistent with `PartialEq`. Every type
        /// built from coordinates derives through this, so the law protects
        /// `ProportionalRect`, `ProportionalPoint`, and `ProportionalPolyline`.
        #[test]
        fn proportional_coord_eq_hash_ord_consistent_with_negative_zero(
            a in arb_coord(),
            b in arb_coord(),
        ) {
            let ca = ProportionalCoord::new(a).map_err(|e| TestCaseError::fail(e.to_string()))?;
            let cb = ProportionalCoord::new(b).map_err(|e| TestCaseError::fail(e.to_string()))?;
            if ca == cb {
                prop_assert_eq!(hash_of(&ca), hash_of(&cb), "equal coords must hash equally");
                prop_assert_eq!(ca.cmp(&cb), Ordering::Equal, "equal coords must order Equal");
            }
            let neg = ProportionalCoord::new(-0.0).map_err(|e| TestCaseError::fail(e.to_string()))?;
            let pos = ProportionalCoord::new(0.0).map_err(|e| TestCaseError::fail(e.to_string()))?;
            prop_assert_eq!(neg, pos, "negative and positive zero must be equal");
            prop_assert_eq!(hash_of(&neg), hash_of(&pos), "equal coords must hash equally");
            prop_assert_eq!(neg.cmp(&pos), Ordering::Equal, "equal coords must order Equal");
        }

        #[test]
        fn proportional_rect_eq_hash_ord_consistent_with_negative_zero(
            a in arb_rect(),
            b in arb_rect(),
        ) {
            // Whenever two rects compare equal, `Hash` and `Ord` must agree —
            // the exact contract negative zero would otherwise break (`PartialEq`
            // folds `-0.0 == +0.0`, but `to_bits`/`total_cmp` distinguish them).
            if a == b {
                prop_assert_eq!(hash_of(&a), hash_of(&b), "equal rects must hash equally");
                prop_assert_eq!(a.cmp(&b), Ordering::Equal, "equal rects must order Equal");
            }

            // `-0.0` can enter any corner; swapping it for `+0.0` in the min
            // corner must be invisible across Eq, Hash, and Ord.
            let neg = ProportionalRect::new(-0.0, -0.0, a.max_corner().x(), a.max_corner().y())
                .map_err(|e| TestCaseError::fail(e.to_string()))?;
            let pos = ProportionalRect::new(0.0, 0.0, a.max_corner().x(), a.max_corner().y())
                .map_err(|e| TestCaseError::fail(e.to_string()))?;
            prop_assert_eq!(neg, pos, "negative and positive zero corners must be equal");
            prop_assert_eq!(hash_of(&neg), hash_of(&pos), "equal rects must hash equally");
            prop_assert_eq!(neg.cmp(&pos), Ordering::Equal, "equal rects must order Equal");
        }
    }

    #[test]
    fn proportional_rect_normalizes_corner_order() -> TestResult {
        // Corners in any order collapse to one canonical rect, so input order is
        // not a construction failure point and equal regions compare equal.
        let canonical = ProportionalRect::new(0.1, 0.2, 0.3, 0.4)?;
        let reversed = ProportionalRect::new(0.3, 0.4, 0.1, 0.2)?;
        let mixed = ProportionalRect::new(0.3, 0.2, 0.1, 0.4)?;
        assert_eq!(canonical, reversed);
        assert_eq!(canonical, mixed);
        assert_eq!(canonical.min_corner(), ProportionalPoint::new(0.1, 0.2)?);
        assert_eq!(canonical.max_corner(), ProportionalPoint::new(0.3, 0.4)?);
        Ok(())
    }

    #[test]
    fn proportional_rect_allows_zero_area() -> TestResult {
        // Coincident corners — a VLM pointing at a small feature — are a valid
        // degenerate rect, not an error.
        let rect = ProportionalRect::new(0.5, 0.5, 0.5, 0.5)?;
        assert_eq!(rect.min_corner(), rect.max_corner());
        Ok(())
    }

    #[test]
    fn proportional_rect_rejects_out_of_range_corner() {
        assert!(matches!(
            ProportionalRect::new(0.0, 0.0, 1.5, 0.5),
            Err(ProportionalCoordError::OutOfBounds { .. })
        ));
    }

    #[test]
    fn full_is_the_validated_unit_rect() -> TestResult {
        // `full` mints the unit square unchecked; this pins it to the value the
        // validating constructor produces, catching a swapped or mistyped corner.
        assert_eq!(
            ProportionalRect::full(),
            ProportionalRect::new(0.0, 0.0, 1.0, 1.0)?
        );
        Ok(())
    }

    #[test]
    fn image_geometry_bbox_rejects_nan() {
        assert!(matches!(
            ImageGeometry::bbox(f64::NAN, 0.0, 0.5, 0.5),
            Err(ProportionalCoordError::NotFinite { .. })
        ));
    }

    #[test]
    fn image_geometry_deserialize_validates_bbox() {
        // The out-of-range corner must fail at the `ProportionalRect` wire
        // boundary, nested under the `bbox` tag and the `rect` field.
        let result: Result<ImageGeometry, _> = serde_json::from_str(
            r#"{"type":"bbox","rect":{"min":{"x":0.6,"y":0.0},"max":{"x":1.5,"y":0.5}}}"#,
        );
        assert!(
            result.is_err(),
            "out-of-range bbox must fail at deserialize"
        );
    }

    #[test]
    fn image_geometry_bbox_round_trips_preserving_corners() -> TestResult {
        // Catches a manual-Deserialize Wire → from_corners wiring bug that
        // dropped or swapped a corner coordinate.
        let geometry = ImageGeometry::bbox(0.1, 0.2, 0.3, 0.4)?;
        let json = serde_json::to_string(&geometry)?;
        let parsed: ImageGeometry = serde_json::from_str(&json)?;
        assert_eq!(parsed, geometry);
        Ok(())
    }

    #[test]
    fn proportional_point_rejects_nan() {
        assert!(matches!(
            ProportionalPoint::new(f64::NAN, 0.5),
            Err(ProportionalCoordError::NotFinite { .. })
        ));
    }

    #[test]
    fn proportional_polyline_rejects_single_point() {
        assert!(matches!(
            ProportionalPolyline::new(vec![(0.1, 0.1)]),
            Err(ProportionalPolylineError::TooFewPoints { count: 1 })
        ));
    }

    #[test]
    fn proportional_polyline_rejects_too_many_points() {
        let points = vec![(0.5f64, 0.5f64); MAX_POLYLINE_POINTS + 1];
        assert!(matches!(
            ProportionalPolyline::new(points),
            Err(ProportionalPolylineError::TooManyPoints { count, limit })
                if count == MAX_POLYLINE_POINTS + 1 && limit == MAX_POLYLINE_POINTS
        ));
    }

    #[test]
    fn proportional_polyline_rejects_non_finite_vertex() {
        assert!(matches!(
            ProportionalPolyline::new(vec![(0.1, 0.1), (f64::INFINITY, 0.2)]),
            Err(ProportionalPolylineError::Coord(
                ProportionalCoordError::NotFinite { .. }
            ))
        ));
    }

    #[test]
    fn proportional_polyline_deserialize_validates_too_few_points() {
        let result: Result<ProportionalPolyline, _> =
            serde_json::from_str(r#"{"points":[{"x":0.1,"y":0.1}]}"#);
        assert!(
            result.is_err(),
            "single-point trace must fail at deserialize"
        );
    }

    #[test]
    fn proportional_polyline_deserialize_validates_out_of_range() {
        let result: Result<ProportionalPolyline, _> =
            serde_json::from_str(r#"{"points":[{"x":0.0,"y":0.0},{"x":1.5,"y":0.2}]}"#);
        assert!(
            result.is_err(),
            "out-of-range vertex must fail at the wire boundary"
        );
    }

    #[test]
    fn proportional_polyline_deserialize_validates_too_many_points() {
        let points = vec![r#"{"x":0.5,"y":0.5}"#; MAX_POLYLINE_POINTS + 1].join(",");
        let json = format!("{{\"points\":[{points}]}}");
        let result: Result<ProportionalPolyline, _> = serde_json::from_str(&json);
        assert!(
            result.is_err(),
            "over-long trace must fail at the wire boundary"
        );
    }

    #[test]
    fn proportional_polyline_round_trips_preserving_vertices() -> TestResult {
        // Catches a derived-Deserialize bug that dropped, reordered, or swapped
        // the x/y of any vertex.
        let trace = ProportionalPolyline::new(vec![(0.1, 0.2), (0.3, 0.4), (0.5, 0.6)])?;
        let json = serde_json::to_string(&trace)?;
        let parsed: ProportionalPolyline = serde_json::from_str(&json)?;
        assert_eq!(parsed, trace);
        Ok(())
    }
}
