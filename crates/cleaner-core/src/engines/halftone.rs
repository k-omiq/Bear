//! The screen step of [`super::postprocess`]: a regular dot screen redrawn in
//! phase across the pixels an edit model repainted.
//!
//! An edit model keeps the tone of a screentone under lettering and loses its
//! dots, or draws dots of its own out of step with the page's. Per background
//! region that touches the mask (the drawing's contours are its walls), from
//! the page within [`RING`] px of the mask:
//!
//! 1. **Frequencies.** The strongest peaks of the power spectrum of the page's
//!    fine detail, each refined twice: a parabola on the log power, then the
//!    mean turn of the detail demodulated at it, since a frequency a little off
//!    turns that phase steadily across the region.
//! 2. **Phase.** For each frequency the page's own local phase, carried into
//!    the mask from the coarse scale to the fine one: where every pixel lies in
//!    the screen's cycle.
//! 3. **Screen model.** For each pair of frequencies at an angle, a table of
//!    the page's value by position in the dot cell and by tone, built on half
//!    the region's page (alternate [`BLOCK`] px blocks) and scored on the other
//!    half by the correlation of predicted and actual detail. The best pair is
//!    the screen if it scores at least [`MIN_FIT`] and enough of the page shows
//!    both frequencies at once ([`MIN_EVIDENCE`]); hand-drawn texture, hatching
//!    and flat paper do not.
//! 4. **Shifts.** Whole-pixel displacements, up to [`MAX_SHIFT`] px, that keep
//!    the phase of both frequencies within [`PHASE_TOL`] of a cycle, kept where
//!    the page's detail one shift away correlates with itself by [`MIN_COPY`].
//! 5. **Copy.** Each mask pixel of the region takes the page's own pixel one
//!    shift away, from a source within [`MAX_TONE_GAP`] of the answer's tone
//!    there, the closest in tone (a shorter shift, and the one a neighbour
//!    took, win ties), with the difference in tone added. A source is a page
//!    pixel whose patch repeats one shift away for most shifts, so lettering,
//!    lines and stains left in the region are never copied. The dots are the
//!    page's, crisp, not an average of them.
//! 6. **Redraw.** A mask pixel no shift reaches from a source takes the
//!    table's value at its own place in the cell and at the answer's tone
//!    there. Where that tone is outside what the region shows, by up to
//!    [`TONE_MARGIN`], the redraw fades out and the answer stays. Both fade
//!    near a contour.
//!
//! [`Reach`] picks how far step 5 goes: off (bench 5's table alone), the
//! direct copy (bench 7, the default), or the copy and then hops from copied
//! pixels deeper into a wide mask (bench 7's chain, which drew streaks there).
//!
//! Measured against synthetic lettering over real halftone pages
//! (docs/research/flux3-klein-5_6b.md, benches 5 and 7).

use std::f64::consts::TAU;

use super::grid::{Grid, Mask, components, distance_to, gaussian, masked_blur, power_spectrum, resize_area, resize_linear};

/// The held-out detail correlation a screen model needs before it redraws.
pub const MIN_FIT: f64 = 0.5;
/// Tone this many levels outside what a region's page shows fades the redraw
/// out.
pub const TONE_MARGIN: f64 = 12.0;
/// The longest shift a copy takes, in px.
pub const MAX_SHIFT: i64 = 64;
/// The phase error, in cycles, a shift may make on either frequency.
pub const PHASE_TOL: f64 = 0.05;
/// The correlation of the page's detail with itself one shift away that a
/// shift needs before the copy uses it.
pub const MIN_COPY: f64 = 0.8;
/// A copy takes no source whose tone is further than this from the answer's.
pub const MAX_TONE_GAP: f64 = 16.0;
/// Shifts tried, the closest in phase in each of [`SHIFT_BANDS`] bands of
/// length.
const SHIFTS_TRIED: usize = 64;
const SHIFT_BANDS: usize = 4;
/// A source's patches (this radius, in px) and theirs one shift away differ by
/// at most this many levels on average, for at least half the shifts.
const PATCH: i64 = 2;
const MAX_PATCH_GAP: f64 = 30.0;
/// A copy's cost per px of shift, and what it saves by taking the shift a
/// neighbour took, in tone levels.
const LENGTH_COST: f64 = 0.05;
const KEEP_BONUS: f64 = 2.0;
/// Page evidence lies within this many px of the mask.
pub const RING: f64 = 64.0;
/// Page pixels a region needs before its screen is measured.
const MIN_KNOWN: usize = 600;
/// Spectral peaks tried as the screen's frequencies, and the share of the
/// strongest one a peak needs.
const CANDIDATES: usize = 6;
const MIN_PEAK: f64 = 0.02;
/// Periods longer than this, in px, are drawing, not screen.
const LONGEST_PERIOD: f64 = 24.0;
/// The two frequencies of a screen lie at least this far apart, in degrees.
const MIN_ANGLE: f64 = 35.0;
/// Phase bins per axis per px of period (4 to 24), and tone bins.
const BINS_PER_PX: f64 = 4.0;
const TONE_BINS: usize = 12;
/// The fine detail a region's page needs, RMS in levels: paper has none.
const MIN_DETAIL: f64 = 2.0;
/// A page pixel shows the screen where, within this many px, its detail
/// passes [`MIN_DETAIL`] and the share of it at the two frequencies together
/// is [`EVIDENCE_SHARE`] and at each [`EVIDENCE_EACH`]; a region needs
/// [`MIN_EVIDENCE`] such pixels. Line-art hatching fits a screen model
/// passably and fails this, so it never gets dots. Built on the reference
/// tool's `_screen_evidence`, with the same window and shares but not the
/// same floor or count: that test asks a power over 36 (6 levels RMS of a
/// 2 px high-pass) and 256 pixels over the whole crop, 128 per region; this
/// one asks 2 levels RMS of this module's detail and 256 pixels per region.
const EVIDENCE_SIGMA: f64 = 5.0;
const EVIDENCE_SHARE: f64 = 0.35;
const EVIDENCE_EACH: f64 = 0.06;
const MIN_EVIDENCE: usize = 256;
/// The side of the blocks the screen model is built on and scored on.
const BLOCK: usize = 16;
/// Demodulation scales, fine to coarse, in px. The fine one runs at full
/// size; the rest on a grid [`DECIMATE`] times smaller.
const PHASE_SIGMAS: [f64; 5] = [6.0, 12.0, 24.0, 48.0, 96.0];
const DECIMATE: usize = 4;
/// A demodulation scale takes over from the coarser one where this much of
/// its window is page.
const FULL_SUPPORT: f64 = 0.25;

