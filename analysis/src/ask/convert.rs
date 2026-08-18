//! Reading the model's box into core's geometry.
//!
//! The one non-trivial conversion at the ask boundary: the model's corners carry
//! a 0-1000 coordinate convention that core's proportional rect does not, so this
//! scales and validates them. The rest of the envelope (masks, embeddings,
//! provenance) is the weave's to assemble.

use chronoscope_core::grammar::geometry::{ProportionalCoordError, ProportionalRect};

use super::types::Rect;

/// The grid the model normalizes box coordinates to. Dividing by it puts a
/// coordinate in the `[0, 1]` proportional space core speaks.
const COORD_GRID: f64 = 1000.0;

/// Reads the model's labeled box corners as a [`ProportionalRect`] in its frame.
///
/// The corners are on the 0-1000 grid; scaling by `COORD_GRID` moves them into
/// `[0, 1]`. A coordinate off the grid (a negative, or past 1000) lands outside
/// `[0, 1]` after scaling and fails [`ProportionalRect::new`], so a box the model
/// could not place surfaces as an error. A full-frame corner pair is in range and
/// reads as a valid whole-image rect, since an entity may fill the frame.
pub fn bbox_to_proportional(rect: Rect) -> Result<ProportionalRect, ProportionalCoordError> {
    ProportionalRect::new(
        rect.upper_left.x / COORD_GRID,
        rect.upper_left.y / COORD_GRID,
        rect.lower_right.x / COORD_GRID,
        rect.lower_right.y / COORD_GRID,
    )
}

#[cfg(test)]
mod tests {
    use super::super::types::Point;
    use super::*;

    fn rect(x1: f64, y1: f64, x2: f64, y2: f64) -> Rect {
        Rect {
            upper_left: Point { x: x1, y: y1 },
            lower_right: Point { x: x2, y: y2 },
        }
    }

    #[test]
    fn bbox_reads_an_asymmetric_box_without_axis_cancellation() -> Result<(), ProportionalCoordError>
    {
        // Distinct, asymmetric corners on the 0-1000 grid, so a swapped axis or a
        // min/max transpose could not produce the same rect. x spans 100..700, y
        // spans 200..900, scaling to 0.1..0.7 and 0.2..0.9.
        let rect = bbox_to_proportional(rect(100.0, 200.0, 700.0, 900.0))?;
        assert_eq!(rect.x(), 0.1);
        assert_eq!(rect.y(), 0.2);
        assert_eq!(rect.width(), 0.7 - 0.1);
        assert_eq!(rect.height(), 0.9 - 0.2);
        Ok(())
    }

    #[test]
    fn an_off_grid_box_is_rejected() {
        // A model that cannot localize emits out-of-range coordinates; a negative
        // stays negative after scaling and fails the `[0, 1]` bound, so a failed
        // box surfaces as an error rather than a bogus rect.
        assert!(matches!(
            bbox_to_proportional(rect(-1.0, 0.0, 1000.0, 1000.0)),
            Err(ProportionalCoordError::OutOfBounds { .. })
        ));
    }
}
