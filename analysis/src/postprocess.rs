//! Reducing a SAM 3 concept pass's raw per-instance regions to the clean,
//! ordered set the rest of the pipeline overlays and describes.
//!
//! The grounding head emits one region per detected instance, but the raw set
//! duplicates and overlaps: the same building is found more than once, a small
//! detection sits mostly inside a larger one. Set-of-Mark then colors each region
//! and the VLM describes it by number, so both want a tidy, bounded, ordered set.
//! This resolves the raw regions geometrically, with no notion of entity type —
//! the VLM assigns type and description later, over the overlay.

use chronoscope_core::grammar::geometry::GridMismatch;

use crate::sam3::ScoredRegion;

/// Two regions overlapping by more than this are the same detection; the
/// lower-scored one is dropped.
const DEDUP_IOU: f64 = 0.7;

/// The least share of its own area a region must own exclusively — pixels no
/// higher-scored region also covers — to survive. A detection almost wholly
/// inside another keeps too little to stand alone and is dropped rather than
/// double-counted.
const SURVIVAL: f64 = 0.1;

/// Reduce raw per-instance regions to the clean, ordered set to overlay and
/// describe: drop near-duplicates, resolve overlaps by exclusive area, keep the
/// most confident `max_regions`, and order them left to right by centroid.
///
/// `max_regions` bounds the overlay's palette and the describe call's length; it
/// is the caller's to tune, not a fixed limit on how many entities an image may
/// hold. Every region must share one grid — they come from one image — so a
/// differing grid is a [`GridMismatch`].
pub fn postprocess_regions(
    regions: Vec<ScoredRegion>,
    max_regions: usize,
) -> Result<Vec<ScoredRegion>, GridMismatch> {
    let Some(first) = regions.first() else {
        return Ok(Vec::new());
    };
    let grid = first.region.dimensions();
    for scored in &regions {
        if scored.region.dimensions() != grid {
            return Err(GridMismatch {
                a: grid,
                b: scored.region.dimensions(),
            });
        }
    }

    // Highest score first, so a dedup or an exclusive-pixel contest resolves in
    // favor of the more confident detection.
    let mut ranked = regions;
    ranked.sort_by(|a, b| b.score.total_cmp(&a.score));

    // Drop a region that overlaps an already-kept, higher-scored one by more than
    // the dedup threshold: the same detection found twice.
    let mut kept: Vec<ScoredRegion> = Vec::new();
    for candidate in ranked {
        let mut duplicate = false;
        for keeper in &kept {
            if candidate.region.intersection_over_union(&keeper.region)? > DEDUP_IOU {
                duplicate = true;
                break;
            }
        }
        if !duplicate {
            kept.push(candidate);
        }
    }

    // Claim each foreground pixel for the highest-scored region covering it —
    // `kept` is score-ordered, so a region claims only pixels no higher-scored
    // region already took — then keep it only if that exclusive share of its own
    // area clears the survival floor. `claimed` is one byte per pixel and each
    // region walks only the pixels it covers, so nothing densifies the grid.
    let extent = (grid.width() as usize) * (grid.height() as usize);
    let mut claimed = vec![false; extent];
    let mut survivors: Vec<ScoredRegion> = Vec::new();
    for scored in kept {
        let mut exclusive: u64 = 0;
        for pixel in scored.region.foreground_pixels() {
            if !claimed[pixel] {
                claimed[pixel] = true;
                exclusive += 1;
            }
        }
        if exclusive as f64 >= SURVIVAL * scored.region.area() as f64 {
            survivors.push(scored);
        }
    }

    // Keep the most confident, then present left to right by centroid column.
    survivors.truncate(max_regions);
    let mut keyed: Vec<(f64, ScoredRegion)> = survivors
        .into_iter()
        .map(|scored| (scored.region.centroid().x(), scored))
        .collect();
    keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
    Ok(keyed.into_iter().map(|(_, scored)| scored).collect())
}

