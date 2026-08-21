//! Set-of-Mark overlay: the second image a "describe by number" VLM call reads.
//!
//! A subimage's post-processed SAM 3 regions are painted as translucent colored
//! masks, each stamped with a numbered marker at a point guaranteed to sit inside
//! the region. The VLM then names each entity by its number, so the overlay's job
//! is to keep each number readable and tied to its region: distinct colors, a
//! disc placed inside the region (its radius bounded by the inscribed circle),
//! and a label legible against its own fill, sized to read rather than to fit.

use ab_glyph::Font;
use chronoscope_core::grammar::geometry::Region;
use image::{GrayImage, Luma, Rgb, RgbImage};
use imageproc::distance_transform::euclidean_squared_distance_transform;
use imageproc::drawing::{
    draw_filled_circle_mut, draw_hollow_circle_mut, draw_text_mut, text_size,
};
use thiserror::Error;

use crate::sam3::ScoredRegion;

/// Kelly's maximally-contrasting colors (the 1965 sequence, dropping the white
/// and black that serve as fill background and text). We assume colors a human
/// tells apart easily are also easy for a VLM; we needed a palette and this is a
/// good starting point. More regions than colors wraps the selection, so a
/// repeated color means many detections, not a collision. The numbers still
/// disambiguate.
const KELLY_COLORS: [Rgb<u8>; 20] = [
    Rgb([243, 195, 0]),   // vivid yellow
    Rgb([135, 86, 146]),  // strong purple
    Rgb([243, 132, 0]),   // vivid orange
    Rgb([161, 202, 241]), // very light blue
    Rgb([190, 0, 50]),    // vivid red
    Rgb([194, 178, 128]), // grayish yellow
    Rgb([132, 132, 130]), // medium gray
    Rgb([0, 136, 86]),    // vivid green
    Rgb([230, 143, 172]), // strong purplish pink
    Rgb([0, 103, 165]),   // strong blue
    Rgb([249, 147, 121]), // strong yellowish pink
    Rgb([96, 78, 151]),   // strong violet
    Rgb([246, 166, 0]),   // vivid orange yellow
    Rgb([179, 68, 108]),  // strong purplish red
    Rgb([220, 211, 0]),   // vivid greenish yellow
    Rgb([136, 45, 23]),   // strong reddish brown
    Rgb([141, 182, 0]),   // vivid yellowish green
    Rgb([101, 69, 34]),   // deep yellowish brown
    Rgb([226, 88, 34]),   // vivid reddish orange
    Rgb([43, 61, 38]),    // dark olive green
];

/// Mask fill opacity: enough to read the color, light enough to keep the
/// underlying pixels visible so the VLM still sees what it is describing.
const MASK_ALPHA: f32 = 0.3;

/// Marker radius floor and ceiling, in pixels. The floor keeps a tiny region's
/// mark legible; the ceiling stops a large region from carrying an oversized
/// disc. Both yield to the inscribed radius so the disc always fits the region.
const MIN_MARKER_RADIUS: f64 = 8.0;
const MAX_MARKER_RADIUS: f64 = 28.0;

/// Label font em-size as a multiple of the disc radius. A font renders its
/// digits shorter than its nominal em, so this exceeds 1 to make the numeral
/// roughly fill the disc.
const LABEL_EM_PER_RADIUS: f64 = 1.4;

const WHITE: Rgb<u8> = Rgb([255, 255, 255]);
const BLACK: Rgb<u8> = Rgb([0, 0, 0]);

/// Per-channel `round((1-alpha)*base + alpha*color)`.
fn blend(base: Rgb<u8>, color: Rgb<u8>, alpha: f32) -> Rgb<u8> {
    let Rgb([br, bg, bb]) = base;
    let Rgb([cr, cg, cb]) = color;
    let mix = |b: u8, c: u8| ((1.0 - alpha) * f32::from(b) + alpha * f32::from(c)).round() as u8;
    Rgb([mix(br, cr), mix(bg, cg), mix(bb, cb)])
}

/// Black or white, whichever reads against `background`. Rec. 601 luma splits
/// the palette at mid-gray.
fn label_color(background: Rgb<u8>) -> Rgb<u8> {
    let Rgb([r, g, b]) = background;
    let luma = 0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b);
    if luma > 128.0 { BLACK } else { WHITE }
}

