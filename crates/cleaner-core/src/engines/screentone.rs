//! The screentone fill: clean lettering printed over a halftone dot screen,
//! keeping the dots in phase with the screen around the hole and the
//! brightness following the screen's gradient.
//!
//! Ported from the research prototype in `spikes/screentone-fill/`
//! (`c09_d2`, the winner of plan steps S1 to S4 in
//! `docs/patch-gradient-plan.md`; that prototype stays the research harness).
//! The steps:
//!
//! 1. **Remnant guard.** A tight mask leaves the outer part of a white letter
//!    outline, a glow, or bits of ink outside it. Every piece brighter or
//!    darker than the screen around the mask ever is, touching the hole and
//!    lying mostly within [`REACH`] px of it, joins the hole, widened by
//!    [`RIM`] px. A piece that runs further (a light, a panel line, a bar) is
//!    art and is left alone. Other detected regions (`avoid`) never join.
//! 2. **Screen model.** The screen's repeat offsets from the masked
//!    autocorrelation of the fine detail (2 to 16 px), and far ones (to 100 px)
//!    where it still repeats strongly. No short repeat at all: the ring is not
//!    a screen, and the whole hole goes to PatchMatch.
//! 3. **Sources.** Known pixels whose detail correlates with itself one repeat
//!    away. Lettering, glows, lights, black fill and lines score low and are
//!    never copied, nor is any other detected region.
//! 4. **Tone field.** The sources' blurred tone, carried across the hole by a
//!    membrane fill ([`super::poisson::membrane`]). This is what keeps the
//!    brightness following a ramp: darker above, lighter below.
//! 5. **Onion copy.** The hole fills from its edge inward. Each pixel copies
//!    through a repeat offset - so the dots stay in phase however wide the hole
//!    is - from a source pixel, or from a pixel already filled when nothing else
//!    reaches. It takes the source closest to the target tone, prefers page
//!    pixels and the offset a neighbour used, and never one more than
//!    [`MAX_TONE_STEP`] levels off. Pixels nothing reaches go to PatchMatch.
//! 6. **Coverage correction.** The gap between the tone field and the copy's
//!    blurred tone, smoothed and capped, is added inside the hole. It is
//!    skipped when the gap is large across the hole, because then the tone
//!    field is what is wrong.

use super::grid::{Grid, Mask as GridMask, autocorr, components, distance_to, gather, gaussian, masked_blur, median, percentile};
use super::patchmatch::{self, Image};
use super::poisson::membrane;
use crate::fit::Fitted;
use crate::image::{BitDepth, ColorMode, Raster};
use crate::mask::{Mask, Rect};

/// How far beyond the mask the remnant guard reaches, in px.
pub const REACH: usize = 12;
/// How much the guard widens what it adds, in px.
pub const RIM: usize = 2;
/// A copy never takes a source more than this many tone levels off target.
pub const MAX_TONE_STEP: f64 = 8.0;
/// Context the fill reads around the mask's bounds, in px: the far repeat
/// offsets reach 100 px, and the guard's ring 20.
pub const MARGIN: usize = 110;

/// The result of [`fill`] over a crop.
#[derive(Clone, Debug)]
pub struct Filled {
    /// The pixels replaced: the hole, grown by the remnant guard.
    pub used: GridMask,
    /// The grayscale result over the whole crop (equal to the input outside `used`).
    pub gray: Grid,
    /// For every pixel of `used` filled by the copy, the page pixel (index into
    /// the crop) its value came from, following chains through filled pixels.
    /// `None` where PatchMatch filled it.
    pub from: Vec<Option<usize>>,
    /// The tone correction added after the copy, per pixel (zero outside `used`).
    pub correction: Grid,
    /// True when the ring held a screen; false means the whole hole went to PatchMatch.
    pub screen: bool,
}

