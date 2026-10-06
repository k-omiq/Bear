//! The page's own noise floor, and its strong edges.
//!
//! Two page-level measurements that every region's fit depends on, computed
//! once and handed down.

use crate::image::Raster;
use crate::mask::{Mask, Rect};

/// The size of the tiles the noise floor is measured over.
const TILE: u32 = 32;
/// The fraction of tiles taken as "the flattest decile".
const FLATTEST: f32 = 0.1;

/// The page's noise floor, in 16-bit luma.
///
/// The fail threshold is
/// `max(8.0, 2.5 × page_noise_sigma)`, "measured once per page from its
/// flattest decile". A fixed threshold is wrong because flat white on a JPEG
/// q75 raw already measures 5–12 from blocking alone.
///
/// Taken from the flattest tiles rather than from the whole page because the
/// question is *how much does paper vary here* - line art and screentone are
/// signal, and averaging them in would put the floor above every region.
pub fn page_noise_sigma(page: &Raster) -> f32 {
    let mut deviations: Vec<f32> = Vec::new();
    let mut y = 0;
    while y < page.height {
        let mut x = 0;
        while x < page.width {
            let w = TILE.min(page.width - x);
            let h = TILE.min(page.height - y);
            if w >= 8 && h >= 8 {
                deviations.push(tile_deviation(page, x, y, w, h));
            }
            x += TILE;
        }
        y += TILE;
    }
    if deviations.is_empty() {
        return 0.0;
    }
    deviations.sort_by(f32::total_cmp);
    let take = ((deviations.len() as f32 * FLATTEST).ceil() as usize).max(1);
    deviations[..take].iter().sum::<f32>() / take as f32
}

fn tile_deviation(page: &Raster, x0: u32, y0: u32, w: u32, h: u32) -> f32 {
    let mut sum = 0f64;
    let mut sum_sq = 0f64;
    let n = (w * h) as f64;
    for y in y0..y0 + h {
        for x in x0..x0 + w {
            let v = page.luma16_at(x, y) as f64;
            sum += v;
            sum_sq += v * v;
        }
    }
    let mean = sum / n;
    ((sum_sq / n) - mean * mean).max(0.0).sqrt() as f32
}

/// Where the page has edges strong enough that a mask must not grow across
/// them.
///
/// §4 asks for "a Canny of the native crop or the detector's bubble mask".
/// This is the gradient magnitude with a high threshold, which is Canny without
/// the thinning and the hysteresis - both of which exist to produce *thin
/// connected contours*, and neither of which matters to the only question asked
/// here: does this candidate mask contain a pixel that sits on a strong edge?
pub struct EdgeMap {
    width: u32,
    height: u32,
    strong: Vec<bool>,
}

/// Gradient magnitude above this fraction of the page's strongest gradient is a
/// strong edge. A bubble stroke against paper is close to the maximum; halftone
/// dots and screentone are far below it, which is what keeps a mask growing
/// through tone but not through an outline.
const EDGE_FRACTION: f32 = 0.5;

/// The Sobel gradient magnitude at one pixel, given a reader for its
/// neighbourhood.
///
/// `at(dx, dy)` answers the 16-bit luma at that offset from the pixel, and the
/// result is in the same units. The kernel is passed a closure rather than a
/// raster because the two callers do not read from the same place:
/// [`EdgeMap::sobel`] reads the page, and the decline metric
/// ([`crate::quality`]) reads the page **with one patch composited over it**,
/// which is not a raster that exists anywhere. Two Sobels in one crate would be
/// two definitions of "edge" free to drift apart, and the metric's whole claim
/// is that it measures the same quantity on both sides of a mask boundary.
pub fn sobel_magnitude(at: impl Fn(i32, i32) -> f32) -> f32 {
    let gx = -at(-1, -1) - 2.0 * at(-1, 0) - at(-1, 1)
        + at(1, -1) + 2.0 * at(1, 0) + at(1, 1);
    let gy = -at(-1, -1) - 2.0 * at(0, -1) - at(1, -1)
        + at(-1, 1) + 2.0 * at(0, 1) + at(1, 1);
    (gx * gx + gy * gy).sqrt()
}