/// How far the copy of the page's own dots reaches before the table draws the
/// rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reach {
    /// No copy: the table draws every pixel (bench 5).
    Table,
    /// One shift from a page source (bench 7).
    Copy,
    /// One shift, then hops from pixels already copied (bench 7's chain).
    Hops,
}

/// The redrawn screen over the crop and the weight to blend it at (zero where
/// nothing was redrawn), or `None` where no region of the mask lies on a
/// screen. `fade` is the contour fade every post-processing step uses.
pub(crate) fn redraw(page: &Grid, answer: &Grid, mask: &Mask, protected: &Mask, inside: &Mask, fade: &Grid, reach: Reach) -> Option<(Grid, Grid)> {
    let (w, h) = (page.w, page.h);
    let n = w * h;
    let grown = mask.dilate(2);
    let from_mask = distance_to(mask);
    let (label, _) = components(&protected.not());
    let mut regions: Vec<usize> = (0..n).filter(|&i| mask.data[i] && label[i] > 0).map(|i| label[i]).collect();
    regions.sort_unstable();
    regions.dedup();
    let mut value = answer.clone();
    let mut weight = Grid::new(w, h, 0.0);
    for r in regions {
        // The region within reach of the mask, cut to its bounds.
        let near: Vec<usize> = (0..n).filter(|&i| label[i] == r && from_mask.data[i] <= RING).collect();
        let (x0, x1) = (near.iter().map(|i| i % w).min()?, near.iter().map(|i| i % w).max()?);
        let (y0, y1) = (near.iter().map(|i| i / w).min()?, near.iter().map(|i| i / w).max()?);
        let win = Window { x0, y0, w: x1 - x0 + 1, h: y1 - y0 + 1, stride: w };
        let region = win.mask(|i| label[i] == r);
        let known = win.mask(|i| label[i] == r && inside.data[i] && !grown.data[i] && from_mask.data[i] <= RING);
        if known.count() < MIN_KNOWN {
            continue;
        }
        let shown = win.grid(page);
        let Some(screen) = Screen::fit(&shown, &known) else {
            continue;
        };
        let tone = masked_blur(&win.grid(answer), &region, screen.tone_sigma).0;
        let hole = win.mask(|i| label[i] == r && mask.data[i]);
        let copied = if reach == Reach::Table { None } else { screen.copy(&shown, &known, &hole, &tone, reach == Reach::Hops) };
        for y in 0..win.h {
            for x in 0..win.w {
                let (j, i) = (y * win.w + x, (y + win.y0) * w + x + win.x0);
                if !region.data[j] || !mask.data[i] {
                    continue;
                }
                if let Some((copy, done)) = &copied {
                    if done.data[j] && fade.data[i] > 0.0 {
                        value.data[i] = copy.data[j];
                        weight.data[i] = fade.data[i];
                        continue;
                    }
                }
                let t = tone.data[j];
                let off = (screen.table.t0 - t).max(t - screen.table.t1).max(0.0);
                let s = (1.0 - off / TONE_MARGIN).clamp(0.0, 1.0) * fade.data[i];
                if s > 0.0 {
                    value.data[i] = screen.table.at(screen.u.data[j], screen.v.data[j], t);
                    weight.data[i] = s;
                }
            }
        }
    }
    weight.data.iter().any(|&s| s > 0.0).then_some((value, weight))
}

/// A rectangle of a crop `stride` px wide.
struct Window {
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
    stride: usize,
}

impl Window {
    fn index(&self, j: usize) -> usize {
        (j / self.w + self.y0) * self.stride + j % self.w + self.x0
    }

    fn grid(&self, g: &Grid) -> Grid {
        Grid { w: self.w, h: self.h, data: (0..self.w * self.h).map(|j| g.data[self.index(j)]).collect() }
    }

    fn mask(&self, keep: impl Fn(usize) -> bool) -> Mask {
        Mask { w: self.w, h: self.h, data: (0..self.w * self.h).map(|j| keep(self.index(j))).collect() }
    }
}

/// A screen measured on one region: its two frequencies, its phase along them
/// at every pixel, its value by place in the cell and tone, and the page's
/// fine detail it was measured on.
struct Screen {
    f: [f64; 2],
    g: [f64; 2],
    u: Grid,
    v: Grid,
    table: Table,
    tone_sigma: f64,
    detail: Grid,
}

