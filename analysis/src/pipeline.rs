//! The analysis pipeline: an image in, the located and read entities out.
//!
//! Built one model deep at a time. Pass 1 ([`detect_composite`]) asks Qwen
//! whether the image is a single picture or a composite of several panels, and
//! the panels become the subimages the later passes read; a single image yields
//! one full-frame subimage, so the downstream passes have one shape to handle.
//!
//! The per-subimage reading pass ([`read_subimage`]) opens with an image-level
//! gate — is the subimage relevant, and if so its medium, with a viewpoint when
//! it is a photograph. A photograph or a pictorial map is then read by region
//! count: with regions, it draws the numbered Set-of-Mark overlay and asks Qwen to
//! describe each mark in turn; with none, it asks whether the segmenter missed a
//! whole structure. A cartographic map or a plan is left for the map-analysis
//! path. Every call leads with the same raw image, so they share one image
//! prefill. Each pass is exercised in isolation — cropping the parent frame and
//! threading the segmenter between the two is a later wiring stage.

use ab_glyph::Font;
use chronoscope_core::grammar::composites::SubimageRegion;
use chronoscope_core::grammar::depiction::Perspective;
use chronoscope_core::grammar::geometry::{ProportionalCoordError, ProportionalRect};
use chronoscope_core::grammar::text::Text;
use image::{DynamicImage, Rgb, RgbImage};
use thiserror::Error;

use crate::ask::convert::bbox_to_proportional;
use crate::ask::{
    CompositeOutcome, EntityReading, ImageOutcome, Outcome, Prompt, RelevantMedium, TriageOutcome,
};
use crate::qwen3::{AskError, Qwen3};
use crate::sam3::ScoredRegion;
use crate::setofmark;

/// The fixed framing for pass 1. `Qwen3::ask` appends the rendered schema.
const PASS1_PREAMBLE: &str = "\
You are analyzing an image that is either a single picture or a composite of \
several separate pictures. Your only task is to detect that layout, not to \
describe what the pictures show.

If the image is a single picture, report it as single. If it is a composite, \
give the bounding box of each picture. Find the actual boundary of each one and \
place its box exactly where that picture begins and ends; the pictures may be \
different sizes and need not be split down the middle. List them left to right, \
then top to bottom.

Coordinates are on a 0 to 1000 grid with the origin at the top-left corner: x \
runs from 0 at the left edge to 1000 at the right, y from 0 at the top edge to \
1000 at the bottom. Each box is given by its top-left and bottom-right corners.

Respond with JSON conforming to this type:";

/// The user-turn trigger after the image.
const PASS1_POSTAMBLE: &str = "Detect the layout of this image.";

/// The border colour of a drawn panel rectangle, and its thickness in pixels.
const PANEL_BORDER: Rgb<u8> = Rgb([255, 0, 255]);
const PANEL_BORDER_PX: u32 = 3;

/// Runs pass 1 over one image: is it a single picture or a composite, and where
/// are the panels.
pub async fn detect_composite(
    qwen: &Qwen3,
    image: DynamicImage,
) -> Result<Outcome<CompositeOutcome>, AskError> {
    let prompt = Prompt {
        preamble: PASS1_PREAMBLE.to_owned(),
        images: vec![image],
        postamble: PASS1_POSTAMBLE.to_owned(),
    };
    qwen.ask::<CompositeOutcome>(prompt).await
}

/// The subimages the later passes read: one per panel of a composite, or the
/// whole frame when the image is single.
///
/// `CompositeOutcome::panels` collapses a composite of fewer than two panels to
/// none, so an under-split composite lands on the whole-frame path with a single
/// picture. The result is always at least one subimage.
pub fn subimages(outcome: &CompositeOutcome) -> Result<Vec<SubimageRegion>, PanelError> {
    let panels = outcome.panels();
    if panels.is_empty() {
        return Ok(vec![SubimageRegion::Rect {
            rect: ProportionalRect::full(),
        }]);
    }
    panels
        .iter()
        .enumerate()
        .map(|(index, &panel)| {
            let rect =
                bbox_to_proportional(panel).map_err(|source| PanelError { index, source })?;
            Ok(SubimageRegion::Rect { rect })
        })
        .collect()
}

