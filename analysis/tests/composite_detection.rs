//! Pass 1 of the image pipeline end to end: compose known panels into one image,
//! ask the model where the panels are, and check it found them.
//!
//! `create_image_grid` builds a composite from source images and hands back the
//! exact proportional rectangle of every panel, so the ground truth is a byproduct
//! of the construction rather than a hand-measurement. A deterministic gate test
//! proves that ground truth is pixel-exact by reading the canvas back; the pure
//! `rect_iou` / `max_iou_matching` helpers score a detection against it. The e2e
//! test loads `Qwen3`, runs `detect_composite` over several layouts, and asserts
//! the panels come back within a seam tolerance. It is ignored by default: it
//! loads a multi-gigabyte model the commit gate cannot realize.

use std::collections::BTreeSet;
use std::{
    env, error,
    path::{Path, PathBuf},
};

use chronoscope_analysis::ask::{CompositeOutcome, Outcome};
use chronoscope_analysis::pipeline::{detect_composite, draw_subimages, subimages};
use chronoscope_analysis::qwen3::Qwen3;
use chronoscope_core::grammar::composites::SubimageRegion;
use chronoscope_core::grammar::geometry::{ProportionalCoordError, ProportionalRect};
use image::{DynamicImage, Rgb, RgbImage, imageops::FilterType};

mod common;
use common::{describe, first_shard};

/// A table of source images, one inner list per row, composed left to right then
/// top to bottom.
type Layout = Vec<Vec<DynamicImage>>;

// ---------------------------------------------------------------------------
// Grid construction with exact ground truth
// ---------------------------------------------------------------------------