impl Screen {
    fn fit(page: &Grid, known: &Mask) -> Option<Screen> {
        let n = page.data.len();
        let base = masked_blur(page, known, 1.5).0;
        let detail = Grid { w: page.w, h: page.h, data: (0..n).map(|i| if known.data[i] { page.data[i] - base.data[i] } else { 0.0 }).collect() };
        let energy = (0..n).filter(|&i| known.data[i]).map(|i| detail.data[i] * detail.data[i]).sum::<f64>();
        if (energy / known.count().max(1) as f64).sqrt() < MIN_DETAIL {
            return None;
        }
        let freqs: Vec<[f64; 2]> = peaks(&detail, known).into_iter().map(|f| refine(&detail, known, f)).collect();
        if freqs.len() < 2 {
            return None;
        }
        let phases: Vec<Grid> = freqs.iter().map(|&f| local_phase(&detail, known, f)).collect();
        let pixels: Vec<usize> = (0..n).filter(|&i| known.data[i]).collect();
        let (train, test): (Vec<usize>, Vec<usize>) = pixels.iter().partition(|&&i| ((i / page.w) / BLOCK + (i % page.w) / BLOCK) % 2 == 0);
        let mut pairs: Vec<(f64, usize, usize, f64)> = Vec::new();
        for a in 0..freqs.len() {
            for b in a + 1..freqs.len() {
                let (f, g) = (freqs[a], freqs[b]);
                let det = (f[0] * g[1] - f[1] * g[0]).abs();
                if det / (f[0].hypot(f[1]) * g[0].hypot(g[1])) < MIN_ANGLE.to_radians().sin() {
                    continue;
                }
                let area = 1.0 / det.max(1e-12);
                if !(2.0..=600.0).contains(&area) {
                    continue;
                }
                let period = area.sqrt();
                let tone = masked_blur(page, known, period.max(2.0)).0;
                let table = Table::build(&train, &phases[a], &phases[b], &tone, page, bins(period));
                let predicted: Vec<f64> = test.iter().map(|&i| table.at(phases[a].data[i], phases[b].data[i], tone.data[i]) - tone.data[i]).collect();
                let actual: Vec<f64> = test.iter().map(|&i| page.data[i] - tone.data[i]).collect();
                let fit = correlation(&predicted, &actual);
                if fit >= MIN_FIT {
                    pairs.push((fit, a, b, period));
                }
            }
        }
        // Best fit first, ties in the order found; the first pair the page
        // shows both of at once is the screen, so a hatching pair that fits a
        // little better does not hide it.
        pairs.sort_by(|p, q| q.0.total_cmp(&p.0));
        let window = evidence_window(&detail, known);
        let (_, a, b, period) = pairs.into_iter().find(|&(_, a, b, _)| evidence(&detail, known, &window, freqs[a], freqs[b]) >= MIN_EVIDENCE)?;
        let tone_sigma = period.max(2.0);
        let tone = masked_blur(page, known, tone_sigma).0;
        let table = Table::build(&pixels, &phases[a], &phases[b], &tone, page, bins(period));
        Some(Screen { f: freqs[a], g: freqs[b], u: phases[a].clone(), v: phases[b].clone(), table, tone_sigma, detail })
    }

    /// Steps 4 and 5 over the region's `hole`: the page's pixels copied in,
    /// at `target`'s tone, and the pixels the copy reached; `None` where no
    /// shift holds the screen. With `hops`, pixels no source reaches copy from
    /// pixels already copied.
    fn copy(&self, page: &Grid, known: &Mask, hole: &Mask, target: &Grid, hops: bool) -> Option<(Grid, Mask)> {
        let shifts = self.shifts(known);
        if shifts.is_empty() {
            return None;
        }
        let (w, h) = (page.w as i64, page.h as i64);
        let inside = |y: i64, x: i64| y >= 0 && y < h && x >= 0 && x < w;
        // The mean difference of the known pixels of two patches, where most
        // of both are known.
        let patch_gap = |a: (i64, i64), b: (i64, i64)| -> Option<f64> {
            let (mut sum, mut count) = (0.0, 0);
            for oy in -PATCH..=PATCH {
                for ox in -PATCH..=PATCH {
                    let (ay, ax, by, bx) = (a.0 + oy, a.1 + ox, b.0 + oy, b.1 + ox);
                    if !inside(ay, ax) || !inside(by, bx) {
                        continue;
                    }
                    let (i, j) = ((ay * w + ax) as usize, (by * w + bx) as usize);
                    if known.data[i] && known.data[j] {
                        sum += (page.data[i] - page.data[j]).abs();
                        count += 1;
                    }
                }
            }
            (count * 2 > ((2 * PATCH + 1) * (2 * PATCH + 1)) as usize).then(|| sum / count as f64)
        };
        // A source repeats one shift away, patch for patch, for most shifts:
        // lettering, a line or a stain left in the region does not.
        let repeats = super::screentone::sources(page, hole, &known.not(), &shifts);
        let src = Mask {
            w: page.w,
            h: page.h,
            data: (0..page.data.len())
                .map(|i| {
                    let at = ((i as i64) / w, (i as i64) % w);
                    let gaps: Vec<f64> = if repeats.data[i] { shifts.iter().filter_map(|&(dy, dx, _)| patch_gap(at, (at.0 + dy, at.1 + dx))).collect() } else { Vec::new() };
                    gaps.len() >= 2 && 2 * gaps.iter().filter(|&&g| g <= MAX_PATCH_GAP).count() >= gaps.len()
                })
                .collect(),
        };
        if !src.any() {
            return None;
        }
        let tone = masked_blur(page, &src, self.tone_sigma).0;
        let mut out = Grid::new(page.w, page.h, 0.0);
        let mut done = Mask::new(page.w, page.h, false);
        let mut used = vec![usize::MAX; page.data.len()];
        for y in 0..h {
            for x in 0..w {
                let p = (y * w + x) as usize;
                if !hole.data[p] {
                    continue;
                }
                let mut best: Option<(f64, usize, usize)> = None;
                for (k, &(dy, dx, _)) in shifts.iter().enumerate() {
                    let (sy, sx) = (y + dy, x + dx);
                    if !inside(sy, sx) {
                        continue;
                    }
                    let s = (sy * w + sx) as usize;
                    let gap = (tone.data[s] - target.data[p]).abs();
                    if !src.data[s] || gap > MAX_TONE_GAP {
                        continue;
                    }
                    let mut cost = gap + LENGTH_COST * ((dy * dy + dx * dx) as f64).sqrt();
                    if (x > 0 && used[p - 1] == k) || (y > 0 && used[p - w as usize] == k) {
                        cost -= KEEP_BONUS;
                    }
                    if best.is_none_or(|(c, ..)| cost < c) {
                        best = Some((cost, k, s));
                    }
                }
                if let Some((_, k, s)) = best {
                    out.data[p] = page.data[s] + target.data[p] - tone.data[s];
                    done.data[p] = true;
                    used[p] = k;
                }
            }
        }
        // Deeper in, where no page source is within a shift: short hops from
        // pixels already copied, nearest first, each in phase with its source.
        if hops {
            let dist = distance_to(&src.or(&done));
            let mut order: Vec<usize> = (0..page.data.len()).filter(|&p| hole.data[p] && !done.data[p]).collect();
            order.sort_by(|&a, &b| dist.data[a].total_cmp(&dist.data[b]));
            for p in order {
                let (y, x) = ((p as i64) / w, (p as i64) % w);
                let mut best: Option<(f64, usize, usize)> = None;
                for (k, &(dy, dx, _)) in shifts.iter().enumerate() {
                    let (sy, sx) = (y + dy, x + dx);
                    if !inside(sy, sx) {
                        continue;
                    }
                    let s = (sy * w + sx) as usize;
                    if !done.data[s] {
                        continue;
                    }
                    let mut cost = (target.data[s] - target.data[p]).abs() + LENGTH_COST * ((dy * dy + dx * dx) as f64).sqrt();
                    let beside = [(y, x - 1), (y, x + 1), (y - 1, x), (y + 1, x)];
                    if beside.iter().any(|&(ny, nx)| inside(ny, nx) && used[(ny * w + nx) as usize] == k) {
                        cost -= KEEP_BONUS;
                    }
                    if best.is_none_or(|(c, ..)| cost < c) {
                        best = Some((cost, k, s));
                    }
                }
                if let Some((_, k, s)) = best {
                    out.data[p] = out.data[s] + target.data[p] - target.data[s];
                    done.data[p] = true;
                    used[p] = k;
                }
            }
        }
        done.any().then_some((out, done))
    }