/// Draws each subimage's rectangle onto a copy of the image, so a detected
/// composite's panels are visible at a glance.
pub fn draw_subimages(image: &DynamicImage, subimages: &[SubimageRegion]) -> RgbImage {
    let mut canvas = image.to_rgb8();
    let (width, height) = canvas.dimensions();
    for SubimageRegion::Rect { rect } in subimages {
        draw_border(&mut canvas, rect, width, height);
    }
    canvas
}

/// Draws the border of one proportional rectangle onto the canvas.
fn draw_border(canvas: &mut RgbImage, rect: &ProportionalRect, width: u32, height: u32) {
    let left = to_pixel(rect.x(), width);
    let right = to_pixel(rect.x() + rect.width(), width);
    let top = to_pixel(rect.y(), height);
    let bottom = to_pixel(rect.y() + rect.height(), height);
    for x in left..=right {
        for offset in 0..PANEL_BORDER_PX {
            put(canvas, x, top.saturating_add(offset), width, height);
            put(canvas, x, bottom.saturating_sub(offset), width, height);
        }
    }
    for y in top..=bottom {
        for offset in 0..PANEL_BORDER_PX {
            put(canvas, left.saturating_add(offset), y, width, height);
            put(canvas, right.saturating_sub(offset), y, width, height);
        }
    }
}

/// Maps a proportional coordinate to a pixel index within `extent`, clamped to
/// the last valid pixel so a rect touching the far edge stays on the canvas.
fn to_pixel(fraction: f64, extent: u32) -> u32 {
    let last = f64::from(extent.saturating_sub(1));
    (fraction * f64::from(extent)).round().clamp(0.0, last) as u32
}

/// Sets one pixel when it lies on the canvas.
fn put(canvas: &mut RgbImage, x: u32, y: u32, width: u32, height: u32) {
    if x < width && y < height {
        canvas.put_pixel(x, y, PANEL_BORDER);
    }
}

/// A panel the model placed off the 0-1000 grid, so its corners could not be
/// read into the parent frame. The index is its position in the reading-order
/// panel list, so a batch failure names which panel.
#[derive(Debug, Error)]
#[error("panel {index} has coordinates off the 0-1000 grid")]
pub struct PanelError {
    pub index: usize,
    #[source]
    pub source: ProportionalCoordError,
}

/// The image-gate framing: relevance, then medium and viewpoint when relevant.
/// [`Prompt::user_text`] appends the rendered schema after it.
const GATE_PREAMBLE: &str = "\
You are examining an image to judge whether it belongs in a research archive of \
real places and the built environment, and if so how to read it.

First decide relevance. The image is relevant when it genuinely shows a real \
building, monument, bridge, street, or other built structure, or a map or plan \
of one. It is irrelevant when there is no such structure, when a structure is \
only incidental to explicit, violent, or otherwise unsuitable content, or when \
it shows a scale model, a toy, or a fictional rendering rather than a real \
place. When it is irrelevant, say why.

When it is relevant, also report its medium and its viewpoint. The viewpoint is \
exterior for a view of a structure from outside, interior for a view from \
within one.

Respond with JSON conforming to this type:";

/// The user-turn trigger after the gate image.
const GATE_POSTAMBLE: &str = "Judge this image.";

/// The per-entity describe framing: the raw scene and the numbered overlay, read
/// one mark at a time. [`Prompt::user_text`] appends the rendered schema after it.
const ENTITY_PREAMBLE: &str = "\
You are given two images of the same scene, pixel-for-pixel aligned. The first \
is the original image. The second is that same image with each detected object \
marked by a colored overlay and a numbered disc.

Critically: the overlay in the second image alters the object's apparent color \
and covers detail, so the object's true color, material, and detail cannot be \
judged from the second image at all. Determine them solely from the first image, \
at the same location. The overlay is never part of the object.

For the numbered object you are asked about: first find its number in the second \
image to see which object it marks and where it sits; then describe the object as \
it truly appears at that location in the first image. Describe the object, not the \
overlay or the number over it.

Respond with JSON conforming to this type:";

