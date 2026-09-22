//! The passes a pipeline run is composed of, each usable and tested alone.
//!
//! Built one model deep at a time. Pass 1 ([`detect_composite`]) asks Qwen
//! whether the image is a single picture or a composite of several panels, and
//! the panels become the subimages the later passes read; a single image yields
//! one full-frame subimage, so the downstream passes have one shape to handle.
//!
//! The per-subimage reading pass opens with [`gate`] over the raw image — is the
//! subimage relevant, and if so its medium, with a viewpoint when it is a
//! photograph. A photograph or a pictorial map is then read by [`read_content`],
//! by region count: with regions, it draws the numbered Set-of-Mark overlay and
//! asks Qwen to describe each mark in turn; with none, it asks whether the
//! segmenter missed a whole structure. A cartographic map or a plan is left for
//! the map-analysis path. Every call leads with the same raw image, so they share
//! one image prefill. Each pass is exercised in isolation; chaining them, with
//! [`crop`] between pass 1 and the reads and the segmenter before them, is
//! [`Pipeline`](super::Pipeline)'s job.

use ab_glyph::Font;
use chronoscope_core::grammar::composites::SubimageRegion;
use chronoscope_core::grammar::geometry::{ProportionalCoordError, ProportionalRect};
use chronoscope_core::grammar::text::Text;
use chronoscope_core::nonempty::NonEmptyVec;
use image::{DynamicImage, Rgb, RgbImage};
use strum::VariantArray as _;
use thiserror::Error;