impl EdgeMap {
    pub fn sobel(page: &Raster) -> EdgeMap {
        let (w, h) = (page.width, page.height);
        let mut magnitude = vec![0f32; (w * h) as usize];
        let mut peak = 0f32;

        for y in 1..h.saturating_sub(1) {
            for x in 1..w.saturating_sub(1) {
                let m = sobel_magnitude(|dx, dy| {
                    page.luma16_at((x as i32 + dx) as u32, (y as i32 + dy) as u32) as f32
                });
                magnitude[(y * w + x) as usize] = m;
                peak = peak.max(m);
            }
        }

        let threshold = peak * EDGE_FRACTION;
        EdgeMap {
            width: w,
            height: h,
            strong: magnitude.into_iter().map(|m| m >= threshold && m > 0.0).collect(),
        }
    }

    /// [`EdgeMap::sobel`] measured over `within` only: the gradient and its
    /// peak are taken inside that rectangle (each reading one pixel past it),
    /// and nothing outside it is a strong edge.
    ///
    /// For a fit, which only ever asks about pixels near its seed: with the
    /// peak taken over a whole bounded window, a strong edge drawn or erased
    /// anywhere in it moved the threshold and could change this region's
    /// mask, so the region depended on page it never looks at.
    pub fn sobel_within(page: &Raster, within: Rect) -> EdgeMap {
        let (w, h) = (page.width, page.height);
        let mut strong = vec![false; (w * h) as usize];
        let x0 = within.x.max(1) as u32;
        let y0 = within.y.max(1) as u32;
        let x1 = (within.right().max(0) as u32).min(w.saturating_sub(1));
        let y1 = (within.bottom().max(0) as u32).min(h.saturating_sub(1));
        let mut magnitude = Vec::new();
        let mut peak = 0f32;
        for y in y0..y1 {
            for x in x0..x1 {
                let m = sobel_magnitude(|dx, dy| {
                    page.luma16_at((x as i32 + dx) as u32, (y as i32 + dy) as u32) as f32
                });
                magnitude.push((y * w + x, m));
                peak = peak.max(m);
            }
        }
        let threshold = peak * EDGE_FRACTION;
        for (at, m) in magnitude {
            strong[at as usize] = m >= threshold && m > 0.0;
        }
        EdgeMap { width: w, height: h, strong }
    }

    /// An edge map that never fires, for a caller with no page - tests, and the
    /// manual path, where §4 says the search does not run at all.
    pub fn none(width: u32, height: u32) -> EdgeMap {
        EdgeMap { width, height, strong: vec![false; (width * height) as usize] }
    }

    pub fn is_strong(&self, x: i64, y: i64) -> bool {
        if x < 0 || y < 0 || x >= self.width as i64 || y >= self.height as i64 {
            return false;
        }
        self.strong[(y as usize) * (self.width as usize) + x as usize]
    }

