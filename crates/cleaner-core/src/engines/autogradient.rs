//! The Gradient tool's **Auto** type: a selection over a gradient (a bubble
//! shaded top to bottom, a glow) filled with the gradient the page has all
//! round it, with no line and no stops to set.
//!
//! Video 2 of the plan is the case: a dark oval bubble, dark red to black,
//! with glowing white lettering. Match surroundings ([`super::gradient::fit`])
//! answers one straight or round gradient with two stops; this answers the
//! surface itself, so a gradient that eases, bends or is lit from a corner
//! still meets the page on every side.
//!
//! 1. **Fit.** A cubic surface per channel (ten terms in `x` and `y`) is
//!    fitted to a ring 2 to 26 px around the selection. The fit is robust: it
//!    starts from the plane with the least median error over random triples,
//!    so a ring that crosses the bubble's outline onto the paper still fits
//!    the bubble, then reweights (Tukey's biweight) to drop the glow and
//!    stray ink. A ring with no one surface (median error past
//!    [`MAX_NOISE`]: a screen, a busy drawing) is refused.
//! 2. **Text and structure.** Pixels far off the surface are pieces. A piece
//!    the selection touches and that lies mostly within [`NEAR`] px of it is
//!    the lettering and its glow: the fill takes it whole, so a lasso that
//!    cut through a letter or stopped inside the glow still clears it. Any
//!    other piece, or one that runs out of the window, is the page's own
//!    (the outline, the paper, the bubble's rim glow): it is never painted,
//!    even inside the selection.
//! 3. **Blend.** The surface is refitted round the grown hole, which then
//!    takes the faint tail of a glow up to [`TAIL`] px further out, and the
//!    page's difference from it at the edge is carried in by a membrane solve
//!    ([`super::poisson::membrane`]) fading over [`REACH`] px: the edge is
//!    seamless, and what is left of a glow tail there does not smear inward.
//!
//! Every value is 0 to 255; one channel (gray) or three (RGB).

use super::grid::{Grid, Mask, components, distance_to, median, percentile};
use super::poisson::membrane;

/// How far past the selection the lettering's pieces are followed, px.
pub const NEAR: usize = 48;
/// The ring the surface is fitted to: from `GAP` to `GAP + RING` px out.
const RING: usize = 24;
const GAP: usize = 2;
/// How far the edge's difference from the surface carries in, px.
const REACH: f64 = 12.0;
/// How far past the grown hole a glow's faint tail is still taken, px.
const TAIL: usize = 8;
/// A ring whose robust spread is past this has no one surface to fill with.
const MAX_NOISE: f64 = 24.0;
/// The window the caller reads around the selection's bounds, px.
pub const MARGIN: u32 = (NEAR + GAP + RING + 4) as u32;
const TERMS: usize = 10;
const MAX_SAMPLES: usize = 20_000;

/// The fill: every channel over the window, and the pixels it paints (the
/// selection, less the page's structure, plus the lettering's pieces past it).
pub struct Filled {
    pub channels: Vec<Grid>,
    pub paint: Mask,
}

/// The surface's frame: the hole's centre and half its size, so the cubic
/// terms stay near 1.
#[derive(Clone, Copy)]
struct Frame {
    cx: f64,
    cy: f64,
    s: f64,
}

impl Frame {
    /// `1, u, v, u^2, uv, v^2, u^3, u^2 v, u v^2, v^3`: the plane is the first three.
    fn basis(&self, x: f64, y: f64) -> [f64; TERMS] {
        let (u, v) = ((x - self.cx) / self.s, (y - self.cy) / self.s);
        [1.0, u, v, u * u, u * v, v * v, u * u * u, u * u * v, u * v * v, v * v * v]
    }
}

/// A fitted surface: coefficients per channel, held to the ring's own range.
struct Surface {
    frame: Frame,
    coef: Vec<[f64; TERMS]>,
    lo: Vec<f64>,
    hi: Vec<f64>,
    /// The robust spread of the ring about the surface, and about the
    /// first plane (what [`MAX_NOISE`] is held against).
    noise: f64,
    spread: f64,
}