    /// Step 4: the shifts that keep the screen's phase, as `(dy, dx,
    /// correlation)`, best first.
    fn shifts(&self, known: &Mask) -> Vec<(i64, i64, f64)> {
        let wrap = |c: f64| (c - c.round()).abs();
        let mut close: Vec<(f64, i64, i64)> = Vec::new();
        for dy in -MAX_SHIFT..=MAX_SHIFT {
            for dx in -MAX_SHIFT..=MAX_SHIFT {
                // past MAX_SHIFT in length a shift falls in no band below
                if dy * dy + dx * dx < 4 || dy * dy + dx * dx > MAX_SHIFT * MAX_SHIFT {
                    continue;
                }
                let (y, x) = (dy as f64, dx as f64);
                let error = wrap(self.f[0] * y + self.f[1] * x).max(wrap(self.g[0] * y + self.g[1] * x));
                if error <= PHASE_TOL {
                    close.push((error, dy, dx));
                }
            }
        }
        // The closest in phase within each band of length, so a copy reaches
        // into a wide mask as well as next to its edge.
        close.sort_by(|a, b| a.0.total_cmp(&b.0));
        let band = |&(_, dy, dx): &(f64, i64, i64)| ((((dy * dy + dx * dx) as f64).sqrt() * SHIFT_BANDS as f64 / MAX_SHIFT as f64).ceil() as usize).max(1);
        let tried: Vec<(f64, i64, i64)> = (1..=SHIFT_BANDS).flat_map(|b| close.iter().filter(move |c| band(c) == b).take(SHIFTS_TRIED / SHIFT_BANDS)).copied().collect();
        let (w, h) = (known.w as i64, known.h as i64);
        let mut kept: Vec<(i64, i64, f64)> = tried
            .into_iter()
            .filter_map(|(_, dy, dx)| {
                let (mut a, mut b) = (Vec::new(), Vec::new());
                for y in 0.max(-dy)..h.min(h - dy) {
                    for x in 0.max(-dx)..w.min(w - dx) {
                        let (i, s) = ((y * w + x) as usize, ((y + dy) * w + x + dx) as usize);
                        if known.data[i] && known.data[s] {
                            a.push(self.detail.data[i]);
                            b.push(self.detail.data[s]);
                        }
                    }
                }
                let c = if a.len() >= MIN_KNOWN / 2 { correlation(&a, &b) } else { 0.0 };
                (c >= MIN_COPY).then_some((dy, dx, c))
            })
            .collect();
        kept.sort_by(|a, b| b.2.total_cmp(&a.2));
        kept
    }
}

