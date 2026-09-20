//! Each patch's weight in a region's pool, by exact quadrature.
//!
//! The patch grid is read as a continuous field: bilinear between patch
//! centers, extended flat past the outermost centers, the way
//! `align_corners=False` upsampling reads it. A region's embedding is the mean
//! of that field over the mask, so a patch's weight is the integral of its
//! basis function over the mask. Integrating over the mask's area, rather than
//! sampling at pixel centers, keeps the weights exact at any mask resolution: a
//! mask coarser than the grid, where one pixel spans several patches, weighs
//! every patch it covers.
//!
//! Coordinates are patch-center units, where node `c` sits at `u = c`. Pixel
//! column `x` of a `W`-wide mask covers the fraction `[x/W, (x+1)/W]` of the
//! frame and so `u` in `[x·cols/W − 0.5, (x+1)·cols/W − 0.5]`, since centers sit
//! half a cell in. The 2D basis is the product of two 1D hats, and a run of
//! consecutive foreground pixels in one row is a rectangle: one pixel tall,
//! spanning `u` from its first pixel's left edge to its last pixel's right edge.
//! So a run's weight on patch `(r, c)` is its row's 1D integral on `r` times the
//! run's span integral on `c`: one integral per run, with the cost following
//! the mask's run count and the columns each run spans.

use std::num::NonZeroUsize;

use chronoscope_core::grammar::geometry::Region;

/// Each patch's weight in `region`'s pool, row-major over the `cols`×`cols`
/// grid. The whole frame weighs `cols²` in total, one per patch, since the basis
/// sums to 1 everywhere and the frame spans `cols` units on each axis.
pub(super) fn patch_weights(region: &Region, cols: NonZeroUsize) -> Vec<f64> {
    let side = cols.get();
    let (width, height) = (region.width(), region.height());

    let mut weights = vec![0.0; side * side];
    // Runs arrive in raster order, so each row's integral is computed once, on
    // its first run, and only for rows the region touches.
    let mut current: Option<(u32, Vec<(usize, f64)>)> = None;
    for (row, run) in region.foreground_row_runs() {
        let rows = match current {
            Some((cached, ref rows)) if cached == row => rows,
            _ => {
                let rows =
                    node_integrals(edge(row, height, cols), edge(row + 1, height, cols), cols);
                &current.insert((row, rows)).1
            }
        };
        let columns = node_integrals(
            edge(run.start, width, cols),
            edge(run.end, width, cols),
            cols,
        );
        // `node_integrals` names only nodes below `side`, so every index is in
        // range.
        for &(row_node, row_weight) in rows {
            for &(col_node, col_weight) in &columns {
                weights[row_node * side + col_node] += row_weight * col_weight;
            }
        }
    }
    weights
}

/// Where the boundary before pixel `pixel` of an `extent`-pixel axis sits, in
/// patch-center units.
fn edge(pixel: u32, extent: u32, cols: NonZeroUsize) -> f64 {
    // Multiplying before dividing keeps a boundary that lands on a node or a
    // cell boundary exact, so a mask aligned to the grid splits nowhere else.
    (u64::from(pixel) * cols.get() as u64) as f64 / f64::from(extent) - 0.5
}