impl Surface {
    fn at(&self, x: usize, y: usize) -> Vec<f64> {
        let b = self.frame.basis(x as f64, y as f64);
        self.coef
            .iter()
            .enumerate()
            .map(|(k, c)| c.iter().zip(&b).map(|(c, b)| c * b).sum::<f64>().clamp(self.lo[k], self.hi[k]))
            .collect()
    }
}

/// Fill `hole` in `chs` from all round it. `None` when the ring is too small
/// or has no one smooth surface.
pub fn fill(chs: &[Grid], hole: &Mask) -> Option<Filled> {
    let (w, h) = (hole.w, hole.h);
    let n = w * h;
    let ys = (0..n).filter(|&i| hole.data[i]).map(|i| i / w);
    let (y0, y1) = ys.fold((usize::MAX, 0), |(a, b), y| (a.min(y), b.max(y)));
    if y0 == usize::MAX {
        return None;
    }
    let near = hole.dilate(NEAR);
    let mut grown = hole.clone();
    let mut structure = Mask::new(w, h, false);
    let mut weak = Mask::new(w, h, false);
    let mut surface = None;
    for round in 0..4 {
        let ring = grown.dilate(GAP + RING).and_not(&grown.dilate(GAP)).and_not(&structure);
        let s = fit(chs, &ring, frame(&grown, y0, y1))?;
        if round == 0 && s.spread > MAX_NOISE {
            return None;
        }
        let off = residuals(chs, &s);
        weak = Mask { w, h, data: off.iter().map(|&r| r > (2.0 * s.noise).max(5.0)).collect() };
        let strong = Mask { w, h, data: off.iter().map(|&r| r > (5.0 * s.noise).max(20.0)).collect() };
        let (label, sizes) = components(&strong);
        let (mut inside, mut touches, mut edge) = (vec![0usize; sizes.len()], vec![false; sizes.len()], vec![false; sizes.len()]);
        for i in 0..n {
            if label[i] > 0 {
                let (x, y) = (i % w, i / w);
                inside[label[i]] += near.data[i] as usize;
                touches[label[i]] |= hole.data[i];
                edge[label[i]] |= x == 0 || y == 0 || x == w - 1 || y == h - 1;
            }
        }
        let text: Vec<bool> = (0..sizes.len()).map(|l| l > 0 && touches[l] && !edge[l] && inside[l] * 5 >= sizes[l] * 3).collect();
        let text_seed = Mask { w, h, data: label.iter().map(|&l| text[l]).collect() };
        let page_seed = Mask { w, h, data: label.iter().map(|&l| l > 0 && !text[l]).collect() };
        let content = spread(&text_seed, &weak.and(&near).and_not(&page_seed));
        structure = spread(&page_seed, &weak.and_not(&content)).and_not(&content);
        let next = hole.and_not(&structure).or(&content);
        let changed = next.data.iter().zip(&grown.data).filter(|(a, b)| a != b).count();
        grown = next;
        surface = Some(s);
        if changed < 50 {
            break;
        }
    }
    // the last round's surface, refitted round the hole as it ended
    let ring = grown.dilate(GAP + RING).and_not(&grown.dilate(GAP)).and_not(&structure);
    let surface = fit(chs, &ring, frame(&grown, y0, y1)).or(surface)?;
    // the last of a glow tail, a few levels over the surface, goes with it
    let tail = Mask { w, h, data: residuals(chs, &surface).iter().map(|&r| r > (1.5 * surface.noise).max(3.0)).collect() };
    let rim = grown.dilate(TAIL).and_not(&structure).and(&near);
    let paint = spread(&grown, &tail.and(&rim)).dilate(2).and(&rim).or(&grown);
    let known = paint.not().and_not(&weak);
    if !known.any() {
        return None;
    }
    let base: Vec<Vec<f64>> = (0..n).map(|i| surface.at(i % w, i / w)).collect();
    let fade: Vec<f64> = distance_to(&paint.not()).data.iter().map(|d| (-d / REACH).exp()).collect();
    let channels = chs
        .iter()
        .enumerate()
        .map(|(k, g)| {
            let diff = Grid { w, h, data: (0..n).map(|i| g.data[i] - base[i][k]).collect() };
            let carried = membrane(&diff, &known);
            Grid {
                w,
                h,
                data: (0..n)
                    .map(|i| if paint.data[i] { (base[i][k] + carried.data[i] * fade[i]).clamp(0.0, 255.0) } else { g.data[i] })
                    .collect(),
            }
        })
        .collect();
    Some(Filled { channels, paint })
}