fn screen_ring(g: &Grid, hole: &GridMask, avoid: &GridMask) -> GridMask {
    let mut ring = hole.dilate(20).and_not(&hole.dilate(3)).and_not(avoid);
    let mean = gaussian(g, 1.5);
    let sq = gaussian(&Grid { w: g.w, h: g.h, data: g.data.iter().map(|v| v * v).collect() }, 1.5);
    let std: Vec<f64> = mean.data.iter().zip(&sq.data).map(|(m, s)| (s - m * m).max(0.0).sqrt()).collect();
    let mut vals: Vec<f64> = (0..std.len()).filter(|&i| ring.data[i]).map(|i| std[i]).collect();
    let level = if vals.is_empty() { 0.0 } else { median(&mut vals) };
    if level >= 1.5 {
        for (r, s) in ring.data.iter_mut().zip(&std) {
            *r &= *s >= 0.4 * level;
        }
    }
    ring
}

/// Step 1: the hole grown over leftover lettering.
pub fn grow(gray: &Grid, hole: &GridMask, avoid: &GridMask) -> GridMask {
    let ring = screen_ring(gray, hole, avoid);
    if ring.count() < 50 {
        return hole.clone();
    }
    let fine = gaussian(gray, 1.0);
    let mut ring_vals = gather(&fine, &ring);
    let lo = percentile(&mut ring_vals, 1.0);
    let hi = percentile(&mut ring_vals, 99.0);
    let margin = (0.25 * (hi - lo)).max(10.0);
    let odd = GridMask {
        w: gray.w,
        h: gray.h,
        data: (0..fine.data.len())
            .map(|i| (fine.data[i] > hi + margin || fine.data[i] < lo - margin) && !hole.data[i] && !avoid.data[i])
            .collect(),
    };
    let band = hole.dilate(REACH);
    let (label, sizes) = components(&odd);
    let mut in_band = vec![0usize; sizes.len()];
    for i in 0..label.len() {
        if band.data[i] {
            in_band[label[i]] += 1;
        }
    }
    let mut grown = hole.clone();
    let mut taken = vec![false; sizes.len()];
    taken[0] = true;
    for _ in 0..4 {
        let touch = grown.dilate(1);
        let mut new = vec![false; sizes.len()];
        let mut any = false;
        for (i, &l) in label.iter().enumerate() {
            if touch.data[i] && odd.data[i] && !taken[l] && in_band[l] as f64 >= 0.6 * sizes[l] as f64 {
                new[l] = true;
                any = true;
            }
        }
        if !any {
            break;
        }
        for i in 0..label.len() {
            if new[label[i]] {
                taken[label[i]] = true;
                if band.data[i] {
                    grown.data[i] = true;
                }
            }
        }
    }
    let added = grown.and_not(hole);
    if !added.any() {
        return grown;
    }
    let added = added.close(1).dilate(RIM);
    hole.or(&added.and(&band).and_not(avoid))
}

fn detail(raw: &Grid, known: &GridMask) -> Grid {
    let (tone, _) = masked_blur(raw, known, 1.5);
    Grid { w: raw.w, h: raw.h, data: (0..raw.data.len()).map(|i| if known.data[i] { raw.data[i] - tone.data[i] } else { 0.0 }).collect() }
}

