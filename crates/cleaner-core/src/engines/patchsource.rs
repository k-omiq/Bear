//! The Patch tool's proposed source: where a selection is filled from before
//! anybody has dragged it anywhere.
//!
//! Finding a clean source is the slow part of Photoshop's Patch tool (plan,
//! "From the videos"): the user drags the selection about until its edge
//! stops showing. Here the source is proposed, drawn as a ghost, and dragged
//! only to override it.
//!
//! A source is good when two things hold. The band of pixels around it looks
//! like the band around the selection, so the copy's edge already agrees with
//! the destination and the Poisson correction of Normal mode has little to
//! carry in; and its inside is no busier than that band, so the copy does not
//! bring lettering or a panel line of its own. The cost of an offset is the
//! mean squared difference over the band plus the inside's variance above the
//! band's, on luma.
//!
//! The search runs on a grid shrunk until the selection is about [`WORK`] px
//! across, over a lattice of about [`LATTICE`] by [`LATTICE`] offsets within
//! [`REACH`] times the selection's size, and then at every offset within one
//! lattice step of the best. An offset is a candidate only when the shifted
//! selection and its band lie wholly on the grid and miss the selection.

use super::grid::{Grid, Mask, resize_area};

/// The selection's size, in px, the search shrinks the grid to.
pub const WORK: usize = 64;
/// How far a source may be, in multiples of the selection's larger side.
pub const REACH: f64 = 3.0;
/// Offsets per axis on the coarse lattice.
pub const LATTICE: usize = 25;

/// The best source for `hole` in the luma grid `gray`, as the offset from the
/// selection to its source in `gray`'s pixels (`source = selection + offset`).
/// `None` when the hole is empty or no offset fits on the grid.
pub fn propose(gray: &Grid, hole: &Mask) -> Option<(i64, i64)> {
    let (x0, y0, x1, y1) = bounds(hole)?;
    let size = (x1 - x0 + 1).max(y1 - y0 + 1);
    let f = size.div_ceil(WORK).max(1);
    let (sw, sh) = (gray.w.div_ceil(f), gray.h.div_ceil(f));
    let (small, small_hole) = if f == 1 {
        (gray.clone(), hole.clone())
    } else {
        let cover = resize_area(&hole.as_grid(), sw, sh);
        (resize_area(gray, sw, sh), Mask { w: sw, h: sh, data: cover.data.iter().map(|&v| v > 1e-6).collect() })
    };
    let best = search(&small, &small_hole)?;
    Some((best.0 * f as i64, best.1 * f as i64))
}

fn bounds(m: &Mask) -> Option<(usize, usize, usize, usize)> {
    let mut b: Option<(usize, usize, usize, usize)> = None;
    for (i, _) in m.data.iter().enumerate().filter(|(_, &v)| v) {
        let (x, y) = (i % m.w, i / m.w);
        b = Some(match b {
            None => (x, y, x, y),
            Some((a, c, d, e)) => (a.min(x), c.min(y), d.max(x), e.max(y)),
        });
    }
    b
}

