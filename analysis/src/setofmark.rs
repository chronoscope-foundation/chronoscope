//! Set-of-Mark overlay: the second image a "describe by number" VLM call reads.
//!
//! A subimage's post-processed SAM 3 regions are marked so the VLM can name each
//! by its number without hiding what it is describing. Each region is drawn as a
//! colored **hatch** — opaque lines over the region, its gaps left as the true
//! pixels — plus a bold boundary **contour** and a numbered **disc**. A solid
//! translucent fill was measured to leak its color into the model's color read
//! (the described color tracked the mark, not the object), so the fill is opaque
//! lines covering only part of the surface, leaving the true color and detail
//! visible between them. Identity rides three redundant channels — the region's
//! Kelly color, its hatch angle, and the number — so a pale color stays legible by
//! its angle and disc alone. The disc sits at a point guaranteed inside the
//! region, its radius bounded by the inscribed circle, its label sized to read.

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

/// Hatch geometry: a colored line every `HATCH_PERIOD` px, `HATCH_LINE_WIDTH` px
/// wide, so roughly a third of the region's pixels carry the mark color and the
/// rest stay true. `HATCH_ANGLES` is how many distinct line directions the angle
/// channel cycles through (diagonal both ways, horizontal, vertical, cross), so a
/// region's pattern identifies it even when its color is pale.
const HATCH_PERIOD: i32 = 10;
const HATCH_LINE_WIDTH: i32 = 3;
const HATCH_ANGLES: usize = 5;

/// Boundary contour half-thickness: a filled dot of this radius stamped at each
/// edge pixel, so the region's extent reads at a glance even where the hatch is
/// sparse.
const CONTOUR_RADIUS: i32 = 2;

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

/// Whether pixel `(x, y)` lies on a hatch line for a region drawn at `angle`
/// (`index % HATCH_ANGLES`): diagonal both ways, horizontal, vertical, or cross.
/// The angle is the second identity channel, so two adjacent regions read apart
/// even if their colors are close.
fn hatch_hit(x: u32, y: u32, angle: usize) -> bool {
    let (x, y) = (x as i32, y as i32);
    let on = |coord: i32| coord.rem_euclid(HATCH_PERIOD) < HATCH_LINE_WIDTH;
    match angle {
        0 => on(x + y),
        1 => on(x - y),
        2 => on(y),
        3 => on(x),
        _ => on(x + y) || on(x - y),
    }
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

/// Paint `regions` over `image` as numbered Set-of-Mark overlays: a colored hatch
/// and boundary contour per region, plus a numbered disc at its pole of
/// inaccessibility. Colors follow the input order (already ranked by the
/// postprocess step) and cycle through the twenty-color Kelly palette.
///
/// A region whose grid disagrees with the image dimensions denotes pixels of a
/// different frame, so it is skipped rather than drawn at the wrong scale.
pub fn annotate(image: &RgbImage, regions: &[ScoredRegion], font: &impl Font) -> RgbImage {
    let mut out = image.clone();
    let (width, height) = (image.width(), image.height());
    // One foreground mask reused across regions: each region marks and clears
    // only its own pixels, so the contour's neighbor test stays an O(1) lookup
    // without densifying the whole grid once per region.
    let mut mask = vec![false; width as usize * height as usize];

    for (index, scored) in regions.iter().enumerate() {
        let region = &scored.region;
        if region.width() != width || region.height() != height {
            continue;
        }
        let color = KELLY_COLORS[index % KELLY_COLORS.len()];

        let w = width as usize;
        let angle = index % HATCH_ANGLES;
        for pixel in region.foreground_pixels() {
            let (x, y) = ((pixel % w) as u32, (pixel / w) as u32);
            if hatch_hit(x, y, angle) {
                out.put_pixel(x, y, color);
            }
        }
        draw_contour(&mut out, region, color, &mut mask);
        draw_marker(&mut out, region, index, color, font);
    }
    out
}

/// Stamps the region's boundary in its color: a filled dot of `CONTOUR_RADIUS` at
/// each edge pixel (a foreground pixel with a background 4-neighbor), so the
/// region's extent reads at a glance even where the hatch is sparse.
///
/// `mask` is a shared scratch buffer sized to the image; the region's foreground
/// is written into it for the neighbor test and cleared again on the way out, so
/// it arrives and leaves all-false.
fn draw_contour(canvas: &mut RgbImage, region: &Region, color: Rgb<u8>, mask: &mut [bool]) {
    let (width, height) = (region.width(), region.height());
    let w = width as usize;
    for pixel in region.foreground_pixels() {
        mask[pixel] = true;
    }
    let foreground = |x: i32, y: i32| {
        x >= 0
            && y >= 0
            && (x as u32) < width
            && (y as u32) < height
            && mask[(y as usize) * w + (x as usize)]
    };
    for pixel in region.foreground_pixels() {
        let (x, y) = ((pixel % w) as i32, (pixel / w) as i32);
        if !foreground(x - 1, y)
            || !foreground(x + 1, y)
            || !foreground(x, y - 1)
            || !foreground(x, y + 1)
        {
            draw_filled_circle_mut(canvas, (x, y), CONTOUR_RADIUS, color);
        }
    }
    for pixel in region.foreground_pixels() {
        mask[pixel] = false;
    }
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
    fn annotate_hatches_the_region_and_stamps_the_pole() -> TestResult {
        let font = font_from_env()?;
        let (width, height) = (40u32, 40u32);
        let base = Rgb([100u8, 110, 120]);
        let image = RgbImage::from_pixel(width, height, base);

        let dims = Dimensions::new(width, height)?;
        let dense = rectangle(dims, 5..35, 10..30);
        let region = Region::from_dense(dims, &dense)?.ok_or("non-empty region")?;
        let regions = vec![ScoredRegion {
            region: region.clone(),
            score: 0.9,
        }];

        let out = annotate(&image, &regions, &font);
        let color = KELLY_COLORS[0];
        let w = width as usize;

        // The hatch is partial: some region pixels take the mark color (hatch
        // lines, contour, disc), and some keep the true pixel (the gaps between
        // lines), so the object stays visible — the whole point over a solid fill.
        let (mut marked, mut untouched) = (0u32, 0u32);
        for pixel in region.foreground_pixels() {
            let (x, y) = ((pixel % w) as u32, (pixel / w) as u32);
            match *out.get_pixel(x, y) {
                p if p == color => marked += 1,
                p if p == base => untouched += 1,
                _ => {} // disc ring / label pixels are neither the mark nor the base
            }
        }
        assert!(marked > 0, "the mark color appears on the region");
        assert!(untouched > 0, "gaps keep the true pixel");

        // A pixel well outside the region is untouched.
        assert_eq!(*out.get_pixel(2, 2), base);

        // The pole pixel is under the drawn disc, so it is no longer the base.
        let ((cx, cy), _) = pole_of_inaccessibility(&region);
        assert_ne!(*out.get_pixel(cx, cy), base);
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
