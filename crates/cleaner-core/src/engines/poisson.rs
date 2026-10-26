//! Membrane and Poisson solves over a hole.
//!
//! - [`membrane`] fills the unknown pixels with the smoothest surface through
//!   the known ones (Laplace's equation). The screentone fill carries the
//!   screen's tone across a hole with it (plan step S2 kept it over the
//!   biharmonic, fitted and PatchMatch fields), and the Patch tool's **Smooth**
//!   is this and nothing else - the "drag onto white paper" trick from
//!   Photoshop, without the paper.
//! - [`poisson`] is the Patch tool's **Normal** mode: the source's detail with
//!   the destination's edge. Laplace's equation for the difference: the
//!   result is `source + membrane(destination - source)` with the difference
//!   known on the edge, so the source's gradients are kept exactly.
//!
//! Both are solved coarse to fine: each level starts from the level below,
//! scaled up, then relaxes by four-neighbour averaging with the known pixels
//! held. Replicated borders, as in the prototype.

use super::grid::{Grid, Mask, resize_area, resize_linear};

const ITERS: usize = 200;

/// Laplace fill of the pixels outside `known` from the ones inside it.
pub fn membrane(values: &Grid, known: &Mask) -> Grid {
    membrane_iters(values, known, ITERS)
}

fn membrane_iters(values: &Grid, known: &Mask, iters: usize) -> Grid {
    let (w, h) = (values.w, values.h);
    let unknown = known.data.iter().any(|&k| !k);
    if !unknown {
        return values.clone();
    }
    let start = if w.min(h) > 32 {
        let (nw, nh) = (w / 2, h / 2);
        let small_known_f = resize_area(&known.as_grid(), nw, nh);
        // any known pixel in the block keeps it known, at their mean: a thin
        // ring must survive every halving, or the coarsest level starts at 0
        let small_known = Mask { w: nw, h: nh, data: small_known_f.data.iter().map(|&v| v > 1e-6).collect() };
        let sum = resize_area(&values.keep(known, 0.0), nw, nh);
        let small = Grid {
            w: nw,
            h: nh,
            data: (0..nw * nh)
                .map(|i| if small_known.data[i] { sum.data[i] / small_known_f.data[i].max(1e-6) } else { 0.0 })
                .collect(),
        };
        resize_linear(&membrane_iters(&small, &small_known, iters), w, h)
    } else {
        let (s, n) = values
            .data
            .iter()
            .zip(&known.data)
            .filter(|(_, &k)| k)
            .fold((0.0, 0usize), |(s, n), (&v, _)| (s + v, n + 1));
        Grid::new(w, h, if n > 0 { s / n as f64 } else { 0.0 })
    };
    let mut u = values.select(known, &start);
    let mut next = u.clone();
    for _ in 0..iters {
        for y in 0..h {
            let (yu, yd) = (y.saturating_sub(1), (y + 1).min(h - 1));
            for x in 0..w {
                let i = y * w + x;
                if known.data[i] {
                    continue;
                }
                let (xl, xr) = (x.saturating_sub(1), (x + 1).min(w - 1));
                next.data[i] = 0.25 * (u.data[yu * w + x] + u.data[yd * w + x] + u.data[y * w + xl] + u.data[y * w + xr]);
            }
        }
        std::mem::swap(&mut u, &mut next);
    }
    u
}