/// Why a grid could not be composed.
#[derive(Debug, thiserror::Error)]
enum GridError {
    #[error("image grid has no rows")]
    NoRows,
    #[error("row {row} has no images")]
    EmptyRow { row: usize },
    #[error("cell at row {row}, column {col} snapped to a zero-{axis} sliver")]
    ZeroCell {
        row: usize,
        col: usize,
        axis: &'static str,
    },
    #[error("a cell rectangle fell outside the unit square")]
    Coord(#[from] ProportionalCoordError),
}

/// Composes `rows` into one canvas and returns it with the exact proportional
/// rectangle of every cell, in row-major order.
///
/// Every image in a row is scaled to a shared row height (so their differing
/// aspect ratios give differing widths and off-center vertical seams), and every
/// row is scaled to the shared canvas width (so their differing content gives
/// differing heights and off-center horizontal seams). Cell boundaries are the
/// cumulative sums of those spans snapped to whole pixels; a boundary is shared
/// by its two neighbors and the last cell in each row and column absorbs the
/// rounding remainder, so the cells tile the canvas with no gap and no overlap.
/// Each source is resized once, straight into its snapped integer cell, so the
/// returned rectangles (the integer boundaries over the canvas dimensions) bound
/// exactly the pixels each source occupies.
fn create_image_grid(rows: Layout) -> Result<(DynamicImage, Vec<ProportionalRect>), GridError> {
    if rows.is_empty() {
        return Err(GridError::NoRows);
    }
    for (row, images) in rows.iter().enumerate() {
        if images.is_empty() {
            return Err(GridError::EmptyRow { row });
        }
    }

    // Per-cell aspect ratio and its per-row sum. A row scaled to height h is
    // (h * sum-of-aspects) wide, so 1/sum-of-aspects is the row's height when the
    // canvas width is one unit.
    let aspects: Vec<Vec<f64>> = rows
        .iter()
        .map(|images| {
            images
                .iter()
                .map(|img| f64::from(img.width()) / f64::from(img.height()))
                .collect()
        })
        .collect();
    let aspect_sums: Vec<f64> = aspects.iter().map(|row| row.iter().sum()).collect();
    let row_heights: Vec<f64> = aspect_sums.iter().map(|sum| 1.0 / sum).collect();
    let height_total: f64 = row_heights.iter().sum();

    // Canvas width from the row that is widest at its own images' native scale, so
    // the busiest row is not upscaled; the height follows from the aspect. Natural
    // pixel sizes, not snapped to the model's tokenization grid: real composites are
    // arbitrary sizes, so any preprocessing distortion is part of what we measure.
    let width_natural = rows
        .iter()
        .zip(&aspect_sums)
        .map(|(images, &sum)| {
            let tallest = images.iter().map(DynamicImage::height).max().unwrap_or(1);
            f64::from(tallest) * sum
        })
        .fold(0.0_f64, f64::max);
    let canvas_w = width_natural.round().max(1.0) as u32;
    let canvas_h = (f64::from(canvas_w) * height_total).round().max(1.0) as u32;

    // Row boundaries: cumulative height weights distributed across the whole
    // canvas height, the last row pinned to the bottom edge.
    let row_count = rows.len();
    let mut y_bounds = Vec::with_capacity(row_count + 1);
    y_bounds.push(0u32);
    let mut height_acc = 0.0;
    for (row, &weight) in row_heights.iter().enumerate() {
        height_acc += weight;
        let bound = if row + 1 == row_count {
            canvas_h
        } else {
            (f64::from(canvas_h) * height_acc / height_total).round() as u32
        };
        y_bounds.push(bound);
    }

    let mut canvas = RgbImage::new(canvas_w, canvas_h);
    let mut rects = Vec::new();
    for (row, images) in rows.iter().enumerate() {
        let top = y_bounds[row];
        let bottom = y_bounds[row + 1];
        if bottom <= top {
            return Err(GridError::ZeroCell {
                row,
                col: 0,
                axis: "height",
            });
        }
        let cell_h = bottom - top;

        let sum = aspect_sums[row];
        let mut width_acc = 0.0;
        let mut left = 0u32;
        let last_col = images.len() - 1;
        for (col, img) in images.iter().enumerate() {
            width_acc += aspects[row][col];
            let right = if col == last_col {
                canvas_w
            } else {
                (f64::from(canvas_w) * width_acc / sum).round() as u32
            };
            if right <= left {
                return Err(GridError::ZeroCell {
                    row,
                    col,
                    axis: "width",
                });
            }
            let cell_w = right - left;

            let resized = img
                .resize_exact(cell_w, cell_h, FilterType::Triangle)
                .to_rgb8();
            image::imageops::replace(&mut canvas, &resized, i64::from(left), i64::from(top));

            rects.push(ProportionalRect::new(
                f64::from(left) / f64::from(canvas_w),
                f64::from(top) / f64::from(canvas_h),
                f64::from(right) / f64::from(canvas_w),
                f64::from(bottom) / f64::from(canvas_h),
            )?);
            left = right;
        }
    }

    Ok((DynamicImage::ImageRgb8(canvas), rects))
}

// ---------------------------------------------------------------------------
// Rectangle overlap scoring
// ---------------------------------------------------------------------------

/// Intersection over union of two proportional rectangles.
///
/// When both rectangles have zero area their union is zero; that degenerate pair
/// scores 1 if they denote the same point and 0 otherwise, mirroring the
/// mask `IoU` convention that two empty regions are identical.
fn rect_iou(a: &ProportionalRect, b: &ProportionalRect) -> f64 {
    let left = a.x().max(b.x());
    let top = a.y().max(b.y());
    let right = (a.x() + a.width()).min(b.x() + b.width());
    let bottom = (a.y() + a.height()).min(b.y() + b.height());
    let intersection = (right - left).max(0.0) * (bottom - top).max(0.0);
    let union = a.width() * a.height() + b.width() * b.height() - intersection;
    if union <= 0.0 {
        return if a == b { 1.0 } else { 0.0 };
    }
    intersection / union
}

/// One detected rectangle paired with a ground-truth rectangle, and the pair's
/// `IoU`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct RectMatch {
    detected: usize,
    truth: usize,
    iou: f64,
}

/// Every permutation of `items`.
fn permutations(items: &[usize]) -> Vec<Vec<usize>> {
    if items.is_empty() {
        return vec![Vec::new()];
    }
    let mut result = Vec::new();
    for (index, &head) in items.iter().enumerate() {
        let mut rest = items.to_vec();
        rest.remove(index);
        for mut tail in permutations(&rest) {
            let mut perm = Vec::with_capacity(items.len());
            perm.push(head);
            perm.append(&mut tail);
            result.push(perm);
        }
    }
    result
}