fn frame(hole: &Mask, y0: usize, y1: usize) -> Frame {
    let (mut sx, mut sy, mut count, mut x0, mut x1) = (0.0f64, 0.0f64, 0.0f64, usize::MAX, 0);
    for (i, _) in hole.data.iter().enumerate().filter(|(_, &v)| v) {
        let (x, y) = (i % hole.w, i / hole.w);
        sx += x as f64;
        sy += y as f64;
        count += 1.0;
        (x0, x1) = (x0.min(x), x1.max(x));
    }
    let span = (x1.saturating_sub(x0)).max(y1.saturating_sub(y0)) as f64;
    Frame { cx: sx / count.max(1.0), cy: sy / count.max(1.0), s: span / 2.0 + (GAP + RING) as f64 }
}

/// The colour distance of every pixel from the surface.
fn residuals(chs: &[Grid], s: &Surface) -> Vec<f64> {
    let w = chs[0].w;
    (0..chs[0].data.len())
        .map(|i| {
            let f = s.at(i % w, i / w);
            chs.iter().zip(&f).map(|(g, f)| (g.data[i] - f).powi(2)).sum::<f64>().sqrt()
        })
        .collect()
}

/// `seed` and every pixel of `within` 8-connected to it.
fn spread(seed: &Mask, within: &Mask) -> Mask {
    let (w, h) = (seed.w, seed.h);
    let mut out = seed.clone();
    let mut stack: Vec<usize> = (0..w * h).filter(|&i| seed.data[i]).collect();
    while let Some(i) = stack.pop() {
        let (x, y) = ((i % w) as isize, (i / w) as isize);
        for (dx, dy) in [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
            let (nx, ny) = (x + dx, y + dy);
            if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                continue;
            }
            let j = ny as usize * w + nx as usize;
            if within.data[j] && !out.data[j] {
                out.data[j] = true;
                stack.push(j);
            }
        }
    }
    out
}

/// Repeatable 0..1 from an index, so a fit never changes between runs.
fn unit(i: u64) -> f64 {
    let mut z = i.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
}