use crate::ask::convert::bbox_to_proportional;
use crate::ask::{CompositeOutcome, EntityReading, ImageOutcome, Outcome, Prompt, TriageOutcome};
use crate::concept::Concept;
use crate::qwen3::{AskError, Qwen3};
use crate::scene::{Detection, Scene};
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
/// picture. The count carries in the type: a caller reading every subimage
/// reads at least one, and an image with no panels is unspellable.
pub fn subimages(outcome: &CompositeOutcome) -> Result<NonEmptyVec<SubimageRegion>, PanelError> {
    let panels = outcome.panels();
    let placed = |index: usize, panel| {
        let rect = bbox_to_proportional(panel).map_err(|source| PanelError { index, source })?;
        Ok(SubimageRegion::Rect { rect })
    };

    // The first panel seeds the list, so "at least one subimage" is how the
    // value is built rather than something a caller re-checks.
    let Some((first, rest)) = panels.split_first() else {
        return Ok(NonEmptyVec::singleton(SubimageRegion::Rect {
            rect: ProportionalRect::full(),
        }));
    };
    let mut subimages = NonEmptyVec::singleton(placed(0, *first)?);
    for (offset, &panel) in rest.iter().enumerate() {
        subimages.push(placed(offset + 1, panel)?);
    }
    Ok(subimages)
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

/// The pixels `rect` covers of `image`, as an image of its own.
///
/// Corners map to pixel boundaries rather than pixel addresses, so subtracting
/// two of them counts pixels: a rect reaching the far edge keeps the frame's
/// last row and column, which an address clamped to the last pixel would drop.
pub fn crop(image: &DynamicImage, rect: ProportionalRect) -> Result<DynamicImage, DegenerateCrop> {
    let (width, height) = (image.width(), image.height());
    let (left, right) = (
        to_edge(rect.x(), width),
        to_edge(rect.max_corner().x(), width),
    );
    let (top, bottom) = (
        to_edge(rect.y(), height),
        to_edge(rect.max_corner().y(), height),
    );

    // The rect holds its corners in canonical order and `to_edge` preserves it,
    // so each far edge sits at or past its near one.
    let (box_width, box_height) = (right - left, bottom - top);
    if box_width == 0 || box_height == 0 {
        return Err(DegenerateCrop {
            left: rect.x(),
            top: rect.y(),
            right: rect.max_corner().x(),
            bottom: rect.max_corner().y(),
            image_width: width,
            image_height: height,
            box_width,
            box_height,
        });
    }
    Ok(image.crop_imm(left, top, box_width, box_height))
}

/// Maps a proportional coordinate to a pixel boundary of an `extent`-wide axis:
/// how many pixels lie before it, which is `extent` at the far edge.
fn to_edge(fraction: f64, extent: u32) -> u32 {
    let extent = f64::from(extent);
    (fraction * extent).round().clamp(0.0, extent) as u32
}

/// A rect that covers no whole pixel of the image it was read against, which a
/// rect of positive area does once it is thin beside the parent's pixels.
///
/// Refused here, where the rect and the frame are both named, rather than
/// downstream where a model reports an empty tensor instead.
#[derive(Debug, Clone, PartialEq, Error)]
#[error(
    "the rect ({left:.4}, {top:.4})-({right:.4}, {bottom:.4}) of a \
     {image_width}x{image_height} image rounds to an empty \
     {box_width}x{box_height} box"
)]
pub struct DegenerateCrop {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub image_width: u32,
    pub image_height: u32,
    pub box_width: u32,
    pub box_height: u32,
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
///
/// The classes are read off [`Concept::VARIANTS`], so the backstop is told what
/// the detector was actually prompted for.
fn triage_preamble() -> String {
    format!(
        "\
An automatic detector for whole built structures and ways ({}) examined this \
image and marked nothing. The image is already judged relevant, so the only \
question is whether the detector missed a structure it should have outlined as \
a single object.

Report a miss only for a whole discrete structure, not for the architectural \
parts of one. If this is an interior view, the building you are inside does not \
count — the detector is not expected to outline the interior you occupy — but a \
separate building visible through a window or opening does.

If such a structure was missed, describe it. If there is genuinely nothing to \
mark, report that instead.

Respond with JSON conforming to this type:",
        concept_list()
    )
}

/// The vocabulary as a bare comma-separated list, to name what the detector was
/// asked for. Reading as a parenthetical keeps every noun in the form
/// [`Concept::prompt`] gives it, so a concept whose plural or article is
/// irregular needs no agreement rule here.
fn concept_list() -> String {
    Concept::VARIANTS
        .iter()
        .map(|concept| concept.prompt())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The user-turn trigger after the triage image.
const TRIAGE_POSTAMBLE: &str = "Report what the detector missed.";

/// What the model made of a relevant subimage's marks: one reading each, or,
/// when the segmenter found none to mark, the recall backstop's verdict.
#[derive(Debug, Clone)]
pub enum Marks {
    /// One reading per region, positionally: `descriptions[i]` answers the mark
    /// numbered from `regions[i]`. The caller holds those regions and pairs them
    /// back up, which is why the order is the contract rather than a convenience.
    Described(Vec<Text>),
    /// The subimage had no regions; whether the detector missed a real structure.
    Triaged(TriageOutcome),
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
}

/// Requires a completed answer, mapping an early stop to a labeled
/// [`ReadError::Incomplete`]. The one place [`Outcome`] is unwrapped, so it never
/// leaks past [`gate`] or [`read_content`].
pub(crate) fn require_complete<T>(
    outcome: Outcome<T>,
    call: impl Into<String>,
) -> Result<T, ReadError> {
    match outcome {
        Outcome::Parsed(value) => Ok(value),
        Outcome::Incomplete { finish, raw_prefix } => Err(ReadError::Incomplete {
            call: call.into(),
            finish,
            raw_prefix,
        }),
    }
}

/// The image-level gate: whether an image is relevant at all, and if so its
/// medium, with a viewpoint when it is a photograph.
///
/// Qwen alone reads it, over the raw image, so relevance is decided before any
/// segmenter runs and a caller can gate an image it has no regions for.
pub async fn gate(qwen: &Qwen3, image: &DynamicImage) -> Result<ImageOutcome, ReadError> {
    let outcome = qwen.ask::<ImageOutcome>(gate_prompt(image.clone())).await?;
    require_complete(outcome, "gate")
}

/// Reads a content-bearing subimage (a photograph or a pictorial map) by region
/// count: describe each numbered region over the shared `[raw][overlay]` prefill,
/// or — with no regions — run the recall backstop. Shared so a photograph and a
/// pictorial map read identically; only their carried viewpoint differs.
///
/// The detections are `scene`'s own, so the overlay's marks and the describe
/// loop's per-index questions number one set of masks on one grid. The mark
/// number is internal — the loop index the trigger names — so it never leaves
/// the stage, and the readings come back in that same order for the caller to
/// pair with the detections it still holds.
///
/// The overlay draws the masks alone. Each region's account is the VLM's to
/// give here, over the raw image; the concept that found it rides along for the
/// caller.
pub async fn read_content<'b>(
    qwen: &Qwen3,
    scene: &Scene<'b>,
    detections: &[Detection<'b>],
    font: &impl Font,
) -> Result<Marks, ReadError> {
    let subimage = scene.image();
    if detections.is_empty() {
        let triage = qwen
            .ask::<TriageOutcome>(triage_prompt(subimage.clone()))
            .await?;
        return Ok(Marks::Triaged(require_complete(triage, "triage")?));
    }
    let regions = detections.iter().map(|detection| &detection.region);
    let overlay: DynamicImage = setofmark::annotate(scene, regions, font).into();
    let mut descriptions = Vec::with_capacity(detections.len());
    for index in 0..detections.len() {
        let reading = qwen
            .ask::<EntityReading>(entity_prompt(subimage.clone(), overlay.clone(), index))
            .await?;
        let reading = require_complete(reading, format!("entity {index}"))?;
        descriptions.push(reading.description_from_raw_image);
    }
    Ok(Marks::Described(descriptions))
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
        preamble: triage_preamble(),
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
    fn a_rect_crops_the_pixels_between_its_boundaries() -> Result<(), Box<dyn std::error::Error>> {
        let image = DynamicImage::new_rgb8(100, 50);

        // Columns 10 through 49 and rows 10 through 29: forty by twenty.
        let panel = crop(&image, ProportionalRect::new(0.1, 0.2, 0.5, 0.6)?)?;
        assert_eq!((panel.width(), panel.height()), (40, 20));

        // The far edge is the count of pixels, not the last index, so the whole
        // frame survives a full-frame rect.
        let whole = crop(&image, ProportionalRect::full())?;
        assert_eq!((whole.width(), whole.height()), (100, 50));
        Ok(())
    }

    #[test]
    fn a_rect_that_rounds_to_no_pixels_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
        // On a two-pixel axis both x boundaries round to zero, so this rect has
        // area yet covers no pixel.
        let image = DynamicImage::new_rgb8(2, 2);
        let error = crop(&image, ProportionalRect::new(0.1, 0.0, 0.2, 1.0)?)
            .err()
            .ok_or("a rect covering no pixel is refused")?;
        assert_eq!((error.box_width, error.box_height), (0, 2));
        Ok(())
    }

    #[test]
    fn a_single_image_is_one_full_frame_subimage() -> Result<(), Box<dyn std::error::Error>> {
        let regions = subimages(&CompositeOutcome::Single)?;
        assert_eq!(
            regions.into_vec(),
            vec![SubimageRegion::rect(0.0, 0.0, 1.0, 1.0)?]
        );
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
            subimages(&one)?.into_vec(),
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
            subimages(&composite)?.into_vec(),
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