/// The coarse lattice, then the refinement around its best offset.
fn search(g: &Grid, hole: &Mask) -> Option<(i64, i64)> {
    let (x0, y0, x1, y1) = bounds(hole)?;
    let size = (x1 - x0 + 1).max(y1 - y0 + 1);
    let band = (size / 6).clamp(2, 8);
    let grown = hole.dilate(band);
    let ring_mask = grown.and_not(hole);
    let at = |i: usize| ((i % g.w) as i64, (i / g.w) as i64);
    let ring: Vec<usize> = (0..g.data.len()).filter(|&i| ring_mask.data[i]).collect();
    let inside: Vec<usize> = (0..g.data.len()).filter(|&i| hole.data[i]).collect();
    if ring.is_empty() {
        return None;
    }
    let mean = ring.iter().map(|&i| g.data[i]).sum::<f64>() / ring.len() as f64;
    let ring_var = ring.iter().map(|&i| (g.data[i] - mean).powi(2)).sum::<f64>() / ring.len() as f64;
    // The shifted selection and band must land on the grid and miss the hole.
    let (gx0, gy0, gx1, gy1) = bounds(&grown)?;
    let fits = |o: (i64, i64)| {
        let (ax, ay, bx, by) = (gx0 as i64 + o.0, gy0 as i64 + o.1, gx1 as i64 + o.0, gy1 as i64 + o.1);
        if ax < 0 || ay < 0 || bx >= g.w as i64 || by >= g.h as i64 {
            return false;
        }
        // the boxes apart is enough; otherwise check pixel by pixel
        if bx < x0 as i64 || ax > x1 as i64 || by < y0 as i64 || ay > y1 as i64 {
            return true;
        }
        ring.iter().chain(&inside).all(|&i| {
            let (x, y) = at(i);
            !hole.data[((y + o.1) as usize) * g.w + (x + o.0) as usize]
        })
    };
    let cost = |o: (i64, i64)| -> f64 {
        let shift = |i: usize| {
            let (x, y) = at(i);
            ((y + o.1) as usize) * g.w + (x + o.0) as usize
        };
        let band_err = ring.iter().map(|&i| (g.data[shift(i)] - g.data[i]).powi(2)).sum::<f64>() / ring.len() as f64;
        let busy = if inside.is_empty() {
            0.0
        } else {
            let vals: Vec<f64> = inside.iter().map(|&i| g.data[shift(i)]).collect();
            let m = vals.iter().sum::<f64>() / vals.len() as f64;
            let var = vals.iter().map(|v| (v - m).powi(2)).sum::<f64>() / vals.len() as f64;
            (var - ring_var).max(0.0)
        };
        // a whisker of distance, so of two equal sources the nearer wins
        let near = 1e-3 * ((o.0 * o.0 + o.1 * o.1) as f64) / (size * size) as f64;
        band_err + busy + near
    };
    let reach = (REACH * size as f64).round() as i64;
    let step = ((2 * reach) as usize / (LATTICE - 1)).max(1);
    let mut best: Option<((i64, i64), f64)> = None;
    let consider = |o: (i64, i64), best: &mut Option<((i64, i64), f64)>| {
        if o == (0, 0) || !fits(o) {
            return;
        }
        let c = cost(o);
        if best.is_none_or(|(_, b)| c < b) {
            *best = Some((o, c));
        }
    };
    let mut dy = -reach;
    while dy <= reach {
        let mut dx = -reach;
        while dx <= reach {
            consider((dx, dy), &mut best);
            dx += step as i64;
        }
        dy += step as i64;
    }
    let (coarse, _) = best?;
    let s = step as i64;
    for dy in -s..=s {
        for dx in -s..=s {
            consider((coarse.0 + dx, coarse.1 + dy), &mut best);
        }
    }
    best.map(|(o, _)| o)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(w: usize, h: usize, x0: usize, y0: usize, side: usize) -> Mask {
        Mask { w, h, data: (0..w * h).map(|i| (x0..x0 + side).contains(&(i % w)) && (y0..y0 + side).contains(&(i / w))).collect() }
    }

    #[test]
    fn proposes_clean_paper_beside_lettering_and_not_the_busy_side() {
        // flat 200 paper; lettering in the selection; noisy hatching to the
        // left of it; the clean paper is to the right
        let (w, h) = (240, 120);
        let mut g = Grid::new(w, h, 200.0);
        for y in 0..h {
            for x in 0..90 {
                g.data[y * w + x] = if (x + y) % 3 == 0 { 30.0 } else { 200.0 };
            }
        }
        let hole = square(w, h, 110, 40, 30);
        for i in 0..w * h {
            if hole.data[i] && (i % 4 == 0) {
                g.data[i] = 0.0;
            }
        }
        let (dx, dy) = propose(&g, &hole).expect("a source");
        // the source must lie on the clean paper right of x = 90
        assert!(110 + dx >= 92, "source at x {}", 110 + dx);
        // and miss the selection
        assert!(dx.abs() >= 30 || dy.abs() >= 30, "({dx}, {dy}) overlaps the selection");
    }

    #[test]
    fn follows_a_ramp_to_the_matching_tone() {
        // a vertical ramp: the band around the selection only matches along
        // its own rows, so the source keeps dy near zero
        let (w, h) = (300, 300);
        let g = Grid { w, h, data: (0..w * h).map(|i| (i / w) as f64 * 0.8).collect() };
        let hole = square(w, h, 130, 130, 30);
        let (_, dy) = propose(&g, &hole).expect("a source");
        assert!(dy.abs() <= 3, "dy {dy}");
    }

    #[test]
    fn large_selections_search_a_shrunk_grid_and_scale_back() {
        let (w, h) = (900, 400);
        let g = Grid::new(w, h, 128.0);
        let hole = square(w, h, 350, 100, 200);
        let (dx, dy) = propose(&g, &hole).expect("a source");
        let (sx, sy) = (350 + dx, 100 + dy);
        assert!(sx >= 0 && sy >= 0 && sx + 200 <= w as i64 && sy + 200 <= h as i64, "({sx}, {sy}) off the grid");
        assert!(dx.abs() >= 200 || dy.abs() >= 200, "({dx}, {dy}) overlaps");
    }

    #[test]
    fn no_room_means_no_source() {
        let (w, h) = (40, 40);
        let g = Grid::new(w, h, 10.0);
        assert!(propose(&g, &square(w, h, 5, 5, 30)).is_none());
        assert!(propose(&g, &Mask::new(w, h, false)).is_none());
    }
}