/// The recall-backstop framing over a region-less subimage: whether the detector
/// missed a whole structure. A miss is one discrete structure, not a parts list,
/// and the enclosing building of an interior does not count — the wording carries
/// that so the backstop stays sound on interior photos.
/// [`Prompt::user_text`] appends the rendered schema.
const TRIAGE_PREAMBLE: &str = "\
An automatic detector for whole built structures — buildings, monuments, and \
bridges — examined this image and marked nothing. The image is already judged \
relevant, so the only question is whether the detector missed a structure it \
should have outlined as a single object.

Report a miss only for a whole discrete structure, not for the architectural \
parts of one. If this is an interior view, the building you are inside does not \
count — the detector is not expected to outline the interior you occupy — but a \
separate building visible through a window or opening does.

If such a structure was missed, describe it. If there is genuinely nothing to \
mark, report that instead.

Respond with JSON conforming to this type:";

/// The user-turn trigger after the triage image.
const TRIAGE_POSTAMBLE: &str = "Report what the detector missed.";

/// One segmented region carrying the model's reading of it. Per-entity coverage
/// gives every region a reading, so the description is unconditional.
#[derive(Debug, Clone)]
pub struct DescribedRegion {
    /// The region the segmenter found.
    pub region: ScoredRegion,
    /// What the model said the region is.
    pub description: Text,
}

/// What the model made of a relevant subimage: its regions read one by one, or —
/// when the segmenter found none — the recall backstop's verdict.
#[derive(Debug, Clone)]
pub enum Content {
    /// The segmenter's regions, each with the model's reading.
    Described(Vec<DescribedRegion>),
    /// The subimage had no regions; whether the detector missed a real structure.
    Triaged(TriageOutcome),
}

/// The result of reading one subimage: set aside by the gate, or read by medium.
///
/// A photograph and a pictorial map both carry [`Content`] — the region flow reads
/// them the same way — but only a photograph has a viewpoint. A cartographic map or
/// a plan is an abstract depiction the building flow does not read; it carries the
/// medium alone now, to be adorned with its own analysis when that path exists.
#[derive(Debug, Clone)]
pub enum SubimageReading {
    /// The gate judged the subimage irrelevant; nothing was read.
    Irrelevant {
        /// Why the gate set it aside.
        reason: Text,
    },
    /// A photograph: its viewpoint, and its regions read (or the recall backstop).
    Picture {
        /// The image-level viewpoint.
        view: Perspective,
        /// The regions read, or the recall backstop for a region-less subimage.
        content: Content,
    },
    /// A pictorial map: read like a picture, but with no viewpoint.
    PictorialMap {
        /// The regions read, or the recall backstop for a region-less subimage.
        content: Content,
    },
    /// A cartographic map, routed to the map-analysis path (not yet built).
    Map,
    /// An orthographic plan, routed to the map-analysis path (not yet built).
    Plan,
}

/// Why a subimage could not be read.
#[derive(Debug, Error)]
pub enum ReadError {
    /// A constrained ask failed outright.
    #[error(transparent)]
    Ask(#[from] AskError),
    /// A call stopped before completing a value. Names which call, the finish
    /// reason, and the partial output, so a truncation is diagnosable rather than
    /// read as a benign result.
    #[error("the {call} call stopped early ({finish}); partial output: {raw_prefix}")]
    Incomplete {
        /// Which call stopped short (`gate`, `entity {i}`, or `triage`).
        call: String,
        /// The finish reason mistral.rs reported.
        finish: String,
        /// The partial output emitted before stopping.
        raw_prefix: String,
    },
    /// A region's grid disagrees with the subimage it is read against. Rejected
    /// at the boundary: `annotate` skips a mismatched region while the describe
    /// loop still asks about its index, which would desync the marks from the
    /// questions.
    #[error(
        "a region on a {region_width}x{region_height} grid does not match the \
         {image_width}x{image_height} subimage"
    )]
    RegionGrid {
        region_width: u32,
        region_height: u32,
        image_width: u32,
        image_height: u32,
    },
}

/// Rejects any region whose grid disagrees with the subimage, so the overlay
/// marks (which `annotate` skips on a mismatch) can't desync from the describe
/// loop's per-index questions. The two consumers must not own numbering
/// independently.
fn require_matching_grids(
    regions: &[ScoredRegion],
    image_width: u32,
    image_height: u32,
) -> Result<(), ReadError> {
    for scored in regions {
        if scored.region.width() != image_width || scored.region.height() != image_height {
            return Err(ReadError::RegionGrid {
                region_width: scored.region.width(),
                region_height: scored.region.height(),
                image_width,
                image_height,
            });
        }
    }
    Ok(())
}