/// Local maxima of an autocorrelation grid (radius `big`) within `radius`,
/// at least `max(floor, rel * best)`, as `(dy, dx, score)` by falling score.
/// `best` defaults to the strongest peak.
fn peaks(c: &Grid, big: usize, radius: usize, rel: f64, floor: f64, best: Option<f64>) -> Vec<(i64, i64, f64)> {
    let n = 2 * radius + 1;
    let off = big - radius;
    let mut sub = vec![0.0; n * n];
    let r = radius as i64;
    for y in 0..n {
        for x in 0..n {
            let (dy, dx) = (y as i64 - r, x as i64 - r);
            sub[y * n + x] = if dy * dy + dx * dx < 4 { -1.0 } else { c.data[(y + off) * c.w + x + off] };
        }
    }
    let mut found = Vec::new();
    for y in 0..n {
        for x in 0..n {
            let v = sub[y * n + x];
            let (dy, dx) = (y as i64 - r, x as i64 - r);
            if dy * dy + dx * dx > r * r {
                continue;
            }
            let mut is_max = true;
            for ny in y.saturating_sub(1)..=(y + 1).min(n - 1) {
                for nx in x.saturating_sub(1)..=(x + 1).min(n - 1) {
                    if sub[ny * n + nx] as f32 > v as f32 + 1e-9 {
                        is_max = false;
                    }
                }
            }
            if is_max {
                found.push((dy, dx, v));
            }
        }
    }
    let best = best.unwrap_or_else(|| found.iter().map(|p| p.2).fold(0.0, f64::max));
    let cut = floor.max(rel * best);
    let mut sel: Vec<_> = found.into_iter().filter(|p| p.2 >= cut).collect();
    sel.sort_by(|a, b| b.2.total_cmp(&a.2).then((a.0, a.1).cmp(&(b.0, b.1))));
    sel
}

fn shifted(g: &Grid, dy: i64, dx: i64) -> (Grid, GridMask) {
    let (w, h) = (g.w as i64, g.h as i64);
    let mut out = Grid::new(g.w, g.h, 0.0);
    let mut inside = GridMask::new(g.w, g.h, false);
    for y in 0..h {
        let sy = y + dy;
        if sy < 0 || sy >= h {
            continue;
        }
        for x in 0..w {
            let sx = x + dx;
            if sx < 0 || sx >= w {
                continue;
            }
            out.data[(y * w + x) as usize] = g.data[(sy * w + sx) as usize];
            inside.data[(y * w + x) as usize] = true;
        }
    }
    (out, inside)
}

fn confidence(hp: &Grid, known: &GridMask, offs: &[(i64, i64, f64)]) -> Grid {
    let sigma = 3.0;
    let sq = |g: &Grid| Grid { w: g.w, h: g.h, data: g.data.iter().map(|v| v * v).collect() };
    let e0 = gaussian(&sq(hp), sigma);
    let mut acc = vec![0.0; hp.data.len()];
    let used = offs.iter().take(4).collect::<Vec<_>>();
    let known_g = known.as_grid();
    for &&(dy, dx, _) in &used {
        let (sh, inside) = shifted(hp, dy, dx);
        let (km, _) = shifted(&known_g, dy, dx);
        let both: Vec<f64> = (0..hp.data.len())
            .map(|i| if known.data[i] && inside.data[i] && km.data[i] > 0.5 { 1.0 } else { 0.0 })
            .collect();
        let num = gaussian(&Grid { w: hp.w, h: hp.h, data: (0..hp.data.len()).map(|i| hp.data[i] * sh.data[i] * both[i]).collect() }, sigma);
        let e1 = gaussian(&Grid { w: hp.w, h: hp.h, data: (0..hp.data.len()).map(|i| sh.data[i] * sh.data[i] * both[i]).collect() }, sigma);
        for (i, a) in acc.iter_mut().enumerate() {
            *a += num.data[i] / (e0.data[i] * e1.data[i] + 1e-6).sqrt();
        }
    }
    let n = used.len().max(1) as f64;
    let mut known_e0 = gather(&e0, known);
    let level = if known_e0.is_empty() { 0.0 } else { median(&mut known_e0) };
    Grid {
        w: hp.w,
        h: hp.h,
        data: (0..acc.len()).map(|i| if e0.data[i] < 0.15 * level { 0.0 } else { acc[i] / n }).collect(),
    }
}