/// How many `known` pixels show a screen at `f` and `g`: their neighbourhood
/// has detail, most of it at the two frequencies together and some at each.
/// Hatching shows one direction at a time and has none. `window` is
/// [`evidence_window`] of the same detail, shared by every pair tried.
fn evidence(detail: &Grid, known: &Mask, window: &(Grid, Grid), f: [f64; 2], g: [f64; 2]) -> usize {
    let (den, power) = window;
    let energy = |f: [f64; 2]| {
        let (re, im) = quadrature(detail, known, f, EVIDENCE_SIGMA);
        Grid { w: detail.w, h: detail.h, data: (0..re.data.len()).map(|i| 2.0 * (re.data[i].powi(2) + im.data[i].powi(2)) / den.data[i].max(1e-6).powi(2)).collect() }
    };
    let (ef, eg) = (energy(f), energy(g));
    (0..detail.data.len())
        .filter(|&i| {
            let p = power.data[i] / den.data[i].max(1e-6);
            let (a, b) = (ef.data[i], eg.data[i]);
            known.data[i] && p > MIN_DETAIL * MIN_DETAIL && a + b > EVIDENCE_SHARE * p && a.min(b) > EVIDENCE_EACH * p
        })
        .count()
}

/// The evidence window's weight of known pixels and of detail power.
fn evidence_window(detail: &Grid, known: &Mask) -> (Grid, Grid) {
    let power = Grid { w: detail.w, h: detail.h, data: detail.data.iter().map(|v| v * v).collect() };
    (gaussian(&known.as_grid(), EVIDENCE_SIGMA), gaussian(&power, EVIDENCE_SIGMA))
}

fn bins(period: f64) -> usize {
    ((period * BINS_PER_PX).round() as usize).clamp(4, 24)
}

/// Index `k` of a `size`-point transform, in cycles per px.
fn frequency(k: usize, size: usize) -> f64 {
    let f = k as f64 / size as f64;
    if k < size / 2 { f } else { f - 1.0 }
}

/// Up to [`CANDIDATES`] spectral peaks of `detail` over `known`, strongest
/// first, as `(fy, fx)` cycles per px, in the upper half-plane and above the
/// drawing's band.
fn peaks(detail: &Grid, known: &Mask) -> Vec<[f64; 2]> {
    let (w, h) = (detail.w, detail.h);
    let window = gaussian(&known.as_grid(), 4.0);
    let tapered = Grid { w, h, data: (0..w * h).map(|i| if known.data[i] { detail.data[i] * window.data[i] } else { 0.0 }).collect() };
    let size = w.max(h).next_power_of_two().max(512);
    let power = power_spectrum(&tapered, size);
    let in_band = |ky: usize, kx: usize| frequency(ky, size).hypot(frequency(kx, size)) > 1.0 / LONGEST_PERIOD;
    let at = |ky: isize, kx: isize| {
        let (y, x) = (ky.rem_euclid(size as isize) as usize, kx.rem_euclid(size as isize) as usize);
        if in_band(y, x) { power[y * size + x] } else { 0.0 }
    };
    let mut found = Vec::new();
    for ky in 0..size {
        let fy = frequency(ky, size);
        if fy < 0.0 {
            continue;
        }
        for kx in 0..size {
            let fx = frequency(kx, size);
            if (fy == 0.0 && fx <= 0.0) || !in_band(ky, kx) {
                continue;
            }
            let p = power[ky * size + kx];
            let (y, x) = (ky as isize, kx as isize);
            if p > 0.0 && (-2..=2).all(|dy| (-2..=2).all(|dx| at(y + dy, x + dx) <= p)) {
                found.push((p, ky, kx));
            }
        }
    }
    found.sort_by(|a, b| b.0.total_cmp(&a.0));
    let Some(&(top, ..)) = found.first() else {
        return Vec::new();
    };
    let parabola = |m: f64, c: f64, p: f64| {
        let (m, c, p) = ((m + 1e-12).ln(), (c + 1e-12).ln(), (p + 1e-12).ln());
        let d = m - 2.0 * c + p;
        if d.abs() < 1e-12 { 0.0 } else { 0.5 * (m - p) / d }
    };
    let raw = |ky: isize, kx: isize| power[ky.rem_euclid(size as isize) as usize * size + kx.rem_euclid(size as isize) as usize];
    found
        .into_iter()
        .take_while(|&(p, ..)| p >= MIN_PEAK * top)
        .take(CANDIDATES)
        .map(|(_, ky, kx)| {
            let (y, x) = (ky as isize, kx as isize);
            let dy = parabola(raw(y - 1, x), raw(y, x), raw(y + 1, x));
            let dx = parabola(raw(y, x - 1), raw(y, x), raw(y, x + 1));
            [frequency(ky, size) + dy / size as f64, frequency(kx, size) + dx / size as f64]
        })
        .collect()
}

/// `detail` over `known` demodulated at `f` and blurred: the unnormalised
/// real and imaginary parts and the blurred weight of `known`.
fn demodulate(detail: &Grid, known: &Mask, f: [f64; 2], sigma: f64) -> (Grid, Grid, Grid) {
    let (re, im) = quadrature(detail, known, f, sigma);
    (re, im, gaussian(&known.as_grid(), sigma))
}

/// [`demodulate`] without the weight.
fn quadrature(detail: &Grid, known: &Mask, f: [f64; 2], sigma: f64) -> (Grid, Grid) {
    let (w, h) = (detail.w, detail.h);
    let mut re = Grid::new(w, h, 0.0);
    let mut im = Grid::new(w, h, 0.0);
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if known.data[i] {
                let arg = TAU * (f[0] * y as f64 + f[1] * x as f64);
                re.data[i] = detail.data[i] * arg.cos();
                im.data[i] = -detail.data[i] * arg.sin();
            }
        }
    }
    (gaussian(&re, sigma), gaussian(&im, sigma))
}