/// Requires a completed answer, mapping an early stop to a labeled
/// [`ReadError::Incomplete`]. The one place [`Outcome`] is unwrapped, so it never
/// leaks past [`read_subimage`].
fn require_complete<T>(outcome: Outcome<T>, call: impl Into<String>) -> Result<T, ReadError> {
    match outcome {
        Outcome::Parsed(value) => Ok(value),
        Outcome::Incomplete { finish, raw_prefix } => Err(ReadError::Incomplete {
            call: call.into(),
            finish,
            raw_prefix,
        }),
    }
}

/// Reads one subimage. The image-level gate runs first over the raw image; an
/// irrelevant verdict short-circuits regardless of region count, and a
/// cartographic map or plan short-circuits too — the building flow does not read
/// the abstract media. A photograph or a pictorial map is read by region count via
/// `read_content`: with regions, the numbered Set-of-Mark overlay is drawn once
/// and each mark described in a serial loop over the shared `[raw][overlay]`
/// prefill; with none, the recall backstop asks whether a whole structure was
/// missed. The mark number is internal — the loop index the trigger names — so it
/// never leaves the stage.
pub async fn read_subimage(
    qwen: &Qwen3,
    subimage: DynamicImage,
    regions: Vec<ScoredRegion>,
    font: &impl Font,
) -> Result<SubimageReading, ReadError> {
    let gate = qwen
        .ask::<ImageOutcome>(gate_prompt(subimage.clone()))
        .await?;
    // The match is total by design: adding a medium is a compile error here, so
    // the taxonomy can't drift out of sync with the ask gate.
    match require_complete(gate, "gate")? {
        ImageOutcome::Irrelevant { reason } => Ok(SubimageReading::Irrelevant { reason }),
        ImageOutcome::Relevant { medium } => match medium {
            RelevantMedium::Picture { view } => Ok(SubimageReading::Picture {
                view,
                content: read_content(qwen, &subimage, regions, font).await?,
            }),
            RelevantMedium::PictorialMap => Ok(SubimageReading::PictorialMap {
                content: read_content(qwen, &subimage, regions, font).await?,
            }),
            RelevantMedium::Map => Ok(SubimageReading::Map),
            RelevantMedium::Plan => Ok(SubimageReading::Plan),
        },
    }
}

/// Reads a content-bearing subimage (a photograph or a pictorial map) by region
/// count: describe each numbered region over the shared `[raw][overlay]` prefill,
/// or — with no regions — run the recall backstop. Shared so a photograph and a
/// pictorial map read identically; only their carried viewpoint differs.
async fn read_content(
    qwen: &Qwen3,
    subimage: &DynamicImage,
    regions: Vec<ScoredRegion>,
    font: &impl Font,
) -> Result<Content, ReadError> {
    if regions.is_empty() {
        let triage = qwen
            .ask::<TriageOutcome>(triage_prompt(subimage.clone()))
            .await?;
        return Ok(Content::Triaged(require_complete(triage, "triage")?));
    }
    // The grid check guards numbering, which only the describe loop does; the gate,
    // triage, and map/plan paths read no marks. Checked here at the point the marks
    // are built, not at the stage boundary, so a map with off-grid regions still
    // classifies rather than erroring.
    require_matching_grids(&regions, subimage.width(), subimage.height())?;
    let overlay: DynamicImage = setofmark::annotate(&subimage.to_rgb8(), &regions, font).into();
    let mut described = Vec::with_capacity(regions.len());
    for (index, region) in regions.into_iter().enumerate() {
        let reading = qwen
            .ask::<EntityReading>(entity_prompt(subimage.clone(), overlay.clone(), index))
            .await?;
        let reading = require_complete(reading, format!("entity {index}"))?;
        described.push(DescribedRegion {
            region,
            description: reading.description_from_raw_image,
        });
    }
    Ok(Content::Described(described))
}

/// The gate prompt: the raw subimage alone, judged for relevance, medium, and view.
fn gate_prompt(subimage: DynamicImage) -> Prompt {
    Prompt {
        preamble: GATE_PREAMBLE.to_owned(),
        images: vec![subimage],
        postamble: GATE_POSTAMBLE.to_owned(),
    }
}