/// Known pixels (not `hole`, not `avoid`) whose fine detail repeats one
/// offset away: the pixels a copy may take. Also the screen step of
/// [`super::postprocess`] ([`super::halftone`]).
pub(super) fn sources(raw: &Grid, hole: &GridMask, avoid: &GridMask, offs: &[(i64, i64, f64)]) -> GridMask {
    let known = hole.dilate(2).not().and_not(avoid);
    let hp = detail(raw, &known);
    let conf = confidence(&hp, &known, offs);
    let ring = known.and(&hole.dilate(60));
    let mut vals = gather(&conf, &ring);
    let reference = if vals.is_empty() { 0.0 } else { median(&mut vals) };
    let cut = (0.6 * reference).max(0.25);
    GridMask { w: raw.w, h: raw.h, data: (0..raw.data.len()).map(|i| known.data[i] && conf.data[i] >= cut).collect() }
}

struct Copy {
    out: Grid,
    done: GridMask,
    from: Vec<Option<usize>>,
}

fn onion(raw: &Grid, hole: &GridMask, src: &GridMask, tone: &Grid, offs: &[(i64, i64)]) -> Copy {
    const WDIST: f64 = 0.02;
    const CHAIN_COST: f64 = 4.0;
    const KEEP_BONUS: f64 = 0.75;
    let (w, h) = (raw.w as i64, raw.h as i64);
    let dist = distance_to(src);
    let mut order: Vec<usize> = (0..raw.data.len()).filter(|&i| hole.data[i]).collect();
    order.sort_by(|&a, &b| dist.data[a].total_cmp(&dist.data[b]));
    let mut out = raw.clone();
    let mut ok = src.data.clone();
    let mut filled = vec![false; raw.data.len()];
    let mut used_k = vec![usize::MAX; raw.data.len()];
    let mut from: Vec<Option<usize>> = vec![None; raw.data.len()];
    for &p in &order {
        let (x, y) = ((p as i64) % w, (p as i64) / w);
        let mut best = f64::INFINITY;
        let mut pick: Option<(usize, usize)> = None;
        for (k, &(dy, dx)) in offs.iter().enumerate() {
            let (sy, sx) = (y + dy, x + dx);
            if sy < 0 || sy >= h || sx < 0 || sx >= w {
                continue;
            }
            let s = (sy * w + sx) as usize;
            if !ok[s] {
                continue;
            }
            let dt = (tone.data[s] - tone.data[p]).abs();
            if dt > MAX_TONE_STEP {
                continue;
            }
            let mut d = dt + WDIST * ((dy * dy + dx * dx) as f64).sqrt();
            if filled[s] {
                d += CHAIN_COST;
            }
            for (ny, nx) in [(y - 1, x), (y + 1, x), (y, x - 1), (y, x + 1)] {
                if ny >= 0 && ny < h && nx >= 0 && nx < w && used_k[(ny * w + nx) as usize] == k {
                    d -= KEEP_BONUS;
                    break;
                }
            }
            if d < best {
                best = d;
                pick = Some((k, s));
            }
        }
        if let Some((k, s)) = pick {
            out.data[p] = out.data[s];
            ok[p] = true;
            filled[p] = true;
            used_k[p] = k;
            from[p] = if filled[s] { from[s] } else { Some(s) };
        }
    }
    Copy { out, done: GridMask { w: raw.w, h: raw.h, data: filled }, from }
}

fn coverage(out: &Grid, used: &GridMask, tone_f: &Grid) -> Grid {
    const LIMIT: f64 = 6.0;
    let tone_out = gaussian(out, 2.0);
    let gap = Grid {
        w: out.w,
        h: out.h,
        data: (0..out.data.len()).map(|i| if used.data[i] { tone_f.data[i] - tone_out.data[i] } else { 0.0 }).collect(),
    };
    let corr = gaussian(&gap, 3.0);
    let wts = gaussian(&used.as_grid(), 3.0);
    let corr: Vec<f64> = corr.data.iter().zip(&wts.data).map(|(c, w)| c / w.max(1e-6)).collect();
    let mut gaps: Vec<f64> = (0..corr.len()).filter(|&i| used.data[i]).map(|i| corr[i].abs()).collect();
    if gaps.is_empty() || median(&mut gaps) > LIMIT / 2.0 {
        return Grid::new(out.w, out.h, 0.0);
    }
    Grid {
        w: out.w,
        h: out.h,
        data: (0..corr.len()).map(|i| if used.data[i] { corr[i].clamp(-LIMIT, LIMIT) } else { 0.0 }).collect(),
    }
}