/// Why the marker font could not be loaded from the environment.
#[derive(Debug, Error)]
pub enum FontError {
    #[error("ANNOTATE_FONT is unset; the marker-label font path must be in the environment")]
    Unset,

    #[error("reading the font at {path} failed")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("the file at {path} is not a font ab_glyph can parse")]
    Parse {
        path: String,
        #[source]
        source: ab_glyph::InvalidFont,
    },
}

/// Load the marker font from `ANNOTATE_FONT`. Nix points this at a bold sans-serif
/// font from nixpkgs in the analysis shell and the test environment, so no font is
/// vendored into the repo.
pub fn font_from_env() -> Result<ab_glyph::FontVec, FontError> {
    let path = std::env::var_os("ANNOTATE_FONT").ok_or(FontError::Unset)?;
    let path = std::path::PathBuf::from(path);
    let display = path.display().to_string();
    let bytes = std::fs::read(&path).map_err(|source| FontError::Read {
        path: display.clone(),
        source,
    })?;
    ab_glyph::FontVec::try_from_vec(bytes).map_err(|source| FontError::Parse {
        path: display,
        source,
    })
}

/// Paint `regions` over `image` as numbered Set-of-Mark overlays: a translucent
/// colored fill per region and a numbered disc at its pole of inaccessibility.
/// Colors follow the input order (already ranked by the postprocess step) and
/// cycle through the twenty-color Kelly palette.
///
/// A region whose grid disagrees with the image dimensions denotes pixels of a
/// different frame, so it is skipped rather than drawn at the wrong scale.
pub fn annotate(image: &RgbImage, regions: &[ScoredRegion], font: &impl Font) -> RgbImage {
    let mut out = image.clone();
    let (width, height) = (image.width(), image.height());

    for (index, scored) in regions.iter().enumerate() {
        let region = &scored.region;
        if region.width() != width || region.height() != height {
            continue;
        }
        let color = KELLY_COLORS[index % KELLY_COLORS.len()];

        let w = width as usize;
        for pixel in region.foreground_pixels() {
            let (x, y) = ((pixel % w) as u32, (pixel / w) as u32);
            let base = *out.get_pixel(x, y);
            out.put_pixel(x, y, blend(base, color, MASK_ALPHA));
        }

        draw_marker(&mut out, region, index, color, font);
    }
    out
}

/// Stamp region `index`'s numbered disc at its pole of inaccessibility.
fn draw_marker(
    canvas: &mut RgbImage,
    region: &Region,
    index: usize,
    color: Rgb<u8>,
    font: &impl Font,
) {
    let ((cx, cy), radius_px) = pole_of_inaccessibility(region);
    let center = (cx as i32, cy as i32);

    // Clamp to the legible band, but never past the inscribed radius, so the disc
    // stays inside the region even when the region is smaller than the floor.
    let r = radius_px
        .min(MAX_MARKER_RADIUS)
        .max(MIN_MARKER_RADIUS.min(radius_px));
    let radius = r as i32;

    draw_filled_circle_mut(canvas, center, radius, color);
    draw_hollow_circle_mut(canvas, center, radius, WHITE);

    let label = index.to_string();
    let scale = (LABEL_EM_PER_RADIUS * r) as f32;
    let (text_w, text_h) = text_size(scale, font, &label);
    let x = center.0 - (text_w as i32) / 2;
    let y = center.1 - (text_h as i32) / 2;
    draw_text_mut(canvas, label_color(color), x, y, scale, font, &label);
}