/// The one-to-one matching of detected to ground-truth rectangles that maximizes
/// total `IoU`, found by exhaustive permutation.
///
/// Panel counts are small (at most four), so the search is cheap and, unlike a
/// greedy nearest-match, never assigns two detections to the same truth. The
/// shorter list is matched in full against a subset of the longer; matches come
/// back ordered by truth index.
fn max_iou_matching(detected: &[ProportionalRect], truth: &[ProportionalRect]) -> Vec<RectMatch> {
    let pairs = detected.len().min(truth.len());
    if pairs == 0 {
        return Vec::new();
    }
    let detected_is_short = detected.len() <= truth.len();
    let long_len = detected.len().max(truth.len());
    let long_indices: Vec<usize> = (0..long_len).collect();

    let score = |assign: &[usize]| -> f64 {
        (0..pairs)
            .map(|k| {
                let (d, t) = if detected_is_short {
                    (k, assign[k])
                } else {
                    (assign[k], k)
                };
                rect_iou(&detected[d], &truth[t])
            })
            .sum()
    };

    let mut best_total = f64::NEG_INFINITY;
    let mut best_assign: Vec<usize> = Vec::new();
    for perm in permutations(&long_indices) {
        let assign = &perm[..pairs];
        let total = score(assign);
        if total > best_total {
            best_total = total;
            best_assign = assign.to_vec();
        }
    }

    let mut matches: Vec<RectMatch> = (0..pairs)
        .map(|k| {
            let (d, t) = if detected_is_short {
                (k, best_assign[k])
            } else {
                (best_assign[k], k)
            };
            RectMatch {
                detected: d,
                truth: t,
                iou: rect_iou(&detected[d], &truth[t]),
            }
        })
        .collect();
    matches.sort_by_key(|m| m.truth);
    matches
}

/// The proportional rectangle a subimage region carries.
fn region_rect(region: &SubimageRegion) -> ProportionalRect {
    let SubimageRegion::Rect { rect } = region;
    *rect
}

// ---------------------------------------------------------------------------
// Deterministic tests (run in `just check`)
// ---------------------------------------------------------------------------

/// Whether two coordinates agree to within floating-point slack.
fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn grid_rects_partition_the_canvas_and_bound_their_fill_color() -> Result<(), Box<dyn error::Error>>
{
    // Each source is a distinct solid color, so after composition every pixel's
    // color names which panel owns it. Reading the canvas back proves the returned
    // rects tile it (each pixel owned by exactly one) and bound exactly the region
    // of the matching fill; that is the ground truth being exact, checked by
    // observation rather than by rerunning the generator's arithmetic.
    let palette: [Rgb<u8>; 5] = [
        Rgb([200, 30, 30]),
        Rgb([30, 200, 30]),
        Rgb([30, 30, 200]),
        Rgb([200, 200, 30]),
        Rgb([200, 30, 200]),
    ];
    // Two rows of unequal column count and mixed aspect ratios, so the seams are
    // off-center both ways.
    let row_dims = [
        vec![(40u32, 30u32), (70, 30)],
        vec![(30u32, 50u32), (50, 50), (20, 50)],
    ];

    let mut fills: Vec<Rgb<u8>> = Vec::new();
    let mut rows: Layout = Vec::new();
    for cells in row_dims {
        let mut row = Vec::new();
        for (w, h) in cells {
            let color = *palette
                .get(fills.len())
                .ok_or("palette smaller than cell count")?;
            fills.push(color);
            row.push(DynamicImage::ImageRgb8(RgbImage::from_pixel(w, h, color)));
        }
        rows.push(row);
    }

    let (canvas, rects) = create_image_grid(rows)?;
    assert_eq!(
        rects.len(),
        fills.len(),
        "one rect per source cell was expected"
    );

    let canvas = canvas.to_rgb8();
    let (width, height) = canvas.dimensions();
    // Snap each proportional rect back to its pixel cell. Dividing an integer
    // boundary by the canvas size then multiplying back recovers the same integer.
    let cells: Vec<(u32, u32, u32, u32)> = rects
        .iter()
        .map(|r| {
            let x0 = (r.x() * f64::from(width)).round() as u32;
            let y0 = (r.y() * f64::from(height)).round() as u32;
            let x1 = ((r.x() + r.width()) * f64::from(width)).round() as u32;
            let y1 = ((r.y() + r.height()) * f64::from(height)).round() as u32;
            (x0, y0, x1, y1)
        })
        .collect();

    for py in 0..height {
        for px in 0..width {
            let mut owner = None;
            let mut owners = 0;
            for (index, &(x0, y0, x1, y1)) in cells.iter().enumerate() {
                if px >= x0 && px < x1 && py >= y0 && py < y1 {
                    owners += 1;
                    owner = Some(index);
                }
            }
            assert_eq!(
                owners, 1,
                "pixel ({px}, {py}) is covered by {owners} rects, not exactly one"
            );
            let index = owner.ok_or("covered pixel had no owner")?;

            // Interior pixels must carry the panel's fill. The one-pixel inset keeps
            // the claim off shared boundaries, where a neighbor abuts.
            let (x0, y0, x1, y1) = cells[index];
            let strictly_inside = px > x0 && px + 1 < x1 && py > y0 && py + 1 < y1;
            if strictly_inside {
                let actual = canvas.get_pixel(px, py);
                assert_eq!(
                    *actual, fills[index],
                    "pixel ({px}, {py}) inside panel {index} is {actual:?}, not its fill {:?}",
                    fills[index]
                );
            }
        }
    }
    Ok(())
}