/// `f` corrected three times by the mean turn, per px down and across, of the
/// detail demodulated at it.
fn refine(detail: &Grid, known: &Mask, mut f: [f64; 2]) -> [f64; 2] {
    let (w, h) = (detail.w, detail.h);
    for _ in 0..3 {
        let (re, im, den) = demodulate(detail, known, f, PHASE_SIGMAS[0]);
        let ok = |i: usize| den.data[i] > 0.5;
        let z = |i: usize| (re.data[i] / den.data[i], im.data[i] / den.data[i]);
        // Sum of z(next) * conj(z(here)) over neighbouring pairs.
        let turn = |a: usize, b: usize, acc: &mut (f64, f64)| {
            let ((ra, ia), (rb, ib)) = (z(a), z(b));
            acc.0 += ra * rb + ia * ib;
            acc.1 += ia * rb - ra * ib;
        };
        let (mut down, mut across) = ((0.0, 0.0), (0.0, 0.0));
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if !ok(i) {
                    continue;
                }
                if y + 1 < h && ok(i + w) {
                    turn(i + w, i, &mut down);
                }
                if x + 1 < w && ok(i + 1) {
                    turn(i + 1, i, &mut across);
                }
            }
        }
        f[0] += down.1.atan2(down.0) / TAU;
        f[1] += across.1.atan2(across.0) / TAU;
    }
    f
}

/// Every pixel's place in the cycle of `f`, 0 to 1: the page's local phase
/// at `f`, carried from the coarse scale to the fine one so it reaches into
/// the mask, added to the phase `f` itself gives there.
fn local_phase(detail: &Grid, known: &Mask, f: [f64; 2]) -> Grid {
    let (w, h) = (detail.w, detail.h);
    let n = w * h;
    let (re, im, den) = demodulate(detail, known, f, PHASE_SIGMAS[0]);
    let (sw, sh) = (w.div_ceil(DECIMATE), h.div_ceil(DECIMATE));
    let small = [&re, &im, &den].map(|g| resize_area(g, sw, sh));
    let mut z = vec![(0.0, 0.0); n];
    for &sigma in PHASE_SIGMAS.iter().rev() {
        let scale = if sigma == PHASE_SIGMAS[0] {
            [re.clone(), im.clone(), den.clone()]
        } else {
            // Blurred at the fine scale already: the rest of the way, on the small grid.
            let rest = (sigma * sigma - PHASE_SIGMAS[0] * PHASE_SIGMAS[0]).sqrt() / DECIMATE as f64;
            small.clone().map(|g| resize_linear(&gaussian(&g, rest), w, h))
        };
        let [sr, si, sd] = scale;
        for i in 0..n {
            let s = (sd.data[i] / FULL_SUPPORT).clamp(0.0, 1.0);
            let d = sd.data[i].max(1e-9);
            z[i] = (z[i].0 * (1.0 - s) + sr.data[i] / d * s, z[i].1 * (1.0 - s) + si.data[i] / d * s);
        }
    }
    Grid {
        w,
        h,
        data: (0..n)
            .map(|i| {
                let (y, x) = ((i / w) as f64, (i % w) as f64);
                let u = f[0] * y + f[1] * x + z[i].1.atan2(z[i].0) / TAU;
                u - u.floor()
            })
            .collect(),
    }
}

/// A screen's value by place in the cell (`u`, `v`, cyclic) and by tone,
/// trilinear.
struct Table {
    bins: usize,
    t0: f64,
    t1: f64,
    cells: Vec<f64>,
}