fn patchmatch_fill(gray: &Grid, hole: &GridMask) -> Option<Grid> {
    patchmatch::complete(&Image::from_grid(gray), hole, None, patchmatch::Options::default()).map(|im| im.channel(0))
}

/// Fill `hole` in the grayscale crop `gray`. `avoid` is every other detected
/// region: never copied from, never grown into.
pub fn fill(gray: &Grid, hole: &GridMask, avoid: &GridMask) -> Filled {
    fill_with(gray, hole, avoid, None)
}

/// [`fill`] with the tone field given rather than measured, for the Gradient
/// tool's **As screentone**: the page's own dots, in phase, at the
/// gradient's brightness. A given tone also means the hole is the user's
/// selection exactly, so the remnant guard does not run.
pub fn fill_with(gray: &Grid, hole: &GridMask, avoid: &GridMask, tone_override: Option<&Grid>) -> Filled {
    let n = gray.data.len();
    let used = if tone_override.is_some() { hole.clone() } else { grow(gray, hole, avoid) };
    let known = used.dilate(2).not().and_not(avoid);
    let hp = detail(gray, &known);
    const FAR: usize = 100;
    let ac = autocorr(&hp, &known, FAR);
    let near = peaks(&ac, FAR, 16, 0.55, 0.2, None);
    let zero = Grid::new(gray.w, gray.h, 0.0);
    if near.is_empty() {
        let out = patchmatch_fill(gray, &used).unwrap_or_else(|| gray.clone());
        return Filled { gray: out.select(&used, gray), used, from: vec![None; n], correction: zero, screen: false };
    }
    let src = sources(gray, &used, avoid, &near);
    let (tone, wts) = masked_blur(gray, &src, 2.0);
    let tone_known = GridMask { w: gray.w, h: gray.h, data: wts.data.iter().map(|&v| v >= 0.25).collect() };
    let tone_f = match tone_override {
        Some(t) => t.select(&used, &membrane(&tone, &tone_known)),
        None => membrane(&tone, &tone_known),
    };
    let far = peaks(&ac, FAR, FAR, 0.6, 0.3, Some(near[0].2));
    let mut offs: Vec<(i64, i64)> = near.iter().chain(&far).map(|p| (p.0, p.1)).collect();
    offs.sort();
    offs.dedup();
    let copy = onion(gray, &used, &src, &tone_f, &offs);
    let mut out = copy.out;
    let rest = used.and_not(&copy.done);
    if rest.any() {
        if let Some(pm) = patchmatch_fill(gray, &used) {
            out = pm.select(&rest, &out);
        }
    }
    let correction = coverage(&out, &used, &tone_f);
    for i in 0..n {
        if used.data[i] {
            out.data[i] += correction.data[i];
        } else {
            out.data[i] = gray.data[i];
        }
    }
    Filled { used, gray: out, from: copy.from, correction, screen: true }
}