    /// Whether a candidate mask covers a strong edge.
    ///
    /// Only the mask's **border** is tested, not its interior: the text strokes
    /// inside are themselves strong edges, and a test over the whole mask would
    /// reject the first candidate on every page.
    pub fn crosses(&self, mask: &Mask) -> bool {
        let bounds = mask.bounds;
        for y in bounds.y..bounds.bottom() {
            for x in bounds.x..bounds.right() {
                if !mask.contains(x, y) {
                    continue;
                }
                let border = !mask.contains(x - 1, y)
                    || !mask.contains(x + 1, y)
                    || !mask.contains(x, y - 1)
                    || !mask.contains(x, y + 1);
                if border && self.is_strong(x, y) {
                    return true;
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{BitDepth, ColorMode};

    fn gray_page(w: u32, h: u32, f: impl Fn(u32, u32) -> u8) -> Raster {
        let mut data = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            for x in 0..w {
                data.push(f(x, y));
            }
        }
        Raster {
            width: w,
            height: h,
            mode: ColorMode::Gray,
            depth: BitDepth::Eight,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None,
            color: Default::default(),
            data,
        }
    }

    #[test]
    fn a_clean_page_has_a_floor_near_zero() {
        let page = gray_page(128, 128, |_, _| 246);
        assert!(page_noise_sigma(&page) < 1.0);
    }

    #[test]
    fn the_floor_comes_from_the_flattest_tiles_not_from_the_art() {
        // Three quarters line art, one quarter flat paper. A whole-page
        // deviation would be enormous; the floor should follow the paper.
        let page = gray_page(128, 128, |x, y| {
            if x < 96 { if (x + y) % 3 == 0 { 0 } else { 255 } } else { 246 }
        });
        assert!(page_noise_sigma(&page) < 512.0, "{}", page_noise_sigma(&page));
    }

    #[test]
    fn a_bubble_stroke_is_a_strong_edge_and_screentone_is_not() {
        let stroke = gray_page(64, 64, |x, _| if (30..34).contains(&x) { 0 } else { 250 });
        let edges = EdgeMap::sobel(&stroke);
        assert!(edges.is_strong(30, 20) || edges.is_strong(29, 20) || edges.is_strong(34, 20));

        let tone = gray_page(64, 64, |x, y| if x % 8 < 3 && y % 8 < 3 { 200 } else { 250 });
        let tone_edges = EdgeMap::sobel(&tone);
        // The tone's own contrast sets the peak, so "strong" is relative - what
        // matters is that a mask growing through it is not stopped everywhere.
        let touched = (0..64)
            .flat_map(|y| (0..64).map(move |x| (x, y)))
            .filter(|(x, y)| tone_edges.is_strong(*x, *y))
            .count();
        assert!(touched < 64 * 64 / 2);
    }

    #[test]
    fn only_the_masks_border_is_tested_against_the_edge_map() {
        // Text strokes inside a mask are strong edges; a test over the whole
        // mask would reject the first candidate on every page.
        let page = gray_page(64, 64, |x, y| if (28..36).contains(&x) && (28..36).contains(&y) { 0 } else { 250 });
        let edges = EdgeMap::sobel(&page);
        let over_the_text = Mask::filled(Rect::new(20, 20, 24, 24));
        assert!(!edges.crosses(&over_the_text), "the interior was tested");
    }

    /// Over the whole page it is the page's edge map; over part of it, a
    /// stronger edge outside the part moves nothing inside it.
    #[test]
    fn edges_within_a_rectangle_ignore_the_page_outside_it() {
        // A faint stroke on the left, a much stronger one far to the right.
        let page = gray_page(96, 32, |x, _| match x { 10 => 150, 80 => 0, _ => 200 });
        let whole = Rect::new(0, 0, 96, 32);
        let all = EdgeMap::sobel(&page);
        let same = EdgeMap::sobel_within(&page, whole);
        for y in 0..32 {
            for x in 0..96 {
                assert_eq!(all.is_strong(x, y), same.is_strong(x, y), "({x}, {y})");
            }
        }
        // A one-pixel stroke's gradient peaks beside it, not on it.
        assert!(all.is_strong(79, 16), "the strong stroke is an edge on the whole page");
        assert!(!all.is_strong(9, 16), "on the whole page the faint stroke is under the peak's half");
        let near = EdgeMap::sobel_within(&page, Rect::new(0, 0, 40, 32));
        assert!(near.is_strong(9, 16), "near its seed the faint stroke is the strongest edge there is");
        assert!(!near.is_strong(79, 16), "outside the rectangle nothing is strong");
    }
}