/// The robust cubic fit to the `ring` pixels of `chs`.
fn fit(chs: &[Grid], ring: &Mask, frame: Frame) -> Option<Surface> {
    let w = ring.w;
    let picked: Vec<usize> = (0..ring.data.len()).filter(|&i| ring.data[i]).collect();
    if picked.len() < 32 {
        return None;
    }
    let stride = picked.len().div_ceil(MAX_SAMPLES).max(1);
    let samples: Vec<([f64; TERMS], Vec<f64>)> = picked
        .iter()
        .step_by(stride)
        .map(|&i| (frame.basis((i % w) as f64, (i / w) as f64), chs.iter().map(|g| g.data[i]).collect()))
        .collect();
    let distance = |coef: &[[f64; TERMS]], (b, c): &([f64; TERMS], Vec<f64>), terms: usize| {
        coef.iter().zip(c).map(|(k, c)| (c - k[..terms].iter().zip(b).map(|(k, b)| k * b).sum::<f64>()).powi(2)).sum::<f64>().sqrt()
    };
    // least median of squares over planes through three samples
    let m = samples.len();
    let probe: Vec<usize> = (0..m).step_by(m.div_ceil(3000).max(1)).collect();
    let mut best: Option<(f64, Vec<[f64; TERMS]>)> = None;
    for t in 0..300u64 {
        let pick = [0, 1, 2].map(|j| (unit(t * 3 + j) * m as f64) as usize % m);
        let rows: Vec<Vec<f64>> = pick.iter().map(|&p| samples[p].0[..3].to_vec()).collect();
        let rhs: Vec<Vec<f64>> = pick.iter().map(|&p| samples[p].1.clone()).collect();
        let Some(plane) = solve(rows, rhs) else { continue };
        let coef: Vec<[f64; TERMS]> = (0..chs.len())
            .map(|k| {
                let mut c = [0.0; TERMS];
                (0..3).for_each(|j| c[j] = plane[j][k]);
                c
            })
            .collect();
        let mut errors: Vec<f64> = probe.iter().map(|&p| distance(&coef, &samples[p], 3)).collect();
        let med = median(&mut errors);
        if best.as_ref().is_none_or(|(b, _)| med < *b) {
            best = Some((med, coef));
        }
    }
    let (med, plane) = best?;
    let first = 1.4826 * med + 1.0;
    let mut weight: Vec<f64> = samples.iter().map(|s| if distance(&plane, s, 3) < 2.5 * first + 5.0 { 1.0 } else { 0.0 }).collect();
    let mut coef = plane;
    let mut noise = first;
    for _ in 0..10 {
        let mut ata = vec![vec![0.0; TERMS]; TERMS];
        let mut atb = vec![vec![0.0; chs.len()]; TERMS];
        for ((b, c), &wt) in samples.iter().zip(&weight) {
            if wt == 0.0 {
                continue;
            }
            for i in 0..TERMS {
                for j in 0..TERMS {
                    ata[i][j] += wt * b[i] * b[j];
                }
                for (k, c) in c.iter().enumerate() {
                    atb[i][k] += wt * b[i] * c;
                }
            }
        }
        let Some(solved) = solve(ata, atb) else { break };
        coef = (0..chs.len()).map(|k| std::array::from_fn(|j| solved[j][k])).collect();
        let errors: Vec<f64> = samples.iter().map(|s| distance(&coef, s, TERMS)).collect();
        let mut kept: Vec<f64> = errors.iter().zip(&weight).filter(|(_, &wt)| wt > 0.0).map(|(e, _)| *e).collect();
        if kept.is_empty() {
            return None;
        }
        noise = 1.4826 * median(&mut kept) + 1.0;
        let cut = 4.685 * noise;
        weight = errors.iter().map(|&e| if e < cut { (1.0 - (e / cut).powi(2)).powi(2) } else { 0.0 }).collect();
    }
    // the surface never leaves the range the ring's own pixels span
    let (mut lo, mut hi) = (vec![], vec![]);
    for k in 0..chs.len() {
        let mut values: Vec<f64> = samples.iter().zip(&weight).filter(|(_, &wt)| wt > 0.0).map(|(s, _)| s.1[k]).collect();
        if values.is_empty() {
            return None;
        }
        lo.push(percentile(&mut values, 1.0) - 4.0);
        hi.push(percentile(&mut values, 99.0) + 4.0);
    }
    Some(Surface { frame, coef, lo, hi, noise, spread: first })
}