#[test]
fn rect_iou_scores_overlap_and_the_degenerate_cases() -> Result<(), Box<dyn error::Error>> {
    let a = ProportionalRect::new(0.2, 0.3, 0.6, 0.7)?;
    assert!(
        approx(rect_iou(&a, &a), 1.0),
        "a rect scores 1 against itself"
    );

    let left = ProportionalRect::new(0.0, 0.0, 0.5, 1.0)?;
    let right = ProportionalRect::new(0.5, 0.0, 1.0, 1.0)?;
    assert!(
        approx(rect_iou(&left, &right), 0.0),
        "disjoint halves score 0"
    );

    // Two equal-area halves offset by a quarter: intersection 0.25, union 0.75.
    let shifted = ProportionalRect::new(0.25, 0.0, 0.75, 1.0)?;
    let iou = rect_iou(&left, &shifted);
    assert!(
        approx(iou, 1.0 / 3.0),
        "quarter-overlap of equal halves is one third, got {iou}"
    );

    // Zero-area rects: identical points share their (empty) region, distinct points
    // do not, and a point against the full frame does not.
    let point = ProportionalRect::new(0.5, 0.5, 0.5, 0.5)?;
    let elsewhere = ProportionalRect::new(0.7, 0.7, 0.7, 0.7)?;
    assert!(
        approx(rect_iou(&point, &point), 1.0),
        "a point matches itself"
    );
    assert!(
        approx(rect_iou(&point, &elsewhere), 0.0),
        "distinct points share nothing"
    );
    assert!(
        approx(rect_iou(&point, &ProportionalRect::full()), 0.0),
        "a point does not fill the frame"
    );
    Ok(())
}

#[test]
fn max_iou_matching_recovers_a_swapped_pairing() -> Result<(), Box<dyn error::Error>> {
    // Detected rects given in the opposite order to the truth; the matching must
    // pair each with its true partner rather than assuming index order.
    let left = ProportionalRect::new(0.0, 0.0, 0.5, 1.0)?;
    let right = ProportionalRect::new(0.5, 0.0, 1.0, 1.0)?;
    let truth = [left, right];
    let detected = [right, left];

    let matches = max_iou_matching(&detected, &truth);
    assert_eq!(matches.len(), 2);
    // Ordered by truth: truth 0 (left) matches detected 1 (left); truth 1 matches
    // detected 0.
    assert_eq!(matches[0].truth, 0);
    assert_eq!(matches[0].detected, 1);
    assert!(approx(matches[0].iou, 1.0));
    assert_eq!(matches[1].truth, 1);
    assert_eq!(matches[1].detected, 0);
    assert!(approx(matches[1].iou, 1.0));
    Ok(())
}

#[test]
fn max_iou_matching_does_not_double_book_one_truth() -> Result<(), Box<dyn error::Error>> {
    // Both detections overlap truth A more than truth B (a greedy nearest-match
    // would assign both to A and orphan B). The bijection must send the weaker
    // detection to B instead.
    let truth_a = ProportionalRect::new(0.0, 0.0, 0.5, 1.0)?;
    let truth_b = ProportionalRect::new(0.5, 0.0, 1.0, 1.0)?;
    let exact_a = ProportionalRect::new(0.0, 0.0, 0.5, 1.0)?;
    let leans_a = ProportionalRect::new(0.1, 0.0, 0.6, 1.0)?;
    let truth = [truth_a, truth_b];
    let detected = [exact_a, leans_a];

    let matches = max_iou_matching(&detected, &truth);
    let matched_truths: BTreeSet<usize> = matches.iter().map(|m| m.truth).collect();
    assert_eq!(
        matched_truths,
        BTreeSet::from([0, 1]),
        "both detections were booked onto the same truth"
    );
    let for_a = matches
        .iter()
        .find(|m| m.truth == 0)
        .ok_or("no detection matched truth A")?;
    assert_eq!(for_a.detected, 0, "the exact match should win truth A");
    Ok(())
}

