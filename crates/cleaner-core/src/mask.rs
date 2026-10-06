//! A binary mask over a rectangle of a page.
//!
//! Masks are the unit the fidelity contract is stated in, so this type is
//! deliberately dull: a bbox, one byte per pixel, and no opinion about what
//! produced it. A hand-drawn mask and a fitted one are the same kind of
//! thing.

/// A rectangle in page **pixels**. Serialised as `{x, y, w, h}`, which is the
/// `bbox` in a persisted patch record.
///
/// It is **not** the `bbox` the seam carries. `ApiRegion.bbox` is in the
/// region's own normalised space - percentages of the page, 0–100
/// (`src/lib/editor/gesture.js`) - because the canvas, the Layers panel and the
/// gesture arithmetic all work in it. The conversion happens once, in
/// `src-tauri/src/library.rs`; sending these numbers across unconverted puts
/// every region off the right-hand edge of the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Rect {
    pub x: i64,
    pub y: i64,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn new(x: i64, y: i64, w: u32, h: u32) -> Self {
        Rect { x, y, w, h }
    }

    pub fn right(&self) -> i64 {
        self.x + self.w as i64
    }

    pub fn bottom(&self) -> i64 {
        self.y + self.h as i64
    }

    pub fn contains(&self, x: i64, y: i64) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    /// Grown by `by` on every side, clamped to a page of the given size. The
    /// clamp is why this takes the page: a bbox at the edge must not grow
    /// negative and then be cast to an index.
    pub fn grown(&self, by: u32, page_w: u32, page_h: u32) -> Rect {
        let by = by as i64;
        let x = (self.x - by).max(0);
        let y = (self.y - by).max(0);
        let right = (self.right() + by).min(page_w as i64);
        let bottom = (self.bottom() + by).min(page_h as i64);
        Rect {
            x,
            y,
            w: (right - x).max(0) as u32,
            h: (bottom - y).max(0) as u32,
        }
    }
}

/// One byte per pixel: 0 outside, 255 inside. A byte rather than a bit because
/// every consumer either dilates it, sums it or feeds it to a model as `u8`,
/// and the pages are small enough that packing buys nothing worth the
/// arithmetic.
///
/// A patch mask may also hold **partial coverage** between the two: a painted
/// shape's anti-aliased rim, and the resampled edge of a moved or turned
/// layer. Membership is still "not zero", so every binary reader is unchanged;
/// the compositor and the PSD layer mask are the two that read the byte as a
/// weight ([`Mask::coverage`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mask {
    pub bounds: Rect,
    pub bits: Vec<u8>,
}

impl Mask {
    pub fn empty(bounds: Rect) -> Self {
        Mask { bits: vec![0; (bounds.w as usize) * (bounds.h as usize)], bounds }
    }

    pub fn filled(bounds: Rect) -> Self {
        Mask { bits: vec![255; (bounds.w as usize) * (bounds.h as usize)], bounds }
    }

    /// Page coordinates in, membership out. Outside the bbox is outside the
    /// mask, which is what makes a mask safe to test against a whole page.
    pub fn contains(&self, x: i64, y: i64) -> bool {
        if !self.bounds.contains(x, y) {
            return false;
        }
        let local = ((y - self.bounds.y) as usize) * self.bounds.w as usize + (x - self.bounds.x) as usize;
        self.bits[local] != 0
    }

    pub fn set(&mut self, x: i64, y: i64, on: bool) {
        if !self.bounds.contains(x, y) {
            return;
        }
        let local = ((y - self.bounds.y) as usize) * self.bounds.w as usize + (x - self.bounds.x) as usize;
        self.bits[local] = if on { 255 } else { 0 };
    }

    /// How much of the pixel the mask covers, 0 to 255. Outside the bbox is 0.
    pub fn coverage(&self, x: i64, y: i64) -> u8 {
        if !self.bounds.contains(x, y) {
            return 0;
        }
        self.bits[((y - self.bounds.y) as usize) * self.bounds.w as usize + (x - self.bounds.x) as usize]
    }

    pub fn set_coverage(&mut self, x: i64, y: i64, coverage: u8) {
        if !self.bounds.contains(x, y) {
            return;
        }
        let local = ((y - self.bounds.y) as usize) * self.bounds.w as usize + (x - self.bounds.x) as usize;
        self.bits[local] = coverage;
    }

    pub fn count(&self) -> usize {
        self.bits.iter().filter(|b| **b != 0).count()
    }

    pub fn is_empty(&self) -> bool {
        self.bits.iter().all(|b| *b == 0)
    }