/// Gaussian elimination with partial pivoting, one right-hand side per column.
fn solve(mut a: Vec<Vec<f64>>, mut b: Vec<Vec<f64>>) -> Option<Vec<Vec<f64>>> {
    let n = a.len();
    for c in 0..n {
        let p = (c..n).max_by(|&i, &j| a[i][c].abs().total_cmp(&a[j][c].abs()))?;
        if a[p][c].abs() < 1e-10 {
            return None;
        }
        a.swap(c, p);
        b.swap(c, p);
        for r in 0..n {
            if r != c {
                let f = a[r][c] / a[c][c];
                if f == 0.0 {
                    continue;
                }
                let (pivot_a, pivot_b) = (a[c].clone(), b[c].clone());
                a[r].iter_mut().zip(&pivot_a).for_each(|(v, p)| *v -= f * p);
                b[r].iter_mut().zip(&pivot_b).for_each(|(v, p)| *v -= f * p);
            }
        }
    }
    Some((0..n).map(|i| b[i].iter().map(|v| v / a[i][i]).collect()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A vertical ease from dark red to red with a white block and its glow in
    /// the middle, the bubble of video 2 in small.
    fn bubble(w: usize, h: usize) -> (Vec<Grid>, Vec<Grid>) {
        let clean: Vec<Grid> = [(10.0, 110.0), (2.0, 18.0), (1.0, 20.0)]
            .iter()
            .map(|&(a, b)| Grid { w, h, data: (0..w * h).map(|i| a + (b - a) * ((i / w) as f64 / h as f64).powf(1.4)).collect() })
            .collect();
        let mut page = clean.clone();
        for i in 0..w * h {
            let (x, y) = ((i % w) as f64, (i / w) as f64);
            // lettering: a block 40..120 x 40..60, glowing red 8 px out
            let d = (40.0 - x).max(x - 120.0).max(40.0 - y).max(y - 60.0);
            if d <= 0.0 {
                page.iter_mut().for_each(|g| g.data[i] = 250.0);
            } else if d < 8.0 {
                page[0].data[i] = (page[0].data[i] + 120.0 * (1.0 - d / 8.0)).min(255.0);
            }
        }
        (clean, page)
    }

    fn rect(w: usize, h: usize, x0: usize, y0: usize, x1: usize, y1: usize) -> Mask {
        Mask { w, h, data: (0..w * h).map(|i| (x0..x1).contains(&(i % w)) && (y0..y1).contains(&(i / w))).collect() }
    }

    fn worst(a: &[Grid], b: &[Grid], m: &Mask) -> f64 {
        (0..m.data.len()).filter(|&i| m.data[i]).flat_map(|i| a.iter().zip(b).map(move |(a, b)| (a.data[i] - b.data[i]).abs())).fold(0.0, f64::max)
    }

    #[test]
    fn clears_lettering_and_a_glow_the_selection_cut_through() {
        let (w, h) = (160, 100);
        let (clean, page) = bubble(w, h);
        // the selection stops 3 px out: the glow runs on past it
        let filled = fill(&page, &rect(w, h, 37, 37, 123, 63)).unwrap();
        assert!(filled.paint.at(36, 50) && filled.paint.at(80, 66), "the glow past the selection is taken");
        let around = rect(w, h, 25, 25, 135, 75);
        let err = worst(&filled.channels, &clean, &around);
        assert!(err < 6.0, "the bubble comes back, off by {err}");
    }

    #[test]
    fn leaves_the_outline_and_the_paper_alone() {
        let (w, h) = (160, 100);
        let (_, mut page) = bubble(w, h);
        // white paper right of a black outline at x = 140
        for i in 0..w * h {
            let x = i % w;
            if x >= 140 {
                page.iter_mut().for_each(|g| g.data[i] = if x < 143 { 0.0 } else { 255.0 });
            }
        }
        let selection = rect(w, h, 37, 37, 150, 63);
        let filled = fill(&page, &selection).unwrap();
        for y in 37..63 {
            for x in 140..150 {
                assert!(!filled.paint.at(x, y), "the outline and the paper are kept at ({x}, {y})");
            }
        }
        assert!(filled.channels[0].at(100, 50) < 120.0, "the text is gone");
    }

    #[test]
    fn refuses_a_ring_with_no_one_surface() {
        let (w, h) = (80, 80);
        let noise: Vec<Grid> = (0..3).map(|k| Grid { w, h, data: (0..w * h).map(|i| unit(i as u64 * 3 + k) * 255.0).collect() }).collect();
        assert!(fill(&noise, &rect(w, h, 30, 30, 50, 50)).is_none());
    }
}