#[test]
fn empty_and_sliver_grids_are_rejected() -> Result<(), Box<dyn error::Error>> {
    assert!(matches!(create_image_grid(vec![]), Err(GridError::NoRows)));
    assert!(matches!(
        create_image_grid(vec![vec![]]),
        Err(GridError::EmptyRow { row: 0 })
    ));
    Ok(())
}

// ---------------------------------------------------------------------------
// End-to-end model test (needs the Qwen weights and the corpus)
// ---------------------------------------------------------------------------

/// The env var naming the corpus link farm the `analysis` shell sets, keyed by
/// bare entry id with no file extension.
const CORPUS_ENV: &str = "CORPUS_IMAGES";

/// The largest a detected panel edge may sit from ground truth, in proportional
/// units. The per-edge error is printed for every layout regardless, so a model
/// bump reads as a number rather than only a pass or fail.
const SEAM_TOLERANCE: f64 = 0.02;

fn corpus_dir() -> Result<PathBuf, String> {
    env::var_os(CORPUS_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            format!(
                "{CORPUS_ENV} is unset. It names the corpus link farm the `analysis` shell \
                 exports; enter that shell (or run `just model-test`) so the panel sources resolve."
            )
        })
}

/// Loads a corpus single by its bare id. The files carry no extension, so the
/// format is sniffed from the bytes rather than guessed from the path.
fn load_corpus(dir: &Path, id: &str) -> Result<DynamicImage, Box<dyn error::Error>> {
    let path = dir.join(id);
    let reader = image::ImageReader::open(&path)
        .map_err(|e| format!("could not open corpus image {}: {e}", path.display()))?
        .with_guessed_format()
        .map_err(|e| format!("could not sniff the format of {}: {e}", path.display()))?;
    let image = reader
        .decode()
        .map_err(|e| format!("could not decode {}: {e}", path.display()))?;
    Ok(image)
}

/// Diagnostics for a failed panel count or seam check: the ground truth, the raw
/// outcome (a collapsed one-panel composite and a genuine single are otherwise
/// indistinguishable in the subimages), and an overlay written to a scratch path.
/// Best-effort so it can run inside an assertion message.
fn diagnose(
    name: &str,
    canvas: &DynamicImage,
    subs: &[SubimageRegion],
    truth: &[ProportionalRect],
    raw: &CompositeOutcome,
) -> String {
    let overlay = draw_subimages(canvas, subs);
    let path = env::temp_dir().join(format!("composite-detection-{name}.png"));
    let overlay_note = match overlay.save(&path) {
        Ok(()) => path.display().to_string(),
        Err(e) => format!("<overlay could not be written: {e}>"),
    };
    format!(
        "layout {name}: {} subimages vs {} panels placed. ground truth: {truth:?}. \
         raw outcome: {raw:?}. overlay: {overlay_note}",
        subs.len(),
        truth.len()
    )
}