impl Table {
    fn build(pixels: &[usize], u: &Grid, v: &Grid, tone: &Grid, page: &Grid, bins: usize) -> Table {
        let (t0, t1) = pixels.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &i| (lo.min(tone.data[i]), hi.max(tone.data[i])));
        let mut table = Table { bins, t0, t1, cells: Vec::new() };
        let (mut weight, mut sum) = (vec![0.0; bins * bins * TONE_BINS], vec![0.0; bins * bins * TONE_BINS]);
        for &i in pixels {
            table.corners(u.data[i], v.data[i], tone.data[i], |cell, wgt| {
                weight[cell] += wgt;
                sum[cell] += wgt * page.data[i];
            });
        }
        let mut cells: Vec<Option<f64>> = sum.iter().zip(&weight).map(|(s, &wt)| (wt > 0.5).then(|| s / wt)).collect();
        // Tone bins a cell place never saw take the nearest seen ones,
        // linearly between them.
        for cell in cells.chunks_mut(TONE_BINS) {
            let seen: Vec<usize> = (0..TONE_BINS).filter(|&t| cell[t].is_some()).collect();
            let (Some(&first), Some(&last)) = (seen.first(), seen.last()) else {
                continue;
            };
            for t in 0..TONE_BINS {
                if cell[t].is_some() {
                    continue;
                }
                cell[t] = Some(if t < first {
                    cell[first].unwrap_or_default()
                } else if t > last {
                    cell[last].unwrap_or_default()
                } else {
                    let lo = *seen.iter().rev().find(|&&s| s < t).unwrap_or(&first);
                    let hi = *seen.iter().find(|&&s| s > t).unwrap_or(&last);
                    let (a, b) = (cell[lo].unwrap_or_default(), cell[hi].unwrap_or_default());
                    a + (b - a) * (t - lo) as f64 / (hi - lo).max(1) as f64
                });
            }
        }
        // A cell place never seen at all (a screen whose period is a whole
        // number of px only ever shows a few) takes the mean of its seen
        // neighbours, ring by ring, so a lookup between seen places blends them.
        let nb = bins as i64;
        while cells.iter().any(Option::is_none) && cells.iter().any(Option::is_some) {
            let mut next = cells.clone();
            for a in 0..nb {
                for b in 0..nb {
                    let place = ((a * nb + b) as usize) * TONE_BINS;
                    if cells[place].is_some() {
                        continue;
                    }
                    let around: Vec<usize> = [(-1, 0), (1, 0), (0, -1), (0, 1)]
                        .iter()
                        .map(|(da, db)| (((a + da).rem_euclid(nb) * nb + (b + db).rem_euclid(nb)) as usize) * TONE_BINS)
                        .filter(|&q| cells[q].is_some())
                        .collect();
                    if around.is_empty() {
                        continue;
                    }
                    for t in 0..TONE_BINS {
                        next[place + t] = Some(around.iter().map(|&q| cells[q + t].unwrap_or_default()).sum::<f64>() / around.len() as f64);
                    }
                }
            }
            cells = next;
        }
        table.cells = cells.into_iter().map(Option::unwrap_or_default).collect();
        table
    }

    /// The eight cells around `(u, v, t)` with their trilinear weights.
    fn corners(&self, u: f64, v: f64, t: f64, mut each: impl FnMut(usize, f64)) {
        let nb = self.bins;
        let (pu, pv) = (u * nb as f64 - 0.5, v * nb as f64 - 0.5);
        let (iu, iv) = (pu.floor() as i64, pv.floor() as i64);
        let (fu, fv) = (pu - pu.floor(), pv - pv.floor());
        let pt = ((t - self.t0) / (self.t1 - self.t0).max(1e-6)).clamp(0.0, 1.0) * (TONE_BINS - 1) as f64;
        let it = (pt.floor() as usize).min(TONE_BINS - 2);
        let ft = pt - it as f64;
        for (du, wu) in [(0, 1.0 - fu), (1, fu)] {
            for (dv, wv) in [(0, 1.0 - fv), (1, fv)] {
                for (dt, wt) in [(0, 1.0 - ft), (1, ft)] {
                    let a = (iu + du).rem_euclid(nb as i64) as usize;
                    let b = (iv + dv).rem_euclid(nb as i64) as usize;
                    each((a * nb + b) * TONE_BINS + (it + dt).min(TONE_BINS - 1), wu * wv * wt);
                }
            }
        }
    }

    fn at(&self, u: f64, v: f64, t: f64) -> f64 {
        let mut out = 0.0;
        self.corners(u, v, t, |cell, wgt| out += wgt * self.cells[cell]);
        out
    }
}