/// The first screen step of [`super::postprocess`] (bench 3b, kept as the
/// [`super::postprocess::Finishing::RepeatOffsets`] choice): the page's own
/// dots, in phase, for the pixels of `mask` an edit model redrew with a screen
/// of its own.
///
/// `page` is the crop as the model was shown it, `answer` its answer after the
/// tone steps, both grey. The screen is measured on the page within
/// [`RESTORE_RING`] px of the mask, not over the whole crop, where art farther
/// out would dilute a screen that holds right up to the lettering; its
/// repeat offsets count only at a correlation of `min_corr`. A mask pixel is
/// restored when the background region it lies in (`protected` contours are
/// its walls) holds enough screen outside the mask, and it copies only from
/// that screen, aiming at the answer's own blurred tone. Returns the copy and
/// the weight to blend it at (the local screen confidence; zero where nothing
/// was copied), or `None` where the page shows no screen to restore.
pub(crate) fn restore(
    page: &Grid,
    answer: &Grid,
    mask: &GridMask,
    protected: &GridMask,
    inside: &GridMask,
    min_corr: f64,
) -> Option<(Grid, Grid)> {
    /// Screen sources a region needs before its mask pixels are restored, and
    /// in all.
    const REGION_SOURCES: usize = 150;
    const SOURCES: usize = 200;
    /// A source's own repeat confidence, and the window the weight is read in.
    const SOURCE_CONFIDENCE: f64 = 0.6;
    const WEIGHT_SIGMA: f64 = 8.0;
    const FAR: usize = 100;
    let n = page.data.len();
    let known = mask.dilate(2).not().and(inside);
    let hp = detail(page, &known);
    let from_mask = distance_to(mask);
    let near = GridMask { w: page.w, h: page.h, data: (0..n).map(|i| known.data[i] && from_mask.data[i] <= RESTORE_RING).collect() };
    let ac = autocorr(&hp.keep(&near, 0.0), &near, FAR);
    let offs: Vec<_> = peaks(&ac, FAR, 16, 0.55, 0.2, None).into_iter().filter(|p| p.2 >= min_corr).collect();
    let best = offs.first()?.2;
    let conf = confidence(&hp, &known, &offs);
    let src = GridMask { w: page.w, h: page.h, data: (0..n).map(|i| known.data[i] && !protected.data[i] && conf.data[i] >= SOURCE_CONFIDENCE).collect() };
    if src.count() < SOURCES {
        return None;
    }
    let (label, _) = components(&protected.not());
    let mut support = vec![0usize; label.iter().max().map_or(1, |m| m + 1)];
    for i in 0..n {
        if src.data[i] {
            support[label[i]] += 1;
        }
    }
    let target = GridMask {
        w: page.w,
        h: page.h,
        data: (0..n).map(|i| mask.data[i] && !protected.data[i] && support[label[i]] >= REGION_SOURCES).collect(),
    };
    if !target.any() {
        return None;
    }
    let far = peaks(&ac, FAR, FAR, 0.6, min_corr.max(0.3), Some(best));
    let mut all: Vec<(i64, i64)> = offs.iter().chain(&far).map(|p| (p.0, p.1)).collect();
    all.sort();
    all.dedup();
    let copy = onion(&page.keep(&mask.not(), 0.0), &target, &src, &gaussian(answer, 2.0), &all);
    let (strength, _) = masked_blur(&conf, &known, WEIGHT_SIGMA);
    let weight = Grid {
        w: page.w,
        h: page.h,
        data: (0..n).map(|i| if target.data[i] && copy.done.data[i] { strength.data[i].clamp(0.0, 1.0) } else { 0.0 }).collect(),
    };
    Some((copy.out, weight))
}

/// How far from the mask [`restore`] measures the page's screen, in px.
pub const RESTORE_RING: f64 = 48.0;