#[test]
#[ignore = "needs the Qwen 3.6 weights and the corpus; run `just model-test`"]
fn composite_layouts_report_their_panels() -> Result<(), Box<dyn error::Error>> {
    let shard = first_shard()?;
    let corpus = corpus_dir()?;

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let model = Qwen3::open(shard.as_path()).await.map_err(|source| {
            format!(
                "could not open the model from {shard:?}: {}",
                describe(&source)
            )
        })?;

        // Distinct-scene corpus singles, resized to impose the aspect each layout
        // wants (the scene is stretched; only the panel geometry matters here).
        let arc = load_corpus(&corpus, "arc-de-triomphe")?;
        let taj = load_corpus(&corpus, "taj-mahal-front")?;
        let angkor = load_corpus(&corpus, "angkor-wat-moat")?;
        let mosque = load_corpus(&corpus, "blue-mosque-istanbul")?;

        let landscape = |img: &DynamicImage| img.resize_exact(512, 288, FilterType::Lanczos3);
        let portrait = |img: &DynamicImage| img.resize_exact(384, 512, FilterType::Lanczos3);
        let square = |img: &DynamicImage| img.resize_exact(400, 400, FilterType::Lanczos3);
        let landscape_even = |img: &DynamicImage| img.resize_exact(512, 384, FilterType::Lanczos3);

        // A fixed, sequential order pins the prefix-cache reuse across layouts.
        let layouts: [(&str, Layout); 4] = [
            (
                "side_by_side_equal",
                vec![vec![landscape_even(&arc), landscape_even(&taj)]],
            ),
            (
                "side_by_side_portrait_landscape",
                vec![vec![portrait(&arc), landscape(&taj)]],
            ),
            (
                "vertical_unequal_heights",
                vec![vec![landscape(&angkor)], vec![portrait(&mosque)]],
            ),
            (
                "grid_2x2_aligned",
                vec![
                    vec![square(&arc), square(&taj)],
                    vec![square(&angkor), square(&mosque)],
                ],
            ),
        ];

        let mut failures: Vec<String> = Vec::new();

        for (name, rows) in layouts {
            let panels: usize = rows.iter().map(Vec::len).sum();
            let (canvas, truth) =
                create_image_grid(rows).map_err(|e| format!("layout {name}: {e}"))?;

            let composite = match detect_composite(&model, canvas.clone())
                .await
                .map_err(|e| format!("layout {name}: ask failed: {}", describe(&e)))?
            {
                Outcome::Parsed(composite) => composite,
                Outcome::Incomplete { finish, raw_prefix } => {
                    failures.push(format!(
                        "layout {name}: the model stopped early ({finish}) with prefix: {raw_prefix}"
                    ));
                    continue;
                }
            };

            let subs = subimages(&composite).map_err(|e| format!("layout {name}: {e}"))?;
            if subs.len() != panels {
                failures.push(diagnose(name, &canvas, &subs, &truth, &composite));
                continue;
            }

            let detected: Vec<ProportionalRect> = subs.iter().map(region_rect).collect();
            let matching = max_iou_matching(&detected, &truth);

            let mut worst_edge = 0.0_f64;
            for m in &matching {
                let d = detected[m.detected];
                let t = truth[m.truth];
                let edges = [
                    ("left", (d.x() - t.x()).abs()),
                    ("top", (d.y() - t.y()).abs()),
                    ("right", ((d.x() + d.width()) - (t.x() + t.width())).abs()),
                    ("bottom", ((d.y() + d.height()) - (t.y() + t.height())).abs()),
                ];
                for (edge, err) in edges {
                    worst_edge = worst_edge.max(err);
                    println!("  {name}: detected {} vs truth {} {edge} off by {err:.4}", m.detected, m.truth);
                }
                println!("  {name}: detected {} vs truth {} IoU {:.4}", m.detected, m.truth, m.iou);
            }
            if worst_edge > SEAM_TOLERANCE {
                failures.push(format!(
                    "layout {name}: worst panel edge off by {worst_edge:.4}, over the {SEAM_TOLERANCE} tolerance. {}",
                    diagnose(name, &canvas, &subs, &truth, &composite)
                ));
            }
        }

        // Negative control: a genuine single must stay one full-frame subimage, so
        // a split-everything degenerate is caught. After the sub-two-panel collapse
        // "model said single" and "model emitted one panel" both land here, so the
        // raw outcome rides in the diagnostic.
        let single = load_corpus(&corpus, "machu-picchu")?;
        match detect_composite(&model, single)
            .await
            .map_err(|e| format!("negative control: ask failed: {}", describe(&e)))?
        {
            Outcome::Parsed(composite) => {
                let subs = subimages(&composite)?;
                let full = SubimageRegion::rect(0.0, 0.0, 1.0, 1.0)?;
                if subs.len() != 1 || subs[0] != full {
                    failures.push(format!(
                        "negative control: a single image did not stay one full frame; subimages: {subs:?}; raw outcome: {composite:?}"
                    ));
                }
            }
            Outcome::Incomplete { finish, raw_prefix } => {
                failures.push(format!(
                    "negative control: the model stopped early ({finish}) with prefix: {raw_prefix}"
                ));
            }
        }

        assert!(
            failures.is_empty(),
            "{} check(s) failed:\n{}",
            failures.len(),
            failures.join("\n")
        );

        Ok::<(), Box<dyn error::Error>>(())
    })
}
