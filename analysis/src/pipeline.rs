//! The analysis pipeline: an image in, the located and read entities out.
//!
//! Built one model deep at a time. This is the first stage: pass 1 asks Qwen
//! whether the image is a single picture or a composite of several panels, and
//! the panels become the subimages the later passes read. A single image yields
//! one full-frame subimage, so the downstream passes have one shape to handle.

use chronoscope_core::grammar::composites::SubimageRegion;
use chronoscope_core::grammar::geometry::{ProportionalCoordError, ProportionalRect};
use image::{DynamicImage, Rgb, RgbImage};
use thiserror::Error;

use crate::ask::convert::bbox_to_proportional;
use crate::ask::{CompositeOutcome, Outcome, Prompt};
use crate::qwen3::{AskError, Qwen3};

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
