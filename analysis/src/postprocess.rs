//! Reducing a SAM 3 concept pass's raw per-instance regions to the clean,
//! ordered set the rest of the pipeline overlays and describes.
//!
//! The grounding head emits one region per detected instance, but the raw set
//! duplicates and overlaps: the same building is found more than once, a small
//! detection sits mostly inside a larger one. Set-of-Mark then marks each region
//! and the VLM describes it by number, so both want a tidy, bounded, ordered set.
//! This resolves the raw regions geometrically, with no notion of entity type —
//! the VLM assigns type and description later, over the overlay.
//!
//! Grouping contained regions into a parent with sub-features (the writeup's
//! `SUBORDINATE_PROMPTS` layer) is deliberately not done here: it keys on which
//! concept prompt found each region, and a [`Region`] carries no such label
//! yet. That is a conscious gap, not another silent drop.

use chronoscope_core::grammar::geometry::{Region as Mask, RegionError};

use crate::scene::{Detection, Region};

/// Two regions overlapping by more than this are the same detection; the
/// lower-scored one is dropped.
const DEDUP_IOU: f64 = 0.7;

/// The least share of its own area a region must own exclusively — pixels no
/// higher-scored region also covers — to survive. A detection almost wholly
/// inside another keeps too little to stand alone and is dropped rather than
/// double-counted.
const SURVIVAL: f64 = 0.1;

/// Why [`postprocess_regions`] could not resolve its input.
#[derive(Debug, thiserror::Error)]
pub enum PostprocessError {
    /// Rebuilding a survivor from its exclusively-claimed pixels failed.
    #[error(transparent)]
    Region(#[from] RegionError),
}

/// Reduce raw per-instance detections to the clean, ordered set of regions to
/// overlay and describe: drop near-duplicates, resolve overlaps by exclusive
/// area, keep the most confident `max_regions`, and order them left to right by
/// centroid.
///
/// The scores are spent here, ranking the contests; what survives is regions,
/// which is all the overlay and the describe loop read.
///
/// `max_regions` bounds the overlay's palette and the describe call's length; it
/// is the caller's to tune, not a fixed limit on how many entities an image may
/// hold. Every region is a mask over one scene, which is what makes the overlap
/// comparisons below meaningful.
pub fn postprocess_regions<'b>(
    detections: Vec<Detection<'b>>,
    max_regions: usize,
) -> Result<Vec<Region<'b>>, PostprocessError> {
    let Some(first) = detections.first() else {
        return Ok(Vec::new());
    };
    let grid = first.region.mask().dimensions();

    // Highest score first, so a dedup or an exclusive-pixel contest resolves in
    // favor of the more confident detection.
    let mut ranked = detections;
    ranked.sort_by(|a, b| b.score.total_cmp(&a.score));

    // Drop a region that overlaps an already-kept, higher-scored one by more than
    // the dedup threshold: the same detection found twice.
    let mut kept: Vec<Region<'b>> = Vec::new();
    for candidate in ranked {
        let mut duplicate = false;
        for keeper in &kept {
            if candidate.region.iou(keeper) > DEDUP_IOU {
                duplicate = true;
                break;
            }
        }
        if !duplicate {
            kept.push(candidate.region);
        }
    }

    // Give each foreground pixel to the highest-scored *surviving* region
    // covering it: walk `kept` score-first, tentatively take the pixels no
    // surviving region already holds, and commit that claim only when the region
    // clears the survival floor. A dropped region leaves no claim, so it cannot
    // steal pixels from a lower-scored survivor. A survivor is rebuilt from
    // exactly the pixels it owns, so the masks are disjoint. `claimed` is one
    // byte per pixel and each region walks only the pixels it covers, so nothing
    // densifies the grid.
    let extent = (grid.width() as usize) * (grid.height() as usize);
    let mut claimed = vec![false; extent];
    let mut survivors: Vec<Region<'b>> = Vec::new();
    for region in kept {
        let mut mine: Vec<usize> = Vec::new();
        for pixel in region.mask().foreground_pixels() {
            // Tentative — not claimed yet. A region that fails the survival floor
            // must leave no claim behind, or it steals pixels from a lower-scored
            // survivor while never appearing in the output. `foreground_pixels`
            // is ascending, so `mine` stays ascending — the shape
            // `Region::from_pixels` requires.
            if !claimed[pixel] {
                mine.push(pixel);
            }
        }
        if mine.len() as f64 >= SURVIVAL * region.mask().area() as f64
            && let Some(mask) = Mask::from_pixels(grid, &mine)?
            // The pixels came from this region, so the rebuilt mask is on its
            // grid and this always binds.
            && let Some(survivor) = region.with_mask(mask)
        {
            // Commit the claim only now that the region survives.
            for &pixel in &mine {
                claimed[pixel] = true;
            }
            survivors.push(survivor);
        }
    }

    // Keep the most confident, then present left to right by centroid column.
    survivors.truncate(max_regions);
    let mut keyed: Vec<(f64, Region<'b>)> = survivors
        .into_iter()
        .map(|region| (region.mask().centroid().x(), region))
        .collect();
    keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
    Ok(keyed.into_iter().map(|(_, region)| region).collect())
}