/// The integral over `[start, end]` of each node's basis: the hat
/// `max(0, 1 − |u − c|)` evaluated at `u` clamped to the outermost centers
/// `[0, cols − 1]`.
///
/// Clamping extends the border patches' values flat to the frame edge. The
/// hats sum to 1 inside the hull of the centers, and past it the clamped border
/// node alone is 1, so the basis sums to 1 everywhere and every span's weights
/// sum to its length. That makes the map total: a sliver of the frame edge
/// lying wholly outside the hull still weighs on the border patch.
fn node_integrals(start: f64, end: f64, cols: NonZeroUsize) -> Vec<(usize, f64)> {
    let last_node = cols.get() - 1;
    let last = last_node as f64;
    let mut weights = Vec::new();

    let below = end.min(0.0) - start;
    if below > 0.0 {
        weights.push((0, below));
    }
    let above = end - start.max(last);
    if above > 0.0 {
        weights.push((last_node, above));
    }

    // Between consecutive centers `k` and `k + 1` only their two hats are
    // nonzero, and both are linear there, so each integral is the sub-span's
    // length times the hat's value at the sub-span's midpoint.
    let (low, high) = (start.max(0.0), end.min(last));
    if low < high {
        for k in (low.floor() as usize)..(high.ceil() as usize) {
            let (from, to) = (low.max(k as f64), high.min((k + 1) as f64));
            let length = to - from;
            if length > 0.0 {
                let midpoint = (from + to) / 2.0 - k as f64;
                weights.push((k, length * (1.0 - midpoint)));
                weights.push((k + 1, length * midpoint));
            }
        }
    }
    weights
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use chronoscope_core::grammar::geometry::{Dimensions, Region};

    use super::super::{
        DegenerateEmbedding, EMBEDDING_DIM, Embedding, Features, Token, embedding::CANCELLATION,
    };

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Features over a `cols`×`cols` grid whose patch `i` is `patch(i)`.
    fn features(cols: usize, patch: impl Fn(usize) -> Token) -> Result<Features, &'static str> {
        Ok(Features {
            cls: patch(0),
            patches: (0..cols * cols).map(patch).collect(),
            cols: NonZeroUsize::new(cols).ok_or("a grid has at least one patch")?,
        })
    }

    /// Patch `index` as the unit vector on axis `index`, so a pooled embedding's
    /// first `cols²` components are the normalized patch weights themselves.
    fn one_hot(index: usize) -> Token {
        let mut token = [0.0; EMBEDDING_DIM];
        if let Some(component) = token.get_mut(index) {
            *component = 1.0;
        }
        token
    }

    /// Patches that differ in every component, so a pool that mixed up which
    /// patch or which dimension it summed would land somewhere else.
    fn varied(index: usize) -> Token {
        std::array::from_fn(|d| ((index * 31 + d * 7) as f32 * 0.013).sin())
    }

    /// A mask over `width`×`height` covering the pixels `covered` accepts.
    fn mask(
        width: u32,
        height: u32,
        covered: impl Fn(u32, u32) -> bool,
    ) -> Result<Region, Box<dyn std::error::Error>> {
        let dense: Vec<bool> = (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .map(|(x, y)| covered(x, y))
            .collect();
        Ok(Region::from_dense(Dimensions::new(width, height)?, &dense)?
            .ok_or("the mask covers at least one pixel")?)
    }

    /// Asserts `pooled` is unit-length and points along `direction` (any
    /// length), component by component within `tolerance`.
    fn assert_points_along(pooled: &Embedding, direction: &[f64], tolerance: f64, what: &str) {
        let norm = pooled
            .as_slice()
            .iter()
            .map(|&value| f64::from(value) * f64::from(value))
            .sum::<f64>()
            .sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-6,
            "{what}: the pooled embedding has norm {norm}, not 1"
        );

        let length = direction
            .iter()
            .map(|value| value * value)
            .sum::<f64>()
            .sqrt();
        let worst = pooled
            .as_slice()
            .iter()
            .zip(direction.iter().chain(std::iter::repeat(&0.0)))
            .map(|(&got, want)| (f64::from(got) - want / length).abs())
            .fold(0.0, f64::max);
        assert!(
            worst <= tolerance,
            "{what}: a component sits {worst:e} from the expected direction, past {tolerance:e}"
        );
    }

    /// The 2D weights `rows[r] · cols[c]`, row-major over a `side`×`side` grid,
    /// from 1D node weights given as `(node, weight)` pairs.
    fn outer(side: usize, axis: &[(usize, f64)]) -> Vec<f64> {
        let mut weights = vec![0.0; side * side];
        for &(row, row_weight) in axis {
            for &(col, col_weight) in axis {
                if let Some(slot) = weights.get_mut(row * side + col) {
                    *slot = row_weight * col_weight;
                }
            }
        }
        weights
    }

    #[test]
    fn a_full_frame_region_pools_to_the_uniform_patch_mean() -> TestResult {
        let cols = 4;
        let features = features(cols, varied)?;
        let mut mean = [0.0_f64; EMBEDDING_DIM];
        for patch in &features.patches {
            for (total, &value) in mean.iter_mut().zip(patch) {
                *total += f64::from(value);
            }
        }

        // Clamp-to-hull makes each border patch weigh exactly 1 like an interior
        // one, whether a pixel is a fraction of a cell (37x23, 16x16), several
        // cells (1x1, 3x2, smaller than the grid), or neither aligned nor square.
        for (width, height) in [(1, 1), (3, 2), (37, 23), (16, 16), (8, 12)] {
            let region = mask(width, height, |_, _| true)?;
            let pooled = features.region(&region)?;
            assert_points_along(&pooled, &mean, 1e-6, &format!("{width}x{height}"));
        }
        Ok(())
    }

    #[test]
    fn an_interior_cell_weighs_its_neighbors_by_the_hat_moments() -> TestResult {
        // Cell (2, 2) of a 5x5 grid, three mask pixels to a cell. It spans
        // `u` in [1.5, 2.5], where the hats integrate to 1/8, 3/4, 1/8 on nodes
        // 1, 2, 3.
        let cols = 5;
        let features = features(cols, one_hot)?;
        let region = mask(15, 15, |x, y| (6..9).contains(&x) && (6..9).contains(&y))?;
        let expected = outer(cols, &[(1, 0.125), (2, 0.75), (3, 0.125)]);
        let pooled = features.region(&region)?;
        assert_points_along(&pooled, &expected, 1e-7, "interior cell");
        Ok(())
    }

    #[test]
    fn a_corner_cell_extends_the_border_value_past_the_outer_center() -> TestResult {
        // Cell (0, 0) spans `u` in [-0.5, 0.5]. Clamped, node 0 takes the
        // outside half whole plus 3/8 inside: 7/8 against node 1's 1/8. Plain
        // hats would give 3/4 : 1/8 and hats cut off at the hull 3/8 : 1/8.
        let cols = 5;
        let features = features(cols, one_hot)?;
        let region = mask(15, 15, |x, y| x < 3 && y < 3)?;
        let expected = outer(cols, &[(0, 0.875), (1, 0.125)]);
        let pooled = features.region(&region)?;
        assert_points_along(&pooled, &expected, 1e-7, "corner cell");
        Ok(())
    }

    #[test]
    fn a_frame_edge_sliver_outside_the_hull_pools_the_border_patches() -> TestResult {
        // One pixel column of a 64-wide mask spans `u` in [-0.5, -0.4375],
        // wholly outside the hull, so it lands on column 0 alone; spanning the
        // full height, it weighs that column's four patches uniformly.
        let cols = 4;
        let features = features(cols, one_hot)?;
        let column = mask(64, 32, |x, _| x == 0)?;
        let mut expected = vec![0.0; cols * cols];
        for row in 0..cols {
            if let Some(slot) = expected.get_mut(row * cols) {
                *slot = 1.0;
            }
        }
        assert_points_along(&features.region(&column)?, &expected, 1e-7, "left column");

        // One corner pixel is outside the hull on both axes: patch (0, 0) alone.
        let corner = mask(64, 32, |x, y| x == 0 && y == 0)?;
        assert_points_along(&features.region(&corner)?, &[1.0], 1e-7, "corner pixel");
        Ok(())
    }

    /// Each patch's weight measured independently of the closed form: sample
    /// every foreground pixel at `samples`×`samples` sub-pixel centers,
    /// interpolate the clamped bilinear field there by floor and fraction, and
    /// accumulate the four corner weights.
    fn supersampled_weights(region: &Region, cols: usize, samples: usize) -> Vec<f64> {
        let (width, height) = (region.width() as usize, region.height() as usize);
        let last = (cols - 1) as f64;
        let to_node = |pixel: usize, sub: usize, extent: usize| {
            let fraction = (pixel as f64 + (sub as f64 + 0.5) / samples as f64) / extent as f64;
            (fraction * cols as f64 - 0.5).clamp(0.0, last)
        };
        let mut weights = vec![0.0; cols * cols];
        let mut add = |row: usize, col: usize, weight: f64| {
            if let Some(slot) = weights.get_mut(row * cols + col) {
                *slot += weight;
            }
        };
        for pixel in region.foreground_pixels() {
            let (x, y) = (pixel % width, pixel / width);
            for sub_y in 0..samples {
                let v = to_node(y, sub_y, height);
                let (row, next_row, dv) = (v as usize, (v as usize + 1).min(cols - 1), v.fract());
                for sub_x in 0..samples {
                    let u = to_node(x, sub_x, width);
                    let (col, next_col, du) =
                        (u as usize, (u as usize + 1).min(cols - 1), u.fract());
                    add(row, col, (1.0 - du) * (1.0 - dv));
                    add(row, next_col, du * (1.0 - dv));
                    add(next_row, col, (1.0 - du) * dv);
                    add(next_row, next_col, du * dv);
                }
            }
        }
        weights
    }

    #[test]
    fn exact_quadrature_agrees_with_a_supersampled_field() -> TestResult {
        // An irregular blob with holes on a grid aligned to nothing, and a mask
        // coarser than the patch grid, where each pixel spans several cells and
        // the split at every node line inside it matters.
        let blob = mask(37, 23, |x, y| {
            let (dx, dy) = (f64::from(x) - 17.0, f64::from(y) - 11.0);
            (dx / 15.0).powi(2) + (dy / 9.0).powi(2) <= 1.0 && x % 7 != 3
        })?;
        let coarse = mask(3, 2, |x, y| x + y != 2)?;

        // The sampled integral lands within 1.1e-6 of the closed form on the blob
        // and 4.2e-6 on the coarse mask, whose pixels span more of each hat and
        // so take more samples. 1e-4 leaves over 20x margin, while a wrong
        // quadrature (a swapped pair of node weights, a dropped clamp, a coarse
        // pixel split at only its first cell) moves some component by 1e-2 or
        // more.
        for (name, region, cols, samples) in [("blob", &blob, 5, 64), ("coarse", &coarse, 7, 256)] {
            let features = features(cols, one_hot)?;
            let oracle = supersampled_weights(region, cols, samples);
            assert_points_along(&features.region(region)?, &oracle, 1e-4, name);
        }
        Ok(())
    }

    #[test]
    fn patches_that_cancel_are_a_degenerate_pool() -> TestResult {
        // A checkerboard of +v and -v under the full frame's uniform weights
        // sums to zero: no direction survives.
        let cols = 4;
        let features = features(cols, |index| {
            let sign = if (index / cols + index % cols) % 2 == 0 {
                1.0
            } else {
                -1.0
            };
            varied(0).map(|value| sign * value)
        })?;
        let region = mask(8, 8, |_, _| true)?;
        match features.region(&region) {
            Err(DegenerateEmbedding { norm, mass }) => {
                assert!(
                    mass > 0.0 && norm <= 1e-6 * mass,
                    "norm {norm}, mass {mass}"
                );
            }
            Ok(_) => return Err("cancelling patches pooled to a direction".into()),
        }
        Ok(())
    }

    #[test]
    fn the_degenerate_floor_is_relative_to_what_went_into_the_pool() -> TestResult {
        // A 2x2 checkerboard of `[±1, ε, 0, …]` under a mask where every patch
        // weighs exactly 1: the ±1 cancels exactly, leaving `4ε` on axis 1
        // against a mass of `4·sqrt(1 + ε²)`, a kept fraction just under ε.
        let region = mask(4, 4, |_, _| true)?;
        for (fraction, degenerate) in [(0.9, true), (1.1, false)] {
            let epsilon = (fraction * CANCELLATION) as f32;
            let features = features(2, |index| {
                let mut token = [0.0; EMBEDDING_DIM];
                token[0] = if (index / 2 + index % 2) % 2 == 0 {
                    1.0
                } else {
                    -1.0
                };
                token[1] = epsilon;
                token
            })?;
            let what = format!("ε at {fraction} of the floor");
            match (features.region(&region), degenerate) {
                (Err(DegenerateEmbedding { .. }), true) => {}
                (Ok(pooled), false) => assert_points_along(&pooled, &[0.0, 1.0], 1e-7, &what),
                (Ok(_), true) => return Err(format!("{what}: pooled to a direction").into()),
                (Err(error), false) => return Err(format!("{what}: {error}").into()),
            }
        }
        Ok(())
    }

    #[test]
    fn small_but_uncancelled_inputs_normalize_to_unit_length() -> TestResult {
        // Far below any absolute cutoff and nothing cancels, so both reads keep
        // their direction.
        let features = features(3, |index| varied(index).map(|value| value * 1e-30))?;
        let cls: Vec<f64> = features.cls.iter().map(|&value| f64::from(value)).collect();
        assert_points_along(&features.image()?, &cls, 1e-6, "tiny CLS");

        let mut mean = [0.0_f64; EMBEDDING_DIM];
        for patch in &features.patches {
            for (total, &value) in mean.iter_mut().zip(patch) {
                *total += f64::from(value);
            }
        }
        let region = mask(9, 9, |_, _| true)?;
        assert_points_along(&features.region(&region)?, &mean, 1e-6, "tiny patches");
        Ok(())
    }

    #[test]
    fn a_zero_cls_has_no_image_embedding() -> TestResult {
        let features = Features {
            cls: [0.0; EMBEDDING_DIM],
            ..features(2, one_hot)?
        };
        assert!(features.image().is_err());
        Ok(())
    }
}