/// The pole of inaccessibility of `region`: the interior pixel farthest from
/// the boundary, and the inscribed radius in pixels of the largest circle
/// centered there. A label sits here so it always lands inside the region, even
/// a concave or split one where the centroid can fall in a background gap.
///
/// The distance transform measures each pixel's distance to the nearest
/// non-zero pixel, so the region is written as zero and the background as
/// non-zero: a region pixel's value is then its distance to the boundary, and
/// the deepest such pixel is the pole. A one-cell border around the bounding
/// box is background, so a region pixel touching an edge measures to that edge.
fn pole_of_inaccessibility(region: &Region) -> ((u32, u32), f64) {
    let width = region.width() as usize;
    let (mut min_col, mut min_row, mut max_col, mut max_row) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for pixel in region.foreground_pixels() {
        let (col, row) = ((pixel % width) as u32, (pixel / width) as u32);
        min_col = min_col.min(col);
        max_col = max_col.max(col);
        min_row = min_row.min(row);
        max_row = max_row.max(row);
    }
    let (bw, bh) = (max_col - min_col + 1, max_row - min_row + 1);

    let mut mask = GrayImage::from_pixel(bw + 2, bh + 2, Luma([255]));
    for pixel in region.foreground_pixels() {
        let (col, row) = ((pixel % width) as u32, (pixel / width) as u32);
        mask.put_pixel(col - min_col + 1, row - min_row + 1, Luma([0]));
    }

    let dist = euclidean_squared_distance_transform(&mask);
    let (mut best, mut best_x, mut best_y) = (0.0f64, 1u32, 1u32);
    for (x, y, pixel) in dist.enumerate_pixels() {
        if pixel.0[0] > best {
            (best, best_x, best_y) = (pixel.0[0], x, y);
        }
    }
    let center = (min_col + best_x - 1, min_row + best_y - 1);
    (center, best.sqrt())
}

#[cfg(test)]
mod tests {
    use chronoscope_core::grammar::geometry::{Dimensions, Region};

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn blend_interpolates_each_channel_and_rounds() {
        // Alpha 0 returns the base untouched; alpha 1 returns the color.
        assert_eq!(blend(BLACK, WHITE, 0.0), BLACK);
        assert_eq!(blend(BLACK, WHITE, 1.0), WHITE);
        // 0.5 * 255 = 127.5, which rounds up.
        assert_eq!(blend(BLACK, WHITE, 0.5), Rgb([128, 128, 128]));
        // Mixed base and color, resolved per channel.
        assert_eq!(
            blend(Rgb([10, 20, 30]), Rgb([200, 100, 0]), 0.5),
            Rgb([105, 60, 15])
        );
    }

    /// A rectangular mask `cols` x `rows` on a `dims` grid.
    fn rectangle(
        dims: Dimensions,
        cols: std::ops::Range<u32>,
        rows: std::ops::Range<u32>,
    ) -> Vec<bool> {
        (0..(dims.width() * dims.height()))
            .map(|p| {
                let (x, y) = (p % dims.width(), p / dims.width());
                cols.contains(&x) && rows.contains(&y)
            })
            .collect()
    }

    #[test]
    fn annotate_blends_the_mask_and_stamps_the_pole() -> TestResult {
        let font = font_from_env()?;
        let (width, height) = (40u32, 40u32);
        let base = Rgb([100u8, 110, 120]);
        let image = RgbImage::from_pixel(width, height, base);

        // A rectangle whose pole is near the center, so the corner we probe for
        // the plain blend sits well outside the disc.
        let dims = Dimensions::new(width, height)?;
        let dense = rectangle(dims, 5..35, 10..30);
        let region = Region::from_dense(dims, &dense)?.ok_or("non-empty region")?;
        let regions = vec![ScoredRegion {
            region: region.clone(),
            score: 0.9,
        }];

        let out = annotate(&image, &regions, &font);

        let color = KELLY_COLORS[0];
        let blended = blend(base, color, MASK_ALPHA);

        // A foreground corner, far from the marker, takes the blended color.
        let corner = *out.get_pixel(5, 10);
        assert_eq!(corner, blended);
        assert_ne!(corner, base);

        // The pole pixel is under the drawn marker, so it is not the plain blend.
        let ((cx, cy), _) = pole_of_inaccessibility(&region);
        assert_ne!(*out.get_pixel(cx, cy), blended);
        Ok(())
    }

    #[test]
    fn pole_of_inaccessibility_lands_inside_a_split_region() -> TestResult {
        // Two separated pixels: the centroid falls in the background gap
        // between them, but the pole lands on the region.
        let grid = Dimensions::new(5, 1)?;
        let region =
            Region::from_dense(grid, &[true, false, false, false, true])?.ok_or("non-empty")?;
        let dense = region.to_dense();

        let centroid_col = (region.centroid().x() * 5.0) as usize;
        assert!(!dense[centroid_col], "centroid falls in the gap");

        let ((pole_col, _), radius) = pole_of_inaccessibility(&region);
        assert!(dense[pole_col as usize], "pole lands on the region");
        assert!(radius >= 1.0, "inscribed radius is at least one pixel");
        Ok(())
    }
}