/// The per-entity describe prompt: the raw scene first for honest color and
/// detail, the numbered overlay second so the model can tie the number to its
/// region, and the trigger naming which mark to read. The trigger is 1-based to
/// match the overlay's disc labels, and is the only part that varies across the
/// describe loop, so the shared image and framing prefix stays byte-identical for
/// cache reuse.
fn entity_prompt(subimage: DynamicImage, overlay: DynamicImage, index: usize) -> Prompt {
    Prompt {
        preamble: ENTITY_PREAMBLE.to_owned(),
        images: vec![subimage, overlay],
        postamble: format!("Describe object {}.", index + 1),
    }
}

/// The triage prompt: the raw subimage alone, with no regions to mark.
fn triage_prompt(subimage: DynamicImage) -> Prompt {
    Prompt {
        preamble: TRIAGE_PREAMBLE.to_owned(),
        images: vec![subimage],
        postamble: TRIAGE_POSTAMBLE.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ask::Rect;
    use crate::ask::types::Point;

    fn rect(x1: f64, y1: f64, x2: f64, y2: f64) -> Rect {
        Rect {
            upper_left: Point { x: x1, y: y1 },
            lower_right: Point { x: x2, y: y2 },
        }
    }

    #[test]
    fn a_region_off_the_subimage_grid_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
        use chronoscope_core::grammar::geometry::{Dimensions, Region};
        let region = Region::from_dense(Dimensions::new(2, 2)?, &[true, false, false, false])?
            .ok_or("non-empty")?;
        let regions = vec![ScoredRegion { region, score: 0.9 }];
        // A matching grid passes; a region on a different grid is rejected at the
        // boundary, before any ask, so marks and describe questions can't desync.
        assert!(require_matching_grids(&regions, 2, 2).is_ok());
        assert!(matches!(
            require_matching_grids(&regions, 4, 4),
            Err(ReadError::RegionGrid { .. })
        ));
        Ok(())
    }

    #[test]
    fn a_single_image_is_one_full_frame_subimage() -> Result<(), Box<dyn std::error::Error>> {
        let regions = subimages(&CompositeOutcome::Single)?;
        assert_eq!(regions, vec![SubimageRegion::rect(0.0, 0.0, 1.0, 1.0)?]);
        Ok(())
    }

    #[test]
    fn an_under_split_composite_collapses_to_the_full_frame()
    -> Result<(), Box<dyn std::error::Error>> {
        // One panel is the same as no split, so `panels` collapses it and the
        // image reads as a single full frame rather than a one-panel composite.
        let one = CompositeOutcome::Composite {
            panels: vec![rect(0.0, 0.0, 1000.0, 500.0)],
        };
        assert_eq!(
            subimages(&one)?,
            vec![SubimageRegion::rect(0.0, 0.0, 1.0, 1.0)?]
        );
        Ok(())
    }

    #[test]
    fn a_vertical_composite_yields_a_panel_per_box_in_order()
    -> Result<(), Box<dyn std::error::Error>> {
        // Top and bottom halves on the 0-1000 grid scale to 0..0.5 and 0.5..1.0
        // in the parent frame, in the reading order the boxes were given.
        let composite = CompositeOutcome::Composite {
            panels: vec![
                rect(0.0, 0.0, 1000.0, 500.0),
                rect(0.0, 500.0, 1000.0, 1000.0),
            ],
        };
        assert_eq!(
            subimages(&composite)?,
            vec![
                SubimageRegion::rect(0.0, 0.0, 1.0, 0.5)?,
                SubimageRegion::rect(0.0, 0.5, 1.0, 1.0)?,
            ]
        );
        Ok(())
    }

    #[test]
    fn an_off_grid_panel_reports_its_index() {
        // A panel the model could not place (a negative corner) fails the [0, 1]
        // bound, and the error names which panel so a batch failure is diagnosable.
        let composite = CompositeOutcome::Composite {
            panels: vec![
                rect(0.0, 0.0, 1000.0, 500.0),
                rect(0.0, -10.0, 1000.0, 1000.0),
            ],
        };
        assert!(matches!(
            subimages(&composite),
            Err(PanelError { index: 1, .. })
        ));
    }
}