/// The app engine: the screentone fill over one fitted region of a page.
///
/// The hole is [`Fitted::ink`], the lettering and its fringe, as the model
/// rungs use it: the fit's wider mask can run far across a flat ring, and a
/// texture invented over a blob is the defect, not the cure. The remnant guard
/// then grows it over what a tight mask left behind, so the applied mask it
/// returns can reach up to [`REACH`] + [`RIM`] px past the ink.
///
/// The fill runs on the page's luma at 8 bits. A grayscale page takes the
/// result directly; a colour page takes each copied pixel's own colour from
/// the page pixel it was copied from, with the tone correction added to every
/// channel, and a PatchMatch completion in colour where the copy did not
/// reach. Alpha is copied through. It declines where the model rungs decline
/// (indexed, CMYK, one-bit, too large).
pub fn render(page: &Raster, fitted: &Fitted) -> Result<(Mask, Raster), super::model::Decline> {
    if let Some(decline) = super::model::declines(page, fitted) {
        return Err(decline);
    }
    let hole_page = &fitted.ink;
    let win = hole_page.bounds.grown(MARGIN as u32, page.width, page.height);
    let (w, h) = (win.w as usize, win.h as usize);
    let mut gray = Grid::new(w, h, 0.0);
    let mut hole = GridMask::new(w, h, false);
    for y in 0..h {
        for x in 0..w {
            let (px, py) = (win.x + x as i64, win.y + y as i64);
            gray.data[y * w + x] = page.luma16_at(px as u32, py as u32) as f64 / 257.0;
            hole.data[y * w + x] = hole_page.contains(px, py);
        }
    }
    let filled = fill(&gray, &hole, &GridMask::new(w, h, false));
    let idx: Vec<usize> = (0..w * h).filter(|&i| filled.used.data[i]).collect();
    let (x0, x1) = (idx.iter().map(|i| i % w).min().unwrap_or(0), idx.iter().map(|i| i % w).max().unwrap_or(0));
    let (y0, y1) = (idx.iter().map(|i| i / w).min().unwrap_or(0), idx.iter().map(|i| i / w).max().unwrap_or(0));
    let bounds = Rect::new(win.x + x0 as i64, win.y + y0 as i64, (x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32);
    let mut mask = Mask::empty(bounds);
    let mut pixels = super::model::page_crop(page, bounds);
    let max = match page.depth {
        BitDepth::Sixteen => 65535.0,
        other => ((1u32 << other.bits()) - 1) as f64,
    };
    let to_depth = |v: f64| (v.clamp(0.0, 255.0) / 255.0 * max).round() as u16;
    let colour = matches!(page.mode, ColorMode::Rgb | ColorMode::Rgba);
    let colour_pm = if colour && idx.iter().any(|&i| filled.from[i].is_none()) {
        let mut img = Image { w, h, c: 3, data: vec![0.0; w * h * 3] };
        for i in 0..w * h {
            let (px, py) = ((win.x + (i % w) as i64) as u32, (win.y + (i / w) as i64) as u32);
            for k in 0..3 {
                img.data[i * 3 + k] = page.sample(px, py, k) as f64 / max * 255.0;
            }
        }
        patchmatch::complete(&img, &filled.used, None, patchmatch::Options::default())
    } else {
        None
    };
    for &i in &idx {
        let (lx, ly) = (i % w, i / w);
        let (px, py) = (win.x + lx as i64, win.y + ly as i64);
        mask.set(px, py, true);
        let (cx, cy) = ((px - bounds.x) as u32, (py - bounds.y) as u32);
        if !colour {
            pixels.set_sample(cx, cy, 0, to_depth(filled.gray.data[i]));
            continue;
        }
        for k in 0..3 {
            let v = match (filled.from[i], &colour_pm) {
                (Some(s), _) => {
                    let (sx, sy) = ((win.x + (s % w) as i64) as u32, (win.y + (s / w) as i64) as u32);
                    page.sample(sx, sy, k) as f64 / max * 255.0 + filled.correction.data[i]
                }
                (None, Some(pm)) => pm.data[i * 3 + k],
                (None, None) => filled.gray.data[i],
            };
            pixels.set_sample(cx, cy, k, to_depth(v));
        }
    }
    Ok((mask, pixels))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 45 degree dot screen whose tone ramps from dark at the top to light at
    /// the bottom, like the sample page's blobs.
    fn screen_page(w: usize, h: usize) -> Grid {
        let period = 6.0;
        let data = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f64, (i / w) as f64);
                let coverage = 0.8 - 0.5 * y / h as f64; // ink share
                let (u, v) = ((x + y) / period, (x - y) / period);
                let cell = ((u * std::f64::consts::TAU).cos() + (v * std::f64::consts::TAU).cos()) / 2.0; // -1..1
                let threshold = 1.0 - 2.0 * coverage;
                if cell > threshold { 20.0 } else { 235.0 }
            })
            .collect();
        Grid { w, h, data }
    }

    fn letters(w: usize, h: usize) -> GridMask {
        // two thick strokes, like a column of lettering
        GridMask {
            w,
            h,
            data: (0..w * h)
                .map(|i| {
                    let (x, y) = (i % w, i / w);
                    ((100..112).contains(&x) && (90..210).contains(&y)) || ((80..140).contains(&x) && (140..150).contains(&y))
                })
                .collect(),
        }
    }

    #[test]
    fn fills_lettering_on_a_ramped_screen() {
        let (w, h) = (220, 300);
        let truth = screen_page(w, h);
        let hole = letters(w, h);
        let mut page = truth.clone();
        for i in 0..w * h {
            if hole.data[i] {
                page.data[i] = 255.0; // white lettering
            }
        }
        let f = fill(&page, &hole, &GridMask::new(w, h, false));
        assert!(f.screen);
        // tone inside the hole follows the ramp: compare blurred fill with blurred truth
        let a = gaussian(&f.gray, 3.0);
        let b = gaussian(&truth, 3.0);
        let err: f64 = (0..w * h).filter(|&i| hole.data[i]).map(|i| (a.data[i] - b.data[i]).abs()).sum::<f64>() / hole.count() as f64;
        assert!(err < 12.0, "tone error {err}");
        // no white lettering survives
        let white = (0..w * h).filter(|&i| hole.data[i] && f.gray.data[i] > 250.0 && truth.data[i] < 100.0).count();
        assert!(white * 4 < hole.count(), "{white} of {} pixels still white", hole.count());
    }

    #[test]
    fn flat_paper_goes_to_patchmatch() {
        let (w, h) = (120, 120);
        let page = Grid::new(w, h, 240.0);
        let hole = GridMask { w, h, data: (0..w * h).map(|i| (50..70).contains(&(i % w)) && (50..70).contains(&(i / w))).collect() };
        let f = fill(&page, &hole, &GridMask::new(w, h, false));
        assert!(!f.screen);
        assert!((0..w * h).filter(|&i| hole.data[i]).all(|i| (f.gray.data[i] - 240.0).abs() < 1.0));
    }

    /// **`restore` copies the page's dots back where a model drew its own.**
    /// The answer holds a softened screen under the lettering; the copy brings
    /// the page's crisp dots back in phase, and nothing outside the mask gets
    /// a weight.
    #[test]
    fn restore_copies_the_pages_dots_over_a_softened_screen() {
        let (w, h) = (220, 300);
        let page = screen_page(w, h);
        let mask = letters(w, h);
        let answer = gaussian(&page, 2.0).select(&mask, &page);
        let none = GridMask::new(w, h, false);
        let all = GridMask::new(w, h, true);
        let (copy, weight) = restore(&page, &answer, &mask, &none, &all, 0.8).expect("a screen to restore");
        assert!((0..w * h).all(|i| mask.data[i] || weight.data[i] == 0.0), "a weight outside the mask");
        let out = Grid { w, h, data: (0..w * h).map(|i| answer.data[i] * (1.0 - weight.data[i]) + copy.data[i] * weight.data[i]).collect() };
        let error = |g: &Grid| (0..w * h).filter(|&i| mask.data[i]).map(|i| (g.data[i] - page.data[i]).abs()).sum::<f64>() / mask.count() as f64;
        assert!(error(&out) < 0.5 * error(&answer), "{} against {}", error(&out), error(&answer));
    }

    /// **No screen, nothing restored.** Flat paper around the lettering.
    #[test]
    fn restore_declines_flat_paper() {
        let (w, h) = (220, 300);
        let page = Grid::new(w, h, 240.0);
        let mask = letters(w, h);
        let none = GridMask::new(w, h, false);
        let all = GridMask::new(w, h, true);
        assert!(restore(&page, &page, &mask, &none, &all, 0.8).is_none());
    }
}
