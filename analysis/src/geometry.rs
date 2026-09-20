//! Comparing two masks over one image's pixel grid.
//!
//! A [`Region`] carries one mask and the geometry of that one mask. A metric
//! over a *pair* of masks belongs where pairs arise, which is this crate: the
//! SAM concept head emits one region per detected instance and the raw set
//! duplicates, so deciding that two detections found the same building is what
//! this answers.
//!
//! The pair's one precondition, a shared grid, is carried by the type that owns
//! the pairs: [`Region::iou`](crate::scene::Region::iou) is the only caller, and
//! its brand proves both masks came from one scene.

use chronoscope_core::grammar::geometry::Region;

/// Intersection over union of two masks on one grid (`IoU`: shared foreground
/// area over combined foreground area). `1.0` for equal masks, `0.0` for
/// disjoint ones.
///
/// Both masks are read in linear index space, so masks on differing grids would
/// compare as though their pixels lined up. The caller owes the shared grid.
pub(crate) fn intersection_over_union(a: &Region, b: &Region) -> f64 {
    let (a_area, b_area) = (a.area(), b.area());
    // A `Region` is never empty, so both areas are positive and the union below
    // is never zero.
    let intersection = foreground_overlap(a, b);
    let union = a_area + b_area - intersection;
    intersection as f64 / union as f64
}

/// Shared foreground pixel count, assuming equal grids: merge the two foreground
/// interval lists, which are sorted by linear index and do not overlap within a
/// mask.
fn foreground_overlap(a: &Region, b: &Region) -> u64 {
    let (mut ai, mut bi) = (a.foreground_intervals(), b.foreground_intervals());
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

#[cfg(test)]
mod tests {
    use chronoscope_core::grammar::geometry::Dimensions;
    use proptest::prelude::*;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

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
    fn region_iou_scores_overlap() -> TestResult {
        let grid = Dimensions::new(2, 2)?;
        let full = Region::from_dense(grid, &[true, true, true, true])?.ok_or("non-empty")?;
        let left = Region::from_dense(grid, &[true, false, true, false])?.ok_or("non-empty")?;
        let right = Region::from_dense(grid, &[false, true, false, true])?.ok_or("non-empty")?;
        // An all-background mask is not a region.
        assert!(Region::from_dense(grid, &[false, false, false, false])?.is_none());

        assert_eq!(intersection_over_union(&full, &full), 1.0);
        assert_eq!(intersection_over_union(&left, &right), 0.0);
        // left is 2 of full's 4 pixels: intersection 2, union 4.
        assert_eq!(intersection_over_union(&left, &full), 0.5);
        Ok(())
    }

    proptest! {
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
            let ab = intersection_over_union(&a, &b);
            let ba = intersection_over_union(&b, &a);
            prop_assert_eq!(ab, ba);
            prop_assert!((0.0..=1.0).contains(&ab));
            prop_assert_eq!(intersection_over_union(&a, &a), 1.0);
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
            prop_assert_eq!(intersection_over_union(&a, &b), expected);
        }
    }
}