#[cfg(test)]
mod tests {
    use chronoscope_core::grammar::geometry::{Dimensions, Region};

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// A scored region from a dense mask on `grid`.
    fn scored(
        grid: Dimensions,
        dense: &[bool],
        score: f32,
    ) -> Result<ScoredRegion, Box<dyn std::error::Error>> {
        Ok(ScoredRegion {
            region: Region::from_dense(grid, dense)?.ok_or("non-empty")?,
            score,
        })
    }

    /// A run of `count` foreground pixels starting at `start` on a `len`-pixel
    /// grid, the rest background.
    fn run(len: usize, start: usize, count: usize) -> Vec<bool> {
        (0..len).map(|i| i >= start && i < start + count).collect()
    }

    #[test]
    fn dedup_keeps_the_higher_scored_of_a_near_duplicate_pair() -> TestResult {
        let grid = Dimensions::new(4, 4)?;
        // 12/16 and 16/16 overlap at IoU 0.75, over the 0.7 threshold.
        let lower = scored(grid, &run(16, 0, 12), 0.8)?;
        let higher = scored(grid, &run(16, 0, 16), 0.9)?;
        let out = postprocess_regions(vec![lower, higher], 32)?;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].score, 0.9);
        Ok(())
    }

    #[test]
    fn exclusive_pixels_drop_a_region_swallowed_by_a_bigger_one() -> TestResult {
        let grid = Dimensions::new(4, 4)?;
        // The small region's 2 pixels sit inside the full region; its IoU with the
        // full one (2/16) is under the dedup threshold, so only the exclusive-pixel
        // step can remove it. The full region claims those pixels first, leaving
        // the small one 0 exclusive of its 2, under the 0.1 survival share.
        let full = scored(grid, &run(16, 0, 16), 0.9)?;
        let small = scored(grid, &run(16, 5, 2), 0.5)?;
        let out = postprocess_regions(vec![full, small], 32)?;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].score, 0.9);
        Ok(())
    }

    #[test]
    fn output_is_ordered_left_to_right_by_centroid() -> TestResult {
        // Three disjoint columns on a 6x1 grid: pixel 5 (right), 0 (left), 2..4
        // (middle). Distinct scores keep them all through dedup and exclusivity.
        let grid = Dimensions::new(6, 1)?;
        let right = scored(grid, &run(6, 5, 1), 0.7)?;
        let left = scored(grid, &run(6, 0, 1), 0.8)?;
        let middle = scored(grid, &run(6, 2, 2), 0.9)?;
        let out = postprocess_regions(vec![right, left, middle], 32)?;
        let columns: Vec<f64> = out.iter().map(|s| s.region.centroid().x()).collect();
        assert_eq!(out.len(), 3);
        assert!(columns[0] < columns[1] && columns[1] < columns[2]);
        Ok(())
    }

    #[test]
    fn cap_keeps_the_most_confident() -> TestResult {
        let grid = Dimensions::new(6, 1)?;
        let a = scored(grid, &run(6, 0, 1), 0.5)?;
        let b = scored(grid, &run(6, 2, 1), 0.9)?;
        let c = scored(grid, &run(6, 4, 1), 0.7)?;
        let out = postprocess_regions(vec![a, b, c], 2)?;
        assert_eq!(out.len(), 2);
        // The 0.5 region is the one dropped; 0.9 and 0.7 remain, ordered by column.
        let scores: Vec<f32> = out.iter().map(|s| s.score).collect();
        assert!(scores.contains(&0.9) && scores.contains(&0.7) && !scores.contains(&0.5));
        Ok(())
    }

    #[test]
    fn regions_on_different_grids_are_a_grid_mismatch() -> TestResult {
        let wide = scored(Dimensions::new(4, 1)?, &run(4, 0, 4), 0.9)?;
        let tall = scored(Dimensions::new(1, 4)?, &run(4, 0, 4), 0.8)?;
        assert!(postprocess_regions(vec![wide, tall], 32).is_err());
        Ok(())
    }

    #[test]
    fn empty_input_is_empty_output() -> TestResult {
        assert!(postprocess_regions(Vec::new(), 32)?.is_empty());
        Ok(())
    }
}