fn correlation(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().max(1) as f64;
    let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
    let (mut ab, mut aa, mut bb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        ab += (x - ma) * (y - mb);
        aa += (x - ma) * (x - ma);
        bb += (y - mb) * (y - mb);
    }
    let d = (aa * bb).sqrt();
    if d > 0.0 { ab / d } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 45 degree dot screen, 2.5 px between dots along the diagonal, whose
    /// ink share ramps from 0.7 at the top to 0.3 at the bottom.
    fn ramp_screen(w: usize, h: usize) -> Grid {
        let f = 1.0 / 2.5 / std::f64::consts::SQRT_2;
        Grid {
            w,
            h,
            data: (0..w * h)
                .map(|i| {
                    let (x, y) = ((i % w) as f64, (i / w) as f64);
                    let cell = ((TAU * f * (x + y)).cos() + (TAU * f * (x - y)).cos()) / 2.0;
                    let ink = 0.7 - 0.4 * y / h as f64;
                    if cell > 1.0 - 2.0 * ink { 30.0 } else { 225.0 }
                })
                .collect(),
        }
    }

    fn square(w: usize, h: usize, x0: usize, y0: usize, x1: usize, y1: usize) -> Mask {
        Mask { w, h, data: (0..w * h).map(|i| (x0..x1).contains(&(i % w)) && (y0..y1).contains(&(i / w))).collect() }
    }

    #[test]
    fn a_grey_patch_on_a_fine_screen_is_redrawn_in_phase() {
        let (w, h) = (192, 192);
        let page = ramp_screen(w, h);
        let mask = square(w, h, 60, 60, 132, 132);
        // The model kept the tone and lost the dots.
        let tone = gaussian(&page, 3.0);
        let answer = page.select(&mask.not(), &tone);
        let none = Mask::new(w, h, false);
        let all = Mask::new(w, h, true);
        let core = square(w, h, 66, 66, 126, 126);
        let error = |g: &Grid| (0..w * h).filter(|&i| core.data[i]).map(|i| (g.data[i] - page.data[i]).abs()).sum::<f64>() / core.count() as f64;
        let crisp = |g: &Grid| {
            let soft = gaussian(g, 1.5);
            let d: Vec<f64> = (0..w * h).filter(|&i| core.data[i]).map(|i| g.data[i] - soft.data[i]).collect();
            (d.iter().map(|v| v * v).sum::<f64>() / d.len() as f64).sqrt()
        };
        for reach in [Reach::Table, Reach::Copy, Reach::Hops] {
            let (value, weight) = redraw(&page, &answer, &mask, &none, &all, &Grid::new(w, h, 1.0), reach).expect("a screen");
            let out = Grid { w, h, data: (0..w * h).map(|i| answer.data[i] * (1.0 - weight.data[i]) + value.data[i] * weight.data[i]).collect() };
            assert!(error(&out) < 0.3 * error(&answer), "{reach:?}: {} against {}", error(&out), error(&answer));
            // The page's own dots, copied: as crisp as the page's, not an
            // average. The table alone draws them softer.
            if reach != Reach::Table {
                assert!(crisp(&out) > 0.9 * crisp(&page), "{reach:?}: {} against {}", crisp(&out), crisp(&page));
            }
        }
    }

    /// White paper with patches of line-art hatching drawn into it, each
    /// patch parallel lines of its own angle and spacing.
    fn hatched_paper(w: usize, h: usize, patches: &[(usize, usize, usize, usize, f64, f64)]) -> Grid {
        let mut page = Grid::new(w, h, 250.0);
        for &(x0, y0, x1, y1, angle, period) in patches {
            let (s, c) = angle.to_radians().sin_cos();
            for y in y0..y1 {
                for x in x0..x1 {
                    if (x as f64 * c + y as f64 * s).rem_euclid(period) < 1.6 {
                        page.data[y * w + x] = 30.0;
                    }
                }
            }
        }
        page
    }

    #[test]
    fn hatching_beside_the_mask_is_not_a_screen() {
        let (w, h) = (192, 192);
        let page = hatched_paper(w, h, &[(8, 8, 72, 68, 45.0, 7.0), (120, 8, 184, 68, -45.0, 7.0), (8, 124, 72, 184, -45.0, 7.0), (120, 124, 184, 184, 45.0, 7.0)]);
        let mask = square(w, h, 72, 72, 120, 120);
        // Two directions of hatching in separate patches fit a screen model
        // (0.69 against MIN_FIT 0.5) and never show both at one place.
        // The model painted the lettering out to paper.
        let answer = page.select(&mask.not(), &Grid::new(w, h, 250.0));
        let none = Mask::new(w, h, false);
        let all = Mask::new(w, h, true);
        for reach in [Reach::Table, Reach::Copy, Reach::Hops] {
            assert!(redraw(&page, &answer, &mask, &none, &all, &Grid::new(w, h, 1.0), reach).is_none(), "{reach:?}");
        }
    }

    #[test]
    fn a_screen_beside_hatching_is_still_copied() {
        let (w, h) = (192, 192);
        // A dot screen on the left, hatched paper on the right, one region.
        let screen = ramp_screen(w, h);
        let hatched = hatched_paper(w, h, &[(120, 16, 184, 90, 45.0, 7.0), (120, 100, 184, 176, -20.0, 6.0)]);
        let left = square(w, h, 0, 0, 112, h);
        let page = hatched.select(&left.not(), &screen);
        let mask = square(w, h, 30, 60, 100, 132);
        let answer = page.select(&mask.not(), &gaussian(&page, 3.0));
        let none = Mask::new(w, h, false);
        let all = Mask::new(w, h, true);
        let (value, weight) = redraw(&page, &answer, &mask, &none, &all, &Grid::new(w, h, 1.0), Reach::Copy).expect("a screen");
        let out = Grid { w, h, data: (0..w * h).map(|i| answer.data[i] * (1.0 - weight.data[i]) + value.data[i] * weight.data[i]).collect() };
        let core = square(w, h, 36, 66, 94, 126);
        let error = |g: &Grid| (0..w * h).filter(|&i| core.data[i]).map(|i| (g.data[i] - page.data[i]).abs()).sum::<f64>() / core.count() as f64;
        assert!(error(&out) < 0.3 * error(&answer), "{} against {}", error(&out), error(&answer));
    }

    #[test]
    fn flat_paper_and_hand_drawn_noise_are_not_a_screen() {
        let (w, h) = (128, 128);
        let mask = square(w, h, 40, 40, 88, 88);
        let none = Mask::new(w, h, false);
        let all = Mask::new(w, h, true);
        let fade = Grid::new(w, h, 1.0);
        let paper = Grid::new(w, h, 250.0);
        assert!(redraw(&paper, &paper, &mask, &none, &all, &fade, Reach::Copy).is_none());
        // Deterministic noise: no lattice in it.
        let mut seed = 12345u64;
        let noise = Grid {
            w,
            h,
            data: (0..w * h)
                .map(|_| {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    128.0 + ((seed >> 33) % 64) as f64
                })
                .collect(),
        };
        assert!(redraw(&noise, &noise, &mask, &none, &all, &fade, Reach::Copy).is_none());
    }

    /// **A region needs 256 pixels of screen evidence.** On a clean screen
    /// every known pixel shows both frequencies, so a 16 x 16 region passes and
    /// a 15 x 17 one, a pixel short, does not.
    #[test]
    fn a_region_needs_256_pixels_of_screen_evidence() {
        let (w, h) = (96, 96);
        let page = ramp_screen(w, h);
        assert!(Screen::fit(&page, &square(w, h, 30, 30, 46, 46)).is_some(), "256 pixels");
        assert!(Screen::fit(&page, &square(w, h, 30, 30, 45, 47)).is_none(), "255 pixels");
    }

    /// **A hatching pair that fits better does not hide a screen.** A faint
    /// screen beside strong hatching: the hatching's pair fits the page a
    /// little better (0.73 against 0.72, measured) and shows no evidence, so
    /// the screen's pair, next in fit, is the screen.
    #[test]
    fn a_better_fitting_hatching_pair_does_not_hide_a_faint_screen() {
        let (w, h) = (192, 192);
        let faint = Grid { w, h, data: ramp_screen(w, h).data.iter().map(|v| 128.0 + (v - 128.0) * 0.2).collect() };
        let hatched = hatched_paper(w, h, &[(96, 0, w, h / 2, 45.0, 7.0), (96, h / 2, w, h, -45.0, 7.0)]);
        let page = hatched.select(&square(w, h, 96, 0, w, h), &faint);
        let screen = Screen::fit(&page, &Mask::new(w, h, true)).expect("the faint screen");
        let period = (screen.f[0] * screen.g[1] - screen.f[1] * screen.g[0]).abs().recip().sqrt();
        assert!((period - 2.36).abs() < 0.1, "period {period}, the hatching's is 7");
    }
}