    /// Dilation by a chebyshev radius, clamped to the page.
    ///
    /// A square structuring element below diameter 5 and a disc above it, which
    /// is the shape rule taken from upstream. The result's bbox grows, so a
    /// dilated mask is still a mask and still answers `contains` in page
    /// coordinates.
    pub fn dilated(&self, radius: u32, page_w: u32, page_h: u32) -> Mask {
        self.dilated_within(radius, Rect::new(0, 0, page_w, page_h))
    }

    /// Dilation clamped to an arbitrary rectangle rather than to the whole
    /// page.
    ///
    /// "Every ring, mask and engine crop is clamped to its owning page
    /// rectangle, joins excepted" and the decode window are the same
    /// sentence from two sides, and this is where a mask meets it: the
    /// candidate series in
    /// [`crate::fit::fit_within`] grows inside the window
    /// [`crate::strip::decode_window`] answered, and a mask that reached past
    /// it would be measured against pixels the window never promised.
    pub fn dilated_within(&self, radius: u32, bounds: Rect) -> Mask {
        if radius == 0 {
            return self.clone();
        }
        let grown = self.bounds.grown(radius, u32::MAX, u32::MAX);
        let x = grown.x.max(bounds.x);
        let y = grown.y.max(bounds.y);
        let bounds = Rect {
            x,
            y,
            w: (grown.right().min(bounds.right()) - x).max(0) as u32,
            h: (grown.bottom().min(bounds.bottom()) - y).max(0) as u32,
        };
        let mut out = Mask::empty(bounds);
        let round = radius * 2 + 1 >= 5;
        let r = radius as i64;
        for y in bounds.y..bounds.bottom() {
            for x in bounds.x..bounds.right() {
                let mut hit = false;
                'kernel: for dy in -r..=r {
                    for dx in -r..=r {
                        if round && dx * dx + dy * dy > r * r {
                            continue;
                        }
                        if self.contains(x + dx, y + dy) {
                            hit = true;
                            break 'kernel;
                        }
                    }
                }
                if hit {
                    out.set(x, y, true);
                }
            }
        }
        out
    }

    /// Whether any pixel is in both masks. Cheap enough to use as a filter:
    /// the loop runs over the *overlap* of the two bboxes, so two masks that
    /// are nowhere near each other cost four comparisons.
    pub fn intersects(&self, other: &Mask) -> bool {
        let x0 = self.bounds.x.max(other.bounds.x);
        let y0 = self.bounds.y.max(other.bounds.y);
        let x1 = self.bounds.right().min(other.bounds.right());
        let y1 = self.bounds.bottom().min(other.bounds.bottom());
        for y in y0..y1 {
            for x in x0..x1 {
                if self.contains(x, y) && other.contains(x, y) {
                    return true;
                }
            }
        }
        false
    }

    /// **The shape a round brush actually paints**: the union of the discs
    /// swept along a polyline, in page pixels.
    ///
    /// Until it existed the only geometry that crossed the seam was
    /// the stroke's **bounding box**, so a diagonal or L-shaped stroke reached
    /// the engines as the rectangle around it - a brush that painted rectangles
    /// however round its cursor looked.
    ///
    /// Rasterised **per segment** rather than by stamping a disc per sample:
    /// each segment fills only its own capsule's bbox, so the work is
    /// proportional to the area the stroke actually covers rather than to the
    /// number of points the pointer happened to deliver. A single point is a
    /// disc, which is what a tap with the brush should be.
    ///
    /// `radius` is in page pixels because a brush is a tool on the image and
    /// not on the screen: the same stroke covers the same ink at every zoom.
    /// A radius below half a pixel still marks the pixels the path crosses -
    /// an empty mask is refused upstream, and a stroke somebody drew is never
    /// nothing.
    pub fn from_stroke(points: &[(f64, f64)], radius: f64, page_w: u32, page_h: u32) -> Mask {
        if points.is_empty() {
            return Mask::empty(Rect::new(0, 0, 0, 0));
        }
        let r = radius.max(std::f64::consts::FRAC_1_SQRT_2);
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for (x, y) in points {
            x0 = x0.min(x - r);
            y0 = y0.min(y - r);
            x1 = x1.max(x + r);
            y1 = y1.max(y + r);
        }
        let bx = (x0.floor() as i64).clamp(0, page_w as i64);
        let by = (y0.floor() as i64).clamp(0, page_h as i64);
        let bx1 = (x1.ceil() as i64 + 1).clamp(0, page_w as i64);
        let by1 = (y1.ceil() as i64 + 1).clamp(0, page_h as i64);
        let bounds = Rect::new(bx, by, (bx1 - bx).max(0) as u32, (by1 - by).max(0) as u32);
        let mut out = Mask::empty(bounds);
        if bounds.w == 0 || bounds.h == 0 {
            return out;
        }

        let r2 = r * r;
        let mut stamp = |a: (f64, f64), b: (f64, f64)| {
            let lo_x = ((a.0.min(b.0) - r).floor() as i64).max(bounds.x);
            let hi_x = ((a.0.max(b.0) + r).ceil() as i64).min(bounds.right() - 1);
            let lo_y = ((a.1.min(b.1) - r).floor() as i64).max(bounds.y);
            let hi_y = ((a.1.max(b.1) + r).ceil() as i64).min(bounds.bottom() - 1);
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let len2 = dx * dx + dy * dy;
            for y in lo_y..=hi_y {
                for x in lo_x..=hi_x {
                    // The pixel's centre, against the segment: the nearest
                    // point on it, clamped to the ends, which is what makes the
                    // caps round rather than square.
                    let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                    let t = if len2 <= f64::EPSILON {
                        0.0
                    } else {
                        (((px - a.0) * dx + (py - a.1) * dy) / len2).clamp(0.0, 1.0)
                    };
                    let (nx, ny) = (a.0 + dx * t, a.1 + dy * t);
                    if (px - nx).powi(2) + (py - ny).powi(2) <= r2 {
                        out.set(x, y, true);
                    }
                }
            }
        };

        if points.len() == 1 {
            stamp(points[0], points[0]);
        } else {
            for pair in points.windows(2) {
                stamp(pair[0], pair[1]);
            }
        }
        out
    }

    /// The union of several masks, as one mask over their combined bbox. This
    /// is `union(masks)` in the fidelity contract, and the contract makes a
    /// point of it being the **applied** masks - post-growth, pre-isolation.
    pub fn union(masks: &[&Mask], page_w: u32, page_h: u32) -> Mask {
        let Some(first) = masks.first() else {
            return Mask::empty(Rect::new(0, 0, 0, 0));
        };
        let mut x0 = first.bounds.x;
        let mut y0 = first.bounds.y;
        let mut x1 = first.bounds.right();
        let mut y1 = first.bounds.bottom();
        for mask in masks.iter().skip(1) {
            x0 = x0.min(mask.bounds.x);
            y0 = y0.min(mask.bounds.y);
            x1 = x1.max(mask.bounds.right());
            y1 = y1.max(mask.bounds.bottom());
        }
        let bounds = Rect {
            x: x0.max(0),
            y: y0.max(0),
            w: (x1.min(page_w as i64) - x0.max(0)).max(0) as u32,
            h: (y1.min(page_h as i64) - y0.max(0)).max(0) as u32,
        };
        let mut out = Mask::empty(bounds);
        for mask in masks {
            for y in mask.bounds.y..mask.bounds.bottom() {
                for x in mask.bounds.x..mask.bounds.right() {
                    if mask.contains(x, y) {
                        out.set(x, y, true);
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mask_answers_outside_its_own_bbox() {
        let mask = Mask::filled(Rect::new(10, 10, 4, 4));
        assert!(mask.contains(10, 10));
        assert!(mask.contains(13, 13));
        assert!(!mask.contains(9, 10));
        assert!(!mask.contains(14, 13));
    }

    #[test]
    fn dilation_grows_the_bbox_and_clamps_to_the_page() {
        let mut mask = Mask::empty(Rect::new(0, 0, 4, 4));
        mask.set(0, 0, true);
        let grown = mask.dilated(3, 100, 100);
        assert_eq!(grown.bounds.x, 0, "clamped at the left edge, not negative");
        assert!(grown.contains(3, 0));
        assert!(!grown.contains(4, 0), "chebyshev radius 3 reaches x=3, not x=4");
    }

    #[test]
    fn a_small_radius_is_square_and_a_larger_one_is_round() {
        let mut point = Mask::empty(Rect::new(10, 10, 1, 1));
        point.set(10, 10, true);

        // radius 1 → diameter 3 → square: the corner is in.
        assert!(point.dilated(1, 100, 100).contains(11, 11));
        // radius 3 → diameter 7 → disc: the far corner is out.
        let round = point.dilated(3, 100, 100);
        assert!(round.contains(13, 10));
        assert!(!round.contains(13, 13));
    }

    /// The clamp rule 1 states and rule 4's window supplies. A mask grown
    /// inside a decode window stops at the window, not at the page - which on a
    /// strip is the difference between a ring measured on paper and a ring
    /// measured on the gutter beside a narrower page.
    #[test]
    fn a_dilation_stops_at_its_window_and_not_only_at_the_page() {
        let mut seed = Mask::empty(Rect::new(50, 50, 4, 4));
        seed.set(52, 52, true);

        let to_the_page = seed.dilated(10, 200, 200);
        assert!(to_the_page.contains(42, 52));
        assert!(to_the_page.contains(62, 52));

        // A window that stops at x = 58 - a page edge in strip coordinates.
        let to_the_window = seed.dilated_within(10, Rect::new(0, 0, 58, 200));
        assert!(to_the_window.contains(42, 52));
        assert!(!to_the_window.contains(62, 52), "the mask grew past its window");
        assert_eq!(to_the_window.bounds.right(), 58);

        // Passing the whole page is exactly the old call, on every radius.
        for radius in [1u32, 3, 6, 10] {
            assert_eq!(
                seed.dilated(radius, 200, 200),
                seed.dilated_within(radius, Rect::new(0, 0, 200, 200))
            );
        }
    }

    #[test]
    fn a_union_covers_every_member() {
        let a = Mask::filled(Rect::new(0, 0, 4, 4));
        let b = Mask::filled(Rect::new(20, 20, 4, 4));
        let union = Mask::union(&[&a, &b], 100, 100);
        assert!(union.contains(1, 1));
        assert!(union.contains(21, 21));
        assert!(!union.contains(10, 10), "the gap between them is not covered");
    }

    /// **A stroke is round, and it is not its bounding box.** The defect is
    /// invisible in a bbox assertion - a diagonal stroke and the rectangle
    /// around it have the same bounds - so the thing to test is the corner: a
    /// pixel inside the bounding box and far from the path must stay out of
    /// the mask.
    #[test]
    fn a_diagonal_stroke_is_a_stroke_and_not_the_box_around_it() {
        let stroke = Mask::from_stroke(&[(20.0, 20.0), (80.0, 80.0)], 5.0, 200, 200);
        // On the path, at both ends and in the middle.
        assert!(stroke.contains(20, 20));
        assert!(stroke.contains(50, 50));
        assert!(stroke.contains(80, 80));
        // Inside the bounding box, nowhere near the path: the two corners the
        // rectangle would have swallowed.
        assert!(!stroke.contains(20, 80));
        assert!(!stroke.contains(80, 20));
        // Perpendicular to the path, the width is the brush's and not more.
        assert!(stroke.contains(50, 53));
        assert!(!stroke.contains(50, 62));
    }

    /// A single point is a disc of the brush's own diameter - a tap paints a
    /// dot - and the cap is round rather than square.
    #[test]
    fn one_point_paints_a_round_stamp_of_the_brushs_diameter() {
        let dot = Mask::from_stroke(&[(50.0, 50.0)], 10.0, 200, 200);
        assert!(dot.contains(50, 50));
        assert!(dot.contains(59, 50));
        assert!(!dot.contains(61, 50));
        // The corner of the square that would circumscribe it.
        assert!(!dot.contains(58, 58));
        // And the bbox is the disc's, clamped to the page.
        assert!(dot.bounds.w <= 22 && dot.bounds.h <= 22);
    }

    /// An L-shaped stroke covers both arms and neither of the two quadrants the
    /// bounding box would have added.
    #[test]
    fn an_l_shaped_stroke_keeps_its_corner_empty() {
        let l = Mask::from_stroke(&[(20.0, 20.0), (20.0, 80.0), (80.0, 80.0)], 4.0, 200, 200);
        assert!(l.contains(20, 50));
        assert!(l.contains(50, 80));
        assert!(!l.contains(70, 30), "the open quadrant is not painted");
    }

    /// A stroke that runs off the page is clamped to it, like every other mask.
    #[test]
    fn a_stroke_off_the_edge_is_clamped_to_the_page() {
        let over = Mask::from_stroke(&[(-20.0, 10.0), (10.0, 10.0)], 6.0, 60, 60);
        assert!(over.bounds.x >= 0 && over.bounds.y >= 0);
        assert!(over.bounds.right() <= 60 && over.bounds.bottom() <= 60);
        assert!(over.contains(2, 10));
    }

    /// `intersects` is the filter that decides which detected text a stroke is
    /// actually over, so it has to answer the *shape* and not the box.
    #[test]
    fn intersects_answers_the_shape_rather_than_the_bounding_box() {
        let stroke = Mask::from_stroke(&[(20.0, 20.0), (80.0, 80.0)], 4.0, 200, 200);
        let on_the_path = Mask::filled(Rect::new(48, 48, 4, 4));
        let in_the_corner = Mask::filled(Rect::new(70, 22, 6, 6));
        assert!(stroke.intersects(&on_the_path));
        assert!(!stroke.intersects(&in_the_corner));
        // Nowhere near: the cheap case.
        assert!(!stroke.intersects(&Mask::filled(Rect::new(150, 150, 4, 4))));
    }

    /// A single-point stroke with a small radius must not produce an empty mask.
    #[test]
    fn single_point_stroke_with_small_radius_rasterizes_non_empty_mask() {
        let dot = Mask::from_stroke(&[(50.0, 50.0)], 0.5, 200, 200);
        assert!(!dot.is_empty(), "a tap must produce at least one pixel disc");
        assert!(dot.count() > 0);
    }
}