/// The Patch tool's Normal mode over one channel: `source`'s detail inside
/// `hole`, matched to `dest` at the hole's edge. `reach` (pixels) limits how
/// far the edge correction carries in before fading, which is what the
/// Diffusion option sets; `None` is a full Poisson blend.
pub fn poisson(dest: &Grid, source: &Grid, hole: &Mask, reach: Option<f64>) -> Grid {
    let known = hole.not();
    let diff = Grid { w: dest.w, h: dest.h, data: dest.data.iter().zip(&source.data).map(|(d, s)| d - s).collect() };
    let corr = membrane(&diff, &known);
    let fade = reach.map(|r| {
        let dist = super::grid::distance_to(&known);
        dist.data.iter().map(|d| (-d / r.max(0.5)).exp()).collect::<Vec<_>>()
    });
    let data = (0..dest.data.len())
        .map(|i| {
            if !hole.data[i] {
                return dest.data[i];
            }
            let c = corr.data[i] * fade.as_ref().map_or(1.0, |f| f[i]);
            source.data[i] + c
        })
        .collect();
    Grid { w: dest.w, h: dest.h, data }
}

/// The Diffusion option (1 to 7, Photoshop's scale) as a reach in pixels: 1
/// keeps the source's own colour a few pixels in from the edge, 7 is a full
/// Poisson blend.
pub fn diffusion_reach(diffusion: u8) -> Option<f64> {
    match diffusion.clamp(1, 7) {
        7 => None,
        d => Some(4.0 * f64::from(1u32 << (d - 1))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(w: usize, h: usize) -> Grid {
        Grid { w, h, data: (0..w * h).map(|i| 30.0 + (i / w) as f64 * 0.5 + (i % w) as f64 * 0.25).collect() }
    }

    fn hole(w: usize, h: usize, x0: usize, y0: usize, x1: usize, y1: usize) -> Mask {
        Mask { w, h, data: (0..w * h).map(|i| (x0..x1).contains(&(i % w)) && (y0..y1).contains(&(i / w))).collect() }
    }

    #[test]
    fn membrane_rebuilds_a_plane() {
        // a plane is harmonic, so the membrane reproduces it inside the hole
        let (w, h) = (80, 70);
        let truth = ramp(w, h);
        let m = hole(w, h, 20, 15, 60, 55);
        let filled = membrane(&truth.keep(&m.not(), 0.0), &m.not());
        let worst = (0..w * h).map(|i| (filled.data[i] - truth.data[i]).abs()).fold(0.0, f64::max);
        assert!(worst < 0.05, "worst {worst}");
    }

    #[test]
    fn membrane_carries_a_thin_ring_across_a_large_hole() {
        // the app's Smooth window leaves a 2 px ring round the selection
        let (w, h) = (204, 204);
        let m = hole(w, h, 2, 2, 202, 202);
        let filled = membrane(&Grid::new(w, h, 255.0), &m.not());
        let worst = filled.data.iter().map(|v| (v - 255.0).abs()).fold(0.0, f64::max);
        assert!(worst < 1.0, "worst {worst}");
    }

    #[test]
    fn poisson_on_flat_source_is_the_membrane() {
        // the video 1 trick: patching from flat paper rebuilds the gradient
        let (w, h) = (64, 64);
        let dest = ramp(w, h);
        let m = hole(w, h, 16, 16, 48, 48);
        let flat = Grid::new(w, h, 250.0);
        let out = poisson(&dest, &flat, &m, None);
        let worst = (0..w * h).map(|i| (out.data[i] - dest.data[i]).abs()).fold(0.0, f64::max);
        assert!(worst < 0.05, "worst {worst}");
    }

    #[test]
    fn poisson_keeps_source_detail() {
        let (w, h) = (64, 64);
        let dest = Grid::new(w, h, 100.0);
        let m = hole(w, h, 16, 16, 48, 48);
        let src = Grid { w, h, data: (0..w * h).map(|i| if (i % w) % 8 < 4 { 10.0 } else { 30.0 }).collect() };
        let out = poisson(&dest, &src, &m, None);
        // stripes survive with their 20 level contrast, shifted to the destination's level
        let (a, b) = (out.at(33, 32), out.at(37, 32));
        // the edge's alternating difference leaves a small ripple in the correction
        assert!((b - a - 20.0).abs() < 3.0 && (a - 90.0).abs() < 6.0, "{a} {b}");
    }
}