#[cfg(test)]
mod tests {
    use chronoscope_core::grammar::geometry::Dimensions;

    use super::*;
    use crate::{
        scene::{Scene, with_scene_sync},
        test_support::frame,
    };

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// A detection of `scene`: a dense mask on `grid`, with the score the
    /// contests rank by.
    fn scored<'b>(
        scene: &Scene<'b>,
        grid: Dimensions,
        dense: &[bool],
        score: f32,
    ) -> Result<Detection<'b>, Box<dyn std::error::Error>> {
        let mask = Mask::from_dense(grid, dense)?.ok_or("non-empty")?;
        let region = Region::new(scene, mask).ok_or("on the scene's grid")?;
        Ok(Detection { region, score })
    }

    /// A run of `count` foreground pixels starting at `start` on a `len`-pixel
    /// grid, the rest background.
    fn run(len: usize, start: usize, count: usize) -> Vec<bool> {
        (0..len).map(|i| i >= start && i < start + count).collect()
    }

    #[test]
    fn dedup_keeps_the_higher_scored_of_a_near_duplicate_pair() -> TestResult {
        let grid = Dimensions::new(4, 4)?;
        with_scene_sync(frame(4, 4), |scene| {
            // 12/16 and 16/16 overlap at IoU 0.75, over the 0.7 threshold.
            let lower = scored(scene, grid, &run(16, 0, 12), 0.8)?;
            let higher = scored(scene, grid, &run(16, 0, 16), 0.9)?;
            let out = postprocess_regions(vec![lower, higher], 32)?;
            assert_eq!(out.len(), 1);
            // The survivor is the 0.9 one, which is the 16-pixel mask.
            assert_eq!(out[0].mask().area(), 16);
            Ok(())
        })
    }

    #[test]
    fn exclusive_pixels_drop_a_region_swallowed_by_a_bigger_one() -> TestResult {
        let grid = Dimensions::new(4, 4)?;
        with_scene_sync(frame(4, 4), |scene| {
            // The small region's 2 pixels sit inside the full region; its IoU with
            // the full one (2/16) is under the dedup threshold, so only the
            // exclusive-pixel step can remove it. The full region claims those
            // pixels first, leaving the small one 0 exclusive of its 2, under the
            // 0.1 survival share.
            let full = scored(scene, grid, &run(16, 0, 16), 0.9)?;
            let small = scored(scene, grid, &run(16, 5, 2), 0.5)?;
            let out = postprocess_regions(vec![full, small], 32)?;
            assert_eq!(out.len(), 1);
            // The full region survives whole: the small one claimed nothing.
            assert_eq!(out[0].mask().area(), 16);
            Ok(())
        })
    }

    #[test]
    fn a_dropped_region_does_not_steal_pixels_from_a_survivor() -> TestResult {
        // A (0.9) owns 0..20. B (0.7) is 10..21 — ten pixels inside A and one
        // exclusive (pixel 20), below the survival floor, so B drops. C (0.5)
        // owns 20..25, sharing pixel 20 with B's sliver. B must not keep pixel 20
        // claimed on its way out, or C loses it to a region that never reaches
        // the output.
        let grid = Dimensions::new(25, 1)?;
        with_scene_sync(frame(25, 1), |scene| {
            let a = scored(scene, grid, &run(25, 0, 20), 0.9)?;
            let b = scored(scene, grid, &run(25, 10, 11), 0.7)?;
            let c = scored(scene, grid, &run(25, 20, 5), 0.5)?;

            let out = postprocess_regions(vec![a, b, c], 32)?;

            assert_eq!(out.len(), 2, "A and C survive, B drops");
            assert!(
                out.iter().any(|s| s.mask().to_dense()[20]),
                "pixel 20 stays with a survivor, not stolen by the dropped B"
            );
            Ok(())
        })
    }

    #[test]
    fn output_is_ordered_left_to_right_by_centroid() -> TestResult {
        // Three disjoint columns on a 6x1 grid: pixel 5 (right), 0 (left), 2..4
        // (middle). Distinct scores keep them all through dedup and exclusivity.
        let grid = Dimensions::new(6, 1)?;
        with_scene_sync(frame(6, 1), |scene| {
            let right = scored(scene, grid, &run(6, 5, 1), 0.7)?;
            let left = scored(scene, grid, &run(6, 0, 1), 0.8)?;
            let middle = scored(scene, grid, &run(6, 2, 2), 0.9)?;
            let out = postprocess_regions(vec![right, left, middle], 32)?;
            let columns: Vec<f64> = out.iter().map(|s| s.mask().centroid().x()).collect();
            assert_eq!(out.len(), 3);
            assert!(columns[0] < columns[1] && columns[1] < columns[2]);
            Ok(())
        })
    }

    #[test]
    fn cap_keeps_the_most_confident() -> TestResult {
        let grid = Dimensions::new(6, 1)?;
        with_scene_sync(frame(6, 1), |scene| {
            let a = scored(scene, grid, &run(6, 0, 1), 0.5)?;
            let b = scored(scene, grid, &run(6, 2, 1), 0.9)?;
            let c = scored(scene, grid, &run(6, 4, 1), 0.7)?;
            let out = postprocess_regions(vec![a, b, c], 2)?;
            assert_eq!(out.len(), 2);
            // The 0.5 region at pixel 0 is dropped; the 0.9 at pixel 2 and the
            // 0.7 at pixel 4 remain, ordered by column.
            let covered: Vec<bool> = out
                .iter()
                .flat_map(|region| region.mask().foreground_pixels())
                .fold(vec![false; 6], |mut covered, pixel| {
                    covered[pixel] = true;
                    covered
                });
            assert!(covered[2] && covered[4] && !covered[0]);
            Ok(())
        })
    }

    #[test]
    fn empty_input_is_empty_output() -> TestResult {
        with_scene_sync(frame(1, 1), |_scene| {
            assert!(postprocess_regions(Vec::new(), 32)?.is_empty());
            Ok(())
        })
    }

    #[test]
    fn surviving_masks_are_disjoint() -> TestResult {
        // Pixels 0..10 @0.9 and 5..15 @0.8 overlap at IoU 5/15 = 0.33, under the
        // dedup threshold, so both clear dedup; each owns >= 10% exclusively, so
        // both survive. Their output masks must share no pixel — the overlap went
        // to the higher score. This is the case the half-port left overlapping.
        let grid = Dimensions::new(20, 1)?;
        with_scene_sync(frame(20, 1), |scene| {
            let higher = scored(scene, grid, &run(20, 0, 10), 0.9)?;
            let lower = scored(scene, grid, &run(20, 5, 10), 0.8)?;
            let out = postprocess_regions(vec![higher, lower], 32)?;
            assert_eq!(out.len(), 2);
            let a = out[0].mask().to_dense();
            let b = out[1].mask().to_dense();
            for pixel in 0..20 {
                assert!(
                    !(a[pixel] && b[pixel]),
                    "pixel {pixel} is claimed by both survivors"
                );
            }
            Ok(())
        })
    }

    #[test]
    fn a_survivor_reduced_around_a_claim_keeps_the_hole() -> TestResult {
        // The higher region sits in the middle of the lower one and claims those
        // pixels first, so the lower survivor's reduced mask is non-contiguous:
        // foreground everywhere it covered except the punched-out middle.
        let grid = Dimensions::new(10, 1)?;
        with_scene_sync(frame(10, 1), |scene| {
            let higher = scored(scene, grid, &run(10, 4, 2), 0.9)?;
            let lower = scored(scene, grid, &run(10, 0, 10), 0.5)?;
            let out = postprocess_regions(vec![higher, lower], 32)?;
            assert_eq!(out.len(), 2);
            // The lower region is the surviving one with a hole, so it is the
            // larger of the two by area.
            let holey = out
                .iter()
                .max_by_key(|region| region.mask().area())
                .ok_or("the lower region survives")?;
            let dense = holey.mask().to_dense();
            assert!(!dense[4] && !dense[5], "the claimed middle is excluded");
            assert!(dense[3] && dense[6], "the surrounding pixels remain");
            Ok(())
        })
    }
}
