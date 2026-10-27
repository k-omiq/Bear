//! The reference tool's finishing v2 (`finishing.py`, "Manga Cleaner
//! Reproduction"), ported for the [`super::postprocess::Finishing`]
//! `Reference` versions. It runs on the answer at the page's resolution, per
//! tone channel, in place of this crate's own steps; the caller still writes
//! only through the applied mask.
//!
//! `finish_generation` is the entry point there and [`finish`] here. It reads
//! the answer in the page's mode (grey first when `force_grayscale` asks), then
//! one tone method:
//!
//! - **off**: the answer as it came.
//! - **global** (`imaging.match_context_tone`): one gain and offset per
//!   channel, fitted on a 15 px ring round the mask. The gain comes from the
//!   5 to 95 percentile spreads and stays within 0.85 to 1.15, the offset is
//!   the median residual within 32 levels. The arithmetic is single precision
//!   as numpy's is, so the fitted pair and the result match it exactly.
//! - **local** (`match_local_tone`): the global match, then the residual
//!   page minus answer outside the mask carried in by normalised convolution
//!   (`_context_field`): `blur(valid * residual) / blur(valid)` at the finest
//!   of 6, 12, 24, 48 and 96 px whose weight passes 0.15.
//! - **poisson** (`seamless_boundary`): the local result's low frequencies
//!   (a 3 px blur) cloned onto the page's by OpenCV's `seamlessClone`
//!   `NORMAL_CLONE`, and the high frequencies kept as the model drew them.
//! - **raster** (`restore_periodic_screen`): where the page around the mask
//!   is a regular dot screen, mask pixels copy page pixels a whole number of
//!   screen periods away, matched by region, tone and spread, so the screen
//!   continues in phase. Contours the model drew are protected.
//!
//! Local and poisson finish with `_protect_drawing`: every change fades to
//! nothing within 5 px of the answer's own contours (`_ink`), which are
//! oriented Canny edges, so drawn lines count and isotropic screen dots do not.
//!
//! The OpenCV calls are rebuilt here with the behaviour that decides the
//! result: `GaussianBlur`'s kernel size for float images (`round(8 sigma + 1)`,
//! odd) and reflect-101 border, `Sobel` 3 x 3, `Canny` on bytes with the L1
//! gradient and its integer direction test, `connectedComponentsWithStats`
//! (8-connected) and `connectedComponents` (4-connected), the 5 x 5 chamfer
//! `distanceTransform`, and `seamlessClone`'s guidance field, eroded mask and
//! truncating write, with its sine-transform solve done exactly in double
//! precision. Three things are not bit-exact. numpy's `default_rng(0)` and
//! `(1)` subsamples (12000 screen samples, 3000 evidence samples) are replaced
//! by a seeded sampler of the same counts: the fitted frequencies move by
//! about 1e-4 cycles per px, enough to reorder the repeat offsets, so raster
//! picks other (equally periodic) source pixels at some targets. OpenCV's
//! single-precision solve lands up to about 0.4 level under the exact one,
//! which its truncation turns into one level at some pixels. And float
//! rounding elsewhere differs from OpenCV's single precision by fractions of
//! a level.
//!
//! The details record has `finishing.py`'s keys; the reasons the raster step
//! declines with are English versions of its German strings.

use serde_json::{Value, json};

use super::grid::{Grid, Mask, components, gaussian_kernel, par_rows, reflect101};
use super::postprocess::Crop;

/// The reference tool's tone method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// No tone match: the answer as it came, grey if asked.
    Off,
    /// One gain and offset per channel from a ring round the mask.
    Global,
    /// The global match, then a normalised-convolution field of the residual.
    Local,
    /// The local match, then the low-frequency lighting solved by Poisson.
    Poisson,
    /// The page's own screen copied in phase over the generated texture.
    Raster,
}

impl Method {
    fn name(self) -> &'static str {
        match self {
            Method::Off => "off",
            Method::Global => "global",
            Method::Local => "local",
            Method::Poisson => "poisson",
            Method::Raster => "raster",
        }
    }
}

/// The answer finished by `method`, one grid per tone channel, and the
/// reference tool's details record.
///
/// `finish_generation(source, generated, mask, force_grayscale=grayscale,
/// tone_match=method != Off, tone_method=method)`. A grey colour crop is
/// finished as one channel and written to every channel: every step is per
/// channel or reads the grey of equal channels, so that is exact. Pixels
/// outside `crop.page` are no context: they never join a ring, a residual,
/// the Poisson boundary or the screen evidence. Like `finishing.py` it
/// returns the whole crop finished, outside the mask too; the caller keeps
/// what it writes.
pub fn finish(crop: &Crop<'_>, method: Method, grayscale: bool) -> (Vec<Grid>, Value) {
    let channels = crop.answer.len();
    let (mask, page) = (crop.mask, crop.page);
    assert!(
        crop.shown.len() == channels && crop.answer.iter().chain(crop.shown).all(|g| (g.w, g.h) == (mask.w, mask.h)),
        "finishing needs the page, the answer and the mask at the same size"
    );
    let shown: Vec<Grid> = crop.shown.iter().map(image).collect();
    let answer: Vec<Grid> = crop.answer.iter().map(image).collect();
    let (reference, generated) = if grayscale { (vec![grey(&shown)], vec![grey(&answer)]) } else { (shown, answer) };
    // the details list one fitted pair per channel of the page's mode
    let copies = if grayscale { channels } else { 1 };
    let mut details = json!({
        "version": 2,
        "force_grayscale": grayscale,
        "tone_match": method != Method::Off,
        "method": method.name(),
    });
    let result = match method {
        Method::Off => generated,
        Method::Global => {
            let (result, fitted) = match_context_tone(&reference, &generated, mask, page);
            details["global"] = affine_json(&fitted, copies);
            result
        }
        Method::Local | Method::Poisson => {
            let (mut result, fitted, correction) = match_local_tone(&reference, &generated, mask, page);
            details["local"] = json!({ "affine": affine_json(&fitted, copies), "local_max_correction": single(correction) });
            if method == Method::Poisson {
                result = seamless_boundary(&reference, &result, mask, page);
            }
            let (result, protected) = protect_drawing(&generated, &result, mask);
            details["protected_pixels"] = json!(protected);
            result
        }
        Method::Raster => {
            let (result, raster) = restore_periodic_screen(&reference, &generated, mask, page);
            details["raster"] = raster;
            result
        }
    };
    let result = if grayscale { vec![grey(&result); channels] } else { result };
    (result, details)
}

/// One grey channel the way PIL's `convert("L")` makes it: ITU-R 601-2 luma
/// in PIL's 16-bit fixed point, `(R*19595 + G*38470 + B*7471 + 0x8000) >> 16`,
/// on the channels read as bytes. One channel is already grey.
pub fn grey(channels: &[Grid]) -> Grid {
    if let [only] = channels {
        return only.clone();
    }
    let (w, h) = (channels[0].w, channels[0].h);
    let byte = |g: &Grid, i: usize| (g.data[i] + 0.5).clamp(0.0, 255.0) as u32;
    let level = |i: usize| {
        let sum = byte(&channels[0], i) * 19595 + byte(&channels[1], i) * 38470 + byte(&channels[2], i) * 7471;
        f64::from((sum + 0x8000) >> 16)
    };
    Grid { w, h, data: (0..w * h).map(level).collect() }
}

fn affine_json(fitted: &[(f64, f64)], copies: usize) -> Value {
    let pairs: Vec<Value> = fitted.iter().map(|&(gain, offset)| json!({ "gain": gain, "offset": offset })).collect();
    if pairs.len() == 1 { Value::Array(vec![pairs[0].clone(); copies]) } else { Value::Array(pairs) }
}

/// A value as numpy's `float32` hands it to `float()`.
fn single(v: f64) -> f64 {
    v as f32 as f64
}

/// `np.rint(v).clip(0, 255)`: a byte level, ties to even.
fn level(v: f64) -> f64 {
    v.round_ties_even().clamp(0.0, 255.0)
}

/// `_image`: a grid rounded to byte levels.
fn image(g: &Grid) -> Grid {
    map(g, level)
}

fn map(g: &Grid, f: impl Fn(f64) -> f64) -> Grid {
    Grid { w: g.w, h: g.h, data: g.data.iter().map(|&v| f(v)).collect() }
}

fn zip(a: &Grid, b: &Grid, f: impl Fn(f64, f64) -> f64) -> Grid {
    Grid { w: a.w, h: a.h, data: a.data.iter().zip(&b.data).map(|(&x, &y)| f(x, y)).collect() }
}

fn flags(w: usize, h: usize, f: impl Fn(usize) -> bool) -> Mask {
    Mask { w, h, data: (0..w * h).map(f).collect() }
}

// ---------------------------------------------------------------------------
// Blur

/// A half-open pixel rectangle.
#[derive(Clone, Copy, Debug)]
struct Rect {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
}

/// The bounds of the pixels `pick` takes (`cv2.boundingRect`).
fn bounds(w: usize, h: usize, pick: impl Fn(usize) -> bool) -> Option<Rect> {
    let mut r = Rect { x0: w, y0: h, x1: 0, y1: 0 };
    for y in 0..h {
        for x in 0..w {
            if pick(y * w + x) {
                r = Rect { x0: r.x0.min(x), y0: r.y0.min(y), x1: r.x1.max(x + 1), y1: r.y1.max(y + 1) };
            }
        }
    }
    (r.x1 > r.x0).then_some(r)
}

/// `cv2.GaussianBlur(g, (0, 0), sigma)` of every grid in `inputs` (each
/// `w x h`), evaluated inside `rect` only: each output is the rectangle's
/// size. The context fields read their largest scales in a small part of the
/// crop, and a 96 px blur is a 769-tap kernel.
fn blur_in(inputs: &[&[f64]], w: usize, h: usize, sigma: f64, rect: Rect) -> Vec<Vec<f64>> {
    let k = gaussian_kernel(sigma);
    let r = k.len() / 2;
    let (bw, bh) = (rect.x1 - rect.x0, rect.y1 - rect.y0);
    let xs: Vec<usize> = (0..bw + 2 * r).map(|t| reflect101(rect.x0 as isize + t as isize - r as isize, w)).collect();
    let ys: Vec<usize> = (0..bh + 2 * r).map(|t| reflect101(rect.y0 as isize + t as isize - r as isize, h)).collect();
    // the source rows the vertical pass reads, each blurred across once
    let mut slot = vec![usize::MAX; h];
    for &y in &ys {
        slot[y] = 0;
    }
    let rows: Vec<usize> = (0..h).filter(|&y| slot[y] == 0).collect();
    for (i, &y) in rows.iter().enumerate() {
        slot[y] = i;
    }
    inputs
        .iter()
        .map(|data| {
            let mut across = vec![0.0; rows.len() * bw];
            par_rows(&mut across, bw, rows.len() * bw * k.len(), |i, out| {
                let src = &data[rows[i] * w..(rows[i] + 1) * w];
                let ext: Vec<f64> = xs.iter().map(|&x| src[x]).collect();
                for (j, kv) in k.iter().enumerate() {
                    for (o, e) in out.iter_mut().zip(&ext[j..j + bw]) {
                        *o += kv * e;
                    }
                }
            });
            let mut out = vec![0.0; bw * bh];
            par_rows(&mut out, bw, bw * bh * k.len(), |y, out| {
                for (j, kv) in k.iter().enumerate() {
                    let s = slot[ys[y + j]];
                    for (o, t) in out.iter_mut().zip(&across[s * bw..(s + 1) * bw]) {
                        *o += kv * t;
                    }
                }
            });
            out
        })
        .collect()
}

/// `cv2.GaussianBlur(g, (0, 0), sigma)` over the whole grid.
fn blur(g: &Grid, sigma: f64) -> Grid {
    let rect = Rect { x0: 0, y0: 0, x1: g.w, y1: g.h };
    let data = blur_in(&[&g.data], g.w, g.h, sigma, rect).remove(0);
    Grid { w: g.w, h: g.h, data }
}

/// Reflect-101 padding by `pad` on every side (`cv2.copyMakeBorder`).
fn pad(g: &Grid, pad: usize) -> Grid {
    let (w, h) = (g.w + 2 * pad, g.h + 2 * pad);
    let data = (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w) as isize - pad as isize, (i / w) as isize - pad as isize);
            g.data[reflect101(y, g.h) * g.w + reflect101(x, g.w)]
        })
        .collect();
    Grid { w, h, data }
}

// ---------------------------------------------------------------------------
// Contours

/// `cv2.Sobel(g, CV_32F, 1, 0)` and `(0, 1)`, 3 x 3, reflect-101 border.
fn sobel(g: &Grid) -> (Grid, Grid) {
    let (w, h) = (g.w, g.h);
    let at = |x: isize, y: isize| g.data[reflect101(y, h) * w + reflect101(x, w)];
    let mut gx = Grid::new(w, h, 0.0);
    let mut gy = Grid::new(w, h, 0.0);
    for y in 0..h as isize {
        for x in 0..w as isize {
            let i = y as usize * w + x as usize;
            gx.data[i] = (at(x + 1, y - 1) - at(x - 1, y - 1)) + 2.0 * (at(x + 1, y) - at(x - 1, y)) + (at(x + 1, y + 1) - at(x - 1, y + 1));
            gy.data[i] = (at(x - 1, y + 1) - at(x - 1, y - 1)) + 2.0 * (at(x, y + 1) - at(x, y - 1)) + (at(x + 1, y + 1) - at(x + 1, y - 1));
        }
    }
    (gx, gy)
}

/// `cv2.Canny(bytes, low, high)`: 3 x 3 Sobel with replicated border, L1
/// magnitude, OpenCV's fixed-point direction sectors (tan 22.5 degrees in Q15)
/// with its strict and non-strict neighbour tests, then hysteresis over the
/// eight neighbours.
fn canny(bytes: &Grid, low: i32, high: i32) -> Mask {
    const TG22: i64 = 13573; // (0.4142135623730950 * (1 << 15) + 0.5) as int
    let (w, h) = (bytes.w, bytes.h);
    let v: Vec<i32> = bytes.data.iter().map(|&p| p as i32).collect();
    let at = |x: isize, y: isize| v[y.clamp(0, h as isize - 1) as usize * w + x.clamp(0, w as isize - 1) as usize];
    let mut dx = vec![0i32; w * h];
    let mut dy = vec![0i32; w * h];
    let mut mag = vec![0i32; w * h];
    for y in 0..h as isize {
        for x in 0..w as isize {
            let i = y as usize * w + x as usize;
            dx[i] = (at(x + 1, y - 1) + 2 * at(x + 1, y) + at(x + 1, y + 1)) - (at(x - 1, y - 1) + 2 * at(x - 1, y) + at(x - 1, y + 1));
            dy[i] = (at(x - 1, y + 1) + 2 * at(x, y + 1) + at(x + 1, y + 1)) - (at(x - 1, y - 1) + 2 * at(x, y - 1) + at(x + 1, y - 1));
            mag[i] = dx[i].abs() + dy[i].abs();
        }
    }
    let m_at = |x: isize, y: isize| {
        if x < 0 || y < 0 || x >= w as isize || y >= h as isize { 0 } else { mag[y as usize * w + x as usize] }
    };
    // 0 a candidate, 1 no edge, 2 an edge
    let mut map = vec![1u8; w * h];
    let mut stack = Vec::new();
    for y in 0..h as isize {
        for x in 0..w as isize {
            let i = y as usize * w + x as usize;
            let m = mag[i];
            if m <= low {
                continue;
            }
            let (xs, ys) = (dx[i], dy[i]);
            let ax = i64::from(xs.abs());
            let ay = i64::from(ys.abs()) << 15;
            let tg22x = ax * TG22;
            let peak = if ay < tg22x {
                m > m_at(x - 1, y) && m >= m_at(x + 1, y)
            } else if ay > tg22x + (ax << 16) {
                m > m_at(x, y - 1) && m >= m_at(x, y + 1)
            } else {
                let s = if (xs ^ ys) < 0 { -1 } else { 1 };
                m > m_at(x - s, y - 1) && m > m_at(x + s, y + 1)
            };
            if peak {
                if m > high {
                    map[i] = 2;
                    stack.push(i);
                } else {
                    map[i] = 0;
                }
            }
        }
    }
    while let Some(i) = stack.pop() {
        let (x, y) = ((i % w) as isize, (i / w) as isize);
        for ny in y - 1..=y + 1 {
            for nx in x - 1..=x + 1 {
                if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                    continue;
                }
                let j = ny as usize * w + nx as usize;
                if map[j] == 0 {
                    map[j] = 2;
                    stack.push(j);
                }
            }
        }
    }
    Mask { w, h, data: map.into_iter().map(|m| m == 2).collect() }
}

/// `_ink`: drawn contours. Canny edges (15, 40) after a 1 px blur, kept where
/// the 3.5 px structure tensor is oriented (coherence over 0.45) and in pieces
/// of at least 8 px that span 10; an isotropic dot screen gives neither. Solid
/// black (under 45 after a 2 px blur) counts too.
fn ink(gray: &Grid) -> Mask {
    let (w, h) = (gray.w, gray.h);
    let smooth = blur(gray, 1.0);
    let (sx, sy) = sobel(&smooth);
    let (gx, gy) = (map(&sx, |v| v / 8.0), map(&sy, |v| v / 8.0));
    let xx = blur(&zip(&gx, &gx, |a, b| a * b), 3.5);
    let yy = blur(&zip(&gy, &gy, |a, b| a * b), 3.5);
    let xy = blur(&zip(&gx, &gy, |a, b| a * b), 3.5);
    let edges = canny(&image(&smooth), 15, 40);
    let oriented = flags(w, h, |i| {
        let (a, b, c) = (xx.data[i], yy.data[i], xy.data[i]);
        edges.data[i] && ((a - b).powi(2) + 4.0 * c * c).sqrt() / (a + b + 1e-6) > 0.45
    });
    let (labels, sizes) = components(&oriented);
    let mut span = vec![(usize::MAX, usize::MAX, 0usize, 0usize); sizes.len()];
    for (i, &l) in labels.iter().enumerate() {
        if l > 0 {
            let (x, y) = (i % w, i / w);
            let s = &mut span[l];
            *s = (s.0.min(x), s.1.min(y), s.2.max(x), s.3.max(y));
        }
    }
    let accepted: Vec<bool> = sizes
        .iter()
        .zip(&span)
        .enumerate()
        .map(|(l, (&size, s))| l > 0 && size >= 8 && (s.2 - s.0 + 1).max(s.3 - s.1 + 1) >= 10)
        .collect();
    let solid = blur(gray, 2.0);
    flags(w, h, |i| accepted[labels[i]] || solid.data[i] < 45.0)
}

/// `cv2.distanceTransform(src, DIST_L2, 5)` to the `zero` pixels: the 5 x 5
/// chamfer (1, 1.4, 2.1969) in 16-bit fixed point, two passes, pixels off the
/// grid infinitely far.
fn chamfer(zero: &Mask) -> Vec<f32> {
    const SHIFT: u32 = 16;
    const FAR: u32 = i32::MAX as u32;
    const CAP: u32 = (i32::MAX >> 2) as u32;
    let fix = |m: f32| (m * (1u32 << SHIFT) as f32).round() as u32;
    let (hv, diag, long) = (fix(1.0), fix(1.4), fix(2.1969));
    let scale = 1.0f32 / (1u32 << SHIFT) as f32;
    let (w, h) = (zero.w, zero.h);
    let pw = w + 4;
    let mut t = vec![FAR; pw * (h + 4)];
    for y in 0..h {
        for x in 0..w {
            let p = (y + 2) * pw + x + 2;
            t[p] = if zero.data[y * w + x] {
                0
            } else {
                [
                    t[p - 2 * pw - 1] + long,
                    t[p - 2 * pw + 1] + long,
                    t[p - pw - 2] + long,
                    t[p - pw - 1] + diag,
                    t[p - pw] + hv,
                    t[p - pw + 1] + diag,
                    t[p - pw + 2] + long,
                    t[p - 1] + hv,
                ]
                .into_iter()
                .min()
                .unwrap_or(FAR)
            };
        }
    }
    let mut out = vec![0.0f32; w * h];
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let p = (y + 2) * pw + x + 2;
            let mut t0 = t[p];
            if t0 > hv {
                t0 = [
                    t0,
                    t[p + 2 * pw + 1] + long,
                    t[p + 2 * pw - 1] + long,
                    t[p + pw + 2] + long,
                    t[p + pw + 1] + diag,
                    t[p + pw] + hv,
                    t[p + pw - 1] + diag,
                    t[p + pw - 2] + long,
                    t[p + 1] + hv,
                ]
                .into_iter()
                .min()
                .unwrap_or(t0);
                t[p] = t0;
            }
            out[y * w + x] = t0.min(CAP) as f32 * scale;
        }
    }
    out
}

/// `_structure_weight`: 0 within 4 px of a contour, rising to 1 five px
/// further out.
fn structure_weight(ink: &Mask) -> Grid {
    let core = ink.dilate(4);
    if !core.any() {
        return Grid::new(ink.w, ink.h, 1.0);
    }
    let data = chamfer(&core).into_iter().map(|d| f64::from((d / 5.0).clamp(0.0, 1.0))).collect();
    Grid { w: ink.w, h: ink.h, data }
}

/// `_protect_drawing`: `processed` faded back to `original` near the
/// original's contours, and how many mask pixels it left entirely alone.
fn protect_drawing(original: &[Grid], processed: &[Grid], mask: &Mask) -> (Vec<Grid>, usize) {
    let weight = structure_weight(&ink(&grey(original)));
    let result = original
        .iter()
        .zip(processed)
        .map(|(before, after)| {
            let data = (0..weight.data.len())
                .map(|i| {
                    // numpy's float32 arithmetic, for the ties `rint` sees
                    let b = before.data[i] as f32;
                    level(f64::from(b + (after.data[i] as f32 - b) * weight.data[i] as f32))
                })
                .collect();
            Grid { w: weight.w, h: weight.h, data }
        })
        .collect();
    let protected = (0..mask.data.len()).filter(|&i| mask.data[i] && weight.data[i] == 0.0).count();
    (result, protected)
}

// ---------------------------------------------------------------------------
// Tone

/// numpy's default (linear) percentile of sorted values, with its lerp.
fn quantile(sorted: &[f64], q: f64) -> f64 {
    let pos = q / 100.0 * (sorted.len() - 1) as f64;
    let lo = pos.floor();
    let t = pos - lo;
    let (a, b) = (sorted[lo as usize], sorted[(lo as usize + 1).min(sorted.len() - 1)]);
    let d = b - a;
    if t >= 0.5 { b - d * (1.0 - t) } else { a + d * t }
}

/// `match_context_tone`: a gain and offset per channel from the 15 px ring
/// round the mask (all context when the ring is under 64 px), in numpy's
/// single precision. Returns the matched answer and the fitted pairs, none
/// when there is no context.
fn match_context_tone(source: &[Grid], generated: &[Grid], mask: &Mask, page: &Mask) -> (Vec<Grid>, Vec<(f64, f64)>) {
    let mut ring = mask.dilate(15).and_not(mask).and(page);
    if ring.count() < 64 {
        ring = mask.not().and(page);
    }
    if ring.count() < 64 {
        return (generated.to_vec(), Vec::new());
    }
    let mut idx: Vec<usize> = (0..ring.data.len()).filter(|&i| ring.data[i]).collect();
    if idx.len() > 20000 {
        let step = idx.len() / 20000;
        idx = idx.into_iter().step_by(step).collect();
    }
    let mut fitted = Vec::new();
    let result = source
        .iter()
        .zip(generated)
        .map(|(original, edited)| {
            let sorted = |g: &Grid| {
                let mut v: Vec<f64> = idx.iter().map(|&i| g.data[i]).collect();
                v.sort_by(f64::total_cmp);
                v
            };
            let (a, b) = (sorted(original), sorted(edited));
            let (lo_a, hi_a, lo_b, hi_b) = (quantile(&a, 5.0), quantile(&a, 95.0), quantile(&b, 5.0), quantile(&b, 95.0));
            let gain = if hi_b - lo_b > 15.0 && hi_a - lo_a > 15.0 { ((hi_a - lo_a) / (hi_b - lo_b)).clamp(0.85, 1.15) } else { 1.0 };
            let g32 = gain as f32;
            let mut residual: Vec<f32> = idx.iter().map(|&i| original.data[i] as f32 - edited.data[i] as f32 * g32).collect();
            residual.sort_by(f32::total_cmp);
            let n = residual.len();
            let median = if n % 2 == 1 { residual[n / 2] } else { (residual[n / 2 - 1] + residual[n / 2]) / 2.0 };
            let offset = median.clamp(-32.0, 32.0);
            fitted.push((gain, f64::from(offset)));
            map(edited, |e| level(f64::from(e as f32 * g32 + offset)))
        })
        .collect();
    (result, fitted)
}

/// `_context_field`: `values` known on `valid` carried everywhere by
/// normalised convolution, `blur(valid * values) / blur(valid)` at the
/// finest of 6, 12, 24, 48 and 96 px whose weight passes 0.15, the mean of
/// the known values where none does; zero with fewer than 16 known pixels.
///
/// With `needed`, only those pixels are worked out (each still exactly as
/// the full field has it); the rest are left at zero or the mean.
fn context_field(values: &[&Grid], valid: &Mask, needed: Option<&Mask>) -> Vec<Grid> {
    let (w, h) = (valid.w, valid.h);
    let mut result: Vec<Grid> = values.iter().map(|_| Grid::new(w, h, 0.0)).collect();
    if valid.count() < 16 {
        return result;
    }
    let known = valid.as_grid();
    let weighted: Vec<Vec<f64>> = values
        .iter()
        .map(|v| v.data.iter().zip(&valid.data).map(|(&x, &k)| if k { x } else { 0.0 }).collect())
        .collect();
    let mut inputs: Vec<&[f64]> = vec![&known.data];
    inputs.extend(weighted.iter().map(|v| v.as_slice()));
    let mut assigned = vec![false; w * h];
    let need = |i: usize| needed.is_none_or(|m| m.data[i]);
    for sigma in [6.0, 12.0, 24.0, 48.0, 96.0] {
        let Some(rect) = bounds(w, h, |i| need(i) && !assigned[i]) else { break };
        let blurred = blur_in(&inputs, w, h, sigma, rect);
        let bw = rect.x1 - rect.x0;
        for y in rect.y0..rect.y1 {
            for x in rect.x0..rect.x1 {
                let (i, t) = (y * w + x, (y - rect.y0) * bw + x - rect.x0);
                let weight = blurred[0][t];
                if weight > 0.15 && !assigned[i] {
                    for (c, out) in result.iter_mut().enumerate() {
                        out.data[i] = blurred[c + 1][t] / weight;
                    }
                    assigned[i] = true;
                }
            }
        }
    }
    if assigned.iter().any(|&a| !a) {
        let n = valid.count() as f64;
        for (c, out) in result.iter_mut().enumerate() {
            let mean = values[c].data.iter().zip(&valid.data).filter(|(_, &k)| k).map(|(&v, _)| v).sum::<f64>() / n;
            for (o, &a) in out.data.iter_mut().zip(&assigned) {
                if !a {
                    *o = mean;
                }
            }
        }
    }
    result
}

/// `match_local_tone`: the global match, then the page-minus-answer
/// residual outside the mask carried across it. Returns the result, the
/// global pairs and the largest correction.
fn match_local_tone(source: &[Grid], generated: &[Grid], mask: &Mask, page: &Mask) -> (Vec<Grid>, Vec<(f64, f64)>, f64) {
    let (initial, fitted) = match_context_tone(source, generated, mask, page);
    let known = mask.not().and(page);
    let residual: Vec<Grid> = source.iter().zip(&initial).map(|(s, e)| zip(s, e, |a, b| a - b)).collect();
    let correction = context_field(&residual.iter().collect::<Vec<_>>(), &known, None);
    let largest = correction.iter().flat_map(|g| g.data.iter()).fold(0.0f64, |m, v| m.max(v.abs()));
    let result = initial.iter().zip(&correction).map(|(e, c)| zip(e, c, |a, b| level(a + b))).collect();
    (result, fitted, largest)
}

// ---------------------------------------------------------------------------
// Poisson

/// `seamless_boundary`: the answer's 3 px lighting cloned onto the page's
/// (the page with the answer inside the mask, blurred the same) by
/// `seamlessClone`, padded 32 px so a mask at the crop's edge still solves,
/// over the mask grown 8 px. The answer keeps its own detail on top.
///
/// Off `page` the destination is the answer too, as inside the mask: what the
/// crop holds there is no page, and as the clone's boundary it would pull the
/// patch toward it.
fn seamless_boundary(source: &[Grid], generated: &[Grid], mask: &Mask, page: &Mask) -> Vec<Grid> {
    const PAD: usize = 32;
    let (w, h) = (mask.w, mask.h);
    if !mask.any() || w.min(h) < 3 {
        return generated.to_vec();
    }
    let padded = pad(&mask.as_grid(), PAD);
    let alpha = Mask { w: padded.w, h: padded.h, data: padded.data.iter().map(|&v| v > 0.0).collect() }.dilate(8);
    let Some(r) = bounds(alpha.w, alpha.h, |i| alpha.data[i]) else { return generated.to_vec() };
    let center = (r.x0 + (r.x1 - r.x0) / 2, r.y0 + (r.y1 - r.y0) / 2);
    let answered = mask.or(&page.not());
    generated
        .iter()
        .zip(source)
        .map(|(edited, shown)| {
            let destination = edited.select(&answered, shown);
            let lighting = blur(edited, 3.0);
            let destination = blur(&destination, 3.0);
            let cloned = seamless_clone(&pad(&image(&lighting), PAD), &pad(&image(&destination), PAD), &alpha, center);
            let pw = w + 2 * PAD;
            let data = (0..w * h)
                .map(|i| level(edited.data[i] + cloned.data[(i / w + PAD) * pw + i % w + PAD] - lighting.data[i]))
                .collect();
            Grid { w, h, data }
        })
        .collect()
}

/// `cv2.seamlessClone(src, dst, alpha, center, NORMAL_CLONE)` for one
/// channel of byte levels. The mask loses its outer pixel row; the clone's
/// box is the mask's bounds, centred on `center` in `dst`. Inside the mask
/// eroded 3 px the guidance is `src`'s forward differences, elsewhere `dst`'s;
/// the box's edge is held at `dst` and the interior solved exactly, then
/// truncated to bytes as OpenCV writes it.
fn seamless_clone(src: &Grid, dst: &Grid, alpha: &Mask, center: (usize, usize)) -> Grid {
    let (w, h) = (dst.w, dst.h);
    let mut mask = alpha.clone();
    for y in 0..h {
        for x in 0..w {
            if x == 0 || y == 0 || x == w - 1 || y == h - 1 {
                mask.data[y * w + x] = false;
            }
        }
    }
    let Some(r) = bounds(w, h, |i| mask.data[i]) else { return dst.clone() };
    let (sw, sh) = (r.x1 - r.x0, r.y1 - r.y0);
    let (dx0, dy0) = (center.0 as isize - (sw / 2) as isize, center.1 as isize - (sh / 2) as isize);
    if sw < 3 || sh < 3 || dx0 < 0 || dy0 < 0 || dx0 as usize + sw > w || dy0 as usize + sh > h {
        return dst.clone();
    }
    let (dx0, dy0) = (dx0 as usize, dy0 as usize);
    let eroded = mask.erode(3);
    let d = |x: usize, y: usize| dst.data[(dy0 + y) * w + dx0 + x];
    let p = |x: usize, y: usize| {
        let i = (r.y0 + y) * w + r.x0 + x;
        if mask.data[i] { src.data[i] } else { 0.0 }
    };
    let inner = |x: usize, y: usize| eroded.data[(r.y0 + y) * w + r.x0 + x];
    let gx = |x: usize, y: usize| if inner(x, y) { p(x + 1, y) - p(x, y) } else { d(x + 1, y) - d(x, y) };
    let gy = |x: usize, y: usize| if inner(x, y) { p(x, y + 1) - p(x, y) } else { d(x, y + 1) - d(x, y) };
    let (n, m) = (sw - 2, sh - 2);
    let mut b = vec![0.0; n * m];
    for j in 0..m {
        for i in 0..n {
            let (x, y) = (i + 1, j + 1);
            let mut v = gx(x, y) - gx(x - 1, y) + gy(x, y) - gy(x, y - 1);
            if x == 1 {
                v -= d(0, y);
            }
            if x == sw - 2 {
                v -= d(sw - 1, y);
            }
            if y == 1 {
                v -= d(x, 0);
            }
            if y == sh - 2 {
                v -= d(x, sh - 1);
            }
            b[j * n + i] = v;
        }
    }
    let u = solve_dirichlet(&b, n, m);
    let mut out = dst.clone();
    for j in 0..m {
        for i in 0..n {
            // OpenCV truncates rather than rounds here
            let v = u[j * n + i] as f32;
            out.data[(dy0 + j + 1) * w + dx0 + i + 1] = if v < 0.0 { 0.0 } else if v > 255.0 { 255.0 } else { f64::from(v.trunc()) };
        }
    }
    out
}

/// The orthonormal DST-I matrix of size `n` (its own inverse).
fn sine_matrix(n: usize) -> Vec<f64> {
    let scale = (2.0 / (n + 1) as f64).sqrt();
    let period = 2 * (n + 1);
    let mut s = vec![0.0; n * n];
    for k in 0..n {
        for i in 0..n {
            let t = ((k + 1) * (i + 1)) % period;
            s[k * n + i] = scale * (std::f64::consts::PI * t as f64 / (n + 1) as f64).sin();
        }
    }
    s
}

/// `rows x cols` grid `a` times `cols x cols` matrix `s`, row by row.
fn times(a: &[f64], s: &[f64], rows: usize, cols: usize) -> Vec<f64> {
    let mut out = vec![0.0; rows * cols];
    par_rows(&mut out, cols, rows * cols * cols, |j, out| {
        for (i, &v) in a[j * cols..(j + 1) * cols].iter().enumerate() {
            for (o, sv) in out.iter_mut().zip(&s[i * cols..(i + 1) * cols]) {
                *o += v * sv;
            }
        }
    });
    out
}

/// `rows x rows` matrix `s` times `rows x cols` grid `a`.
fn times_left(s: &[f64], a: &[f64], rows: usize, cols: usize) -> Vec<f64> {
    let mut out = vec![0.0; rows * cols];
    par_rows(&mut out, cols, rows * rows * cols, |l, out| {
        for (j, &sv) in s[l * rows..(l + 1) * rows].iter().enumerate() {
            for (o, v) in out.iter_mut().zip(&a[j * cols..(j + 1) * cols]) {
                *o += sv * v;
            }
        }
    });
    out
}

/// The five-point Poisson equation `laplacian(u) = b` on an `n x m`
/// interior with zero boundary, solved exactly in the sine basis (what
/// OpenCV's `Cloning::solve` does with its DFT-built sine transform).
fn solve_dirichlet(b: &[f64], n: usize, m: usize) -> Vec<f64> {
    let (sn, sm) = (sine_matrix(n), sine_matrix(m));
    let mut c = times_left(&sm, &times(b, &sn, m, n), m, n);
    for l in 0..m {
        let ey = 2.0 * (std::f64::consts::PI * (l + 1) as f64 / (m + 1) as f64).cos() - 2.0;
        for k in 0..n {
            let ex = 2.0 * (std::f64::consts::PI * (k + 1) as f64 / (n + 1) as f64).cos() - 2.0;
            c[l * n + k] /= ex + ey;
        }
    }
    times_left(&sm, &times(&c, &sn, m, n), m, n)
}

// ---------------------------------------------------------------------------
// Raster

#[derive(Clone, Copy, Default)]
struct Cx {
    re: f64,
    im: f64,
}

impl Cx {
    fn mul(self, o: Cx) -> Cx {
        Cx { re: self.re * o.re - self.im * o.im, im: self.re * o.im + self.im * o.re }
    }
    fn conj(self) -> Cx {
        Cx { re: self.re, im: -self.im }
    }
}

/// In-place radix-2 FFT; `inverse` does not scale.
fn fft_pow2(a: &mut [Cx], inverse: bool) {
    let n = a.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            a.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = 2.0 * std::f64::consts::PI / len as f64 * if inverse { 1.0 } else { -1.0 };
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let wv = Cx { re: (ang * k as f64).cos(), im: (ang * k as f64).sin() };
                let u = a[start + k];
                let v = a[start + k + len / 2].mul(wv);
                a[start + k] = Cx { re: u.re + v.re, im: u.im + v.im };
                a[start + k + len / 2] = Cx { re: u.re - v.re, im: u.im - v.im };
            }
        }
        len <<= 1;
    }
}

/// A forward DFT of one length: radix 2 when it is a power of two,
/// Bluestein's chirp otherwise (`np.fft` takes any length).
struct Dft {
    n: usize,
    m: usize,
    chirp: Vec<Cx>,
    filter: Vec<Cx>,
}

impl Dft {
    fn new(n: usize) -> Dft {
        if n.is_power_of_two() {
            return Dft { n, m: n, chirp: Vec::new(), filter: Vec::new() };
        }
        let m = (2 * n - 1).next_power_of_two();
        let chirp: Vec<Cx> = (0..n)
            .map(|k| {
                let t = (k * k) % (2 * n);
                let a = -std::f64::consts::PI * t as f64 / n as f64;
                Cx { re: a.cos(), im: a.sin() }
            })
            .collect();
        let mut filter = vec![Cx::default(); m];
        filter[0] = chirp[0].conj();
        for k in 1..n {
            filter[k] = chirp[k].conj();
            filter[m - k] = chirp[k].conj();
        }
        fft_pow2(&mut filter, false);
        Dft { n, m, chirp, filter }
    }

    fn run(&self, a: &mut [Cx]) {
        if self.chirp.is_empty() {
            fft_pow2(a, false);
            return;
        }
        let mut buf = vec![Cx::default(); self.m];
        for k in 0..self.n {
            buf[k] = a[k].mul(self.chirp[k]);
        }
        fft_pow2(&mut buf, false);
        for (b, f) in buf.iter_mut().zip(&self.filter) {
            *b = b.mul(*f);
        }
        fft_pow2(&mut buf, true);
        let s = 1.0 / self.m as f64;
        for k in 0..self.n {
            a[k] = Cx { re: buf[k].re * s, im: buf[k].im * s }.mul(self.chirp[k]);
        }
    }
}

/// `abs(np.fft.fft2(values))` of a `w x h` real grid, unshifted.
fn spectrum(values: &[f64], w: usize, h: usize) -> Vec<f64> {
    let mut a: Vec<Cx> = values.iter().map(|&re| Cx { re, im: 0.0 }).collect();
    let rows = Dft::new(w);
    for row in a.chunks_mut(w) {
        rows.run(row);
    }
    let cols = Dft::new(h);
    let mut col = vec![Cx::default(); h];
    for x in 0..w {
        for y in 0..h {
            col[y] = a[y * w + x];
        }
        cols.run(&mut col);
        for y in 0..h {
            a[y * w + x] = col[y];
        }
    }
    a.into_iter().map(|c| c.re.hypot(c.im)).collect()
}

/// `np.hanning(m)`.
fn hanning(m: usize) -> Vec<f64> {
    if m == 1 {
        return vec![1.0];
    }
    (0..m)
        .map(|i| {
            let n = (2 * i) as f64 + 1.0 - m as f64;
            0.5 + 0.5 * (std::f64::consts::PI * n / (m - 1) as f64).cos()
        })
        .collect()
}

/// `k` of `0..n` without replacement, in increasing order: a seeded partial
/// Fisher-Yates standing in for numpy's `default_rng(seed).choice`, whose
/// stream is not reproduced.
fn choose(n: usize, k: usize, seed: u64) -> Vec<usize> {
    let mut state = seed;
    let mut next = || {
        // SplitMix64
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    let mut idx: Vec<usize> = (0..n).collect();
    for i in 0..k.min(n) {
        let j = i + (next() % (n - i) as u64) as usize;
        idx.swap(i, j);
    }
    idx.truncate(k.min(n));
    idx.sort_unstable();
    idx
}

/// `scipy.optimize.minimize(method="Nelder-Mead")` in two dimensions with
/// box bounds, as scipy does it: the 5 % initial simplex reflected into the
/// box, every trial point clipped to it, standard coefficients, and the stop
/// on `xatol` and `fatol` together or at `maxiter`.
fn nelder_mead(f: impl Fn([f64; 2]) -> f64, x0: [f64; 2], lo: [f64; 2], hi: [f64; 2], xatol: f64, maxiter: usize) -> [f64; 2] {
    const FATOL: f64 = 1e-4;
    let clip = |p: [f64; 2]| [p[0].max(lo[0]).min(hi[0]), p[1].max(lo[1]).min(hi[1])];
    let x0 = clip(x0);
    let mut sim = [x0; 3];
    for k in 0..2 {
        let mut y = x0;
        y[k] = if y[k] != 0.0 { 1.05 * y[k] } else { 0.00025 };
        sim[k + 1] = y;
    }
    for s in &mut sim {
        for d in 0..2 {
            if s[d] > hi[d] {
                s[d] = 2.0 * hi[d] - s[d];
            }
        }
        *s = clip(*s);
    }
    let mut fs = [f(sim[0]), f(sim[1]), f(sim[2])];
    let sort = |sim: &mut [[f64; 2]; 3], fs: &mut [f64; 3]| {
        let mut order = [0, 1, 2];
        order.sort_by(|&a, &b| fs[a].total_cmp(&fs[b]));
        *sim = order.map(|i| sim[i]);
        *fs = order.map(|i| fs[i]);
    };
    sort(&mut sim, &mut fs);
    let mut iterations = 1;
    while iterations < maxiter {
        let xspread = (1..3).flat_map(|j| (0..2).map(move |d| (j, d))).map(|(j, d)| (sim[j][d] - sim[0][d]).abs()).fold(0.0, f64::max);
        let fspread = (1..3).map(|j| (fs[0] - fs[j]).abs()).fold(0.0, f64::max);
        if xspread <= xatol && fspread <= FATOL {
            break;
        }
        let xbar = [(sim[0][0] + sim[1][0]) / 2.0, (sim[0][1] + sim[1][1]) / 2.0];
        let along = |a: f64, b: f64| clip([a * xbar[0] + b * sim[2][0], a * xbar[1] + b * sim[2][1]]);
        let xr = along(2.0, -1.0);
        let fxr = f(xr);
        let mut shrink = false;
        if fxr < fs[0] {
            let xe = along(3.0, -2.0);
            let fxe = f(xe);
            (sim[2], fs[2]) = if fxe < fxr { (xe, fxe) } else { (xr, fxr) };
        } else if fxr < fs[1] {
            (sim[2], fs[2]) = (xr, fxr);
        } else if fxr < fs[2] {
            let xc = along(1.5, -0.5);
            let fxc = f(xc);
            if fxc <= fxr { (sim[2], fs[2]) = (xc, fxc) } else { shrink = true }
        } else {
            let xcc = along(0.5, 0.5);
            let fxcc = f(xcc);
            if fxcc < fs[2] { (sim[2], fs[2]) = (xcc, fxcc) } else { shrink = true }
        }
        if shrink {
            for j in 1..3 {
                sim[j] = clip([sim[0][0] + 0.5 * (sim[j][0] - sim[0][0]), sim[0][1] + 0.5 * (sim[j][1] - sim[0][1])]);
                fs[j] = f(sim[j]);
            }
        }
        iterations += 1;
        sort(&mut sim, &mut fs);
    }
    sim[0]
}

/// `_screen_frequencies`: the screen's two lattice frequencies (cycles per
/// px), or none. The 16 strongest peaks of the windowed spectrum of the
/// central 512 px (native resolution: scaling would alias the screen) above
/// 0.08 cycles; the strongest pair of similar length at a wide angle;
/// each refined by Nelder-Mead on the demodulated energy of up to 12000
/// known samples, within one bin.
fn screen_frequencies(high: &Grid, valid: &Mask) -> Vec<[f64; 2]> {
    let (h0, w0) = (high.h, high.w);
    let (y1, x1) = ((h0 / 2).saturating_sub(256), (w0 / 2).saturating_sub(256));
    let (hh, ww) = ((y1 + 512).min(h0) - y1, (x1 + 512).min(w0) - x1);
    let inside = |y: usize, x: usize| valid.data[(y1 + y) * w0 + x1 + x];
    let count = (0..hh * ww).filter(|&i| inside(i / ww, i % ww)).count();
    if hh.min(ww) < 24 || count < 256 {
        return Vec::new();
    }
    let (hy, hx) = (hanning(hh), hanning(ww));
    let values: Vec<f64> = (0..hh * ww)
        .map(|i| {
            let (y, x) = (i / ww, i % ww);
            let v = if inside(y, x) { high.data[(y1 + y) * w0 + x1 + x] } else { 0.0 };
            v * (hy[y] * hx[x])
        })
        .collect();
    let raw = spectrum(&values, ww, hh);
    let fy: Vec<f64> = (0..hh).map(|i| (i as isize - (hh / 2) as isize) as f64 * (1.0 / hh as f64)).collect();
    let fx: Vec<f64> = (0..ww).map(|i| (i as isize - (ww / 2) as isize) as f64 * (1.0 / ww as f64)).collect();
    let mut power: Vec<f64> = (0..hh * ww)
        .map(|i| {
            let (y, x) = (i / ww, i % ww);
            let v = raw[((y + hh - hh / 2) % hh) * ww + (x + ww - ww / 2) % ww];
            if fy[y] * fy[y] + fx[x] * fx[x] < 0.08 * 0.08 { 0.0 } else { v }
        })
        .collect();
    let mut peaks = Vec::new();
    for _ in 0..16 {
        let mut at = 0;
        for (i, &v) in power.iter().enumerate() {
            if v > power[at] {
                at = i;
            }
        }
        let (y, x) = (at / ww, at % ww);
        peaks.push(([fx[x], fy[y]], power[at]));
        for yy in y.saturating_sub(3)..(y + 4).min(hh) {
            for xx in x.saturating_sub(3)..(x + 4).min(ww) {
                power[yy * ww + xx] = 0.0;
            }
        }
    }
    let mut pair = None;
    let mut best = 0.0;
    for (i, &(a, pa)) in peaks.iter().enumerate() {
        for &(b, pb) in &peaks[i + 1..] {
            let (la, lb) = (a[0].hypot(a[1]), b[0].hypot(b[1]));
            let cross = (a[0] * b[1] - a[1] * b[0]).abs();
            if la.min(lb) < 0.1 || la.max(lb) > la.min(lb) * 1.5 || cross < la * lb * 0.7 {
                continue;
            }
            if pa.min(pb) > best {
                best = pa.min(pb);
                pair = Some([a, b]);
            }
        }
    }
    let Some(pair) = pair else { return Vec::new() };
    let mut at: Vec<usize> = (0..hh * ww).filter(|&i| inside(i / ww, i % ww)).collect();
    if at.len() > 12000 {
        at = choose(at.len(), 12000, 0).into_iter().map(|k| at[k]).collect();
    }
    let samples: Vec<(f64, f64, f64)> = at.iter().map(|&i| ((i % ww) as f64, (i / ww) as f64, values[i])).collect();
    let energy = |f: [f64; 2]| {
        let (mut c, mut s) = (0.0, 0.0);
        for &(x, y, v) in &samples {
            let phase = 2.0 * std::f64::consts::PI * (f[0] * x + f[1] * y);
            c += v * phase.cos();
            s += v * phase.sin();
        }
        -(c * c + s * s)
    };
    let (dx, dy) = (1.0 / ww as f64, 1.0 / hh as f64);
    pair.iter()
        .map(|&p| nelder_mead(energy, p, [p[0] - dx, p[1] - dy], [p[0] + dx, p[1] + dy], 1e-7, 120))
        .collect()
}

/// `_screen_evidence`: known pixels whose 5 px neighbourhood is a screen
/// with both lattice directions: high-pass power over 36, the two
/// frequencies' demodulated energy over 35 % of it, and each over 6 %.
fn screen_evidence(high: &Grid, known: &Mask, frequencies: &[[f64; 2]]) -> Mask {
    let (w, h) = (high.w, high.h);
    let valid = known.as_grid();
    let denominator = map(&blur(&valid, 5.0), |v| v.max(1e-6));
    let power = zip(&blur(&zip(&map(high, |v| v * v), &valid, |a, b| a * b), 5.0), &denominator, |a, b| a / b);
    let energies: Vec<Grid> = frequencies
        .iter()
        .map(|f| {
            let phase = |i: usize| 2.0 * std::f64::consts::PI * (f[0] * (i % w) as f64 + f[1] * (i / w) as f64);
            let part = |trig: fn(f64) -> f64| {
                let g = Grid { w, h, data: (0..w * h).map(|i| high.data[i] * valid.data[i] * trig(phase(i))).collect() };
                zip(&blur(&g, 5.0), &denominator, |a, b| a / b)
            };
            zip(&part(f64::cos), &part(f64::sin), |a, b| 2.0 * (a * a + b * b))
        })
        .collect();
    flags(w, h, |i| {
        let (p, e0, e1) = (power.data[i], energies[0].data[i], energies[1].data[i]);
        known.data[i] && p > 36.0 && e0 + e1 > p * 0.35 && e0.min(e1) > p * 0.06
    })
}

/// `cv2.connectedComponents(mask, connectivity=4)`: a label per pixel, 0 off
/// the mask.
fn label4(mask: &Mask) -> Vec<u32> {
    let (w, h) = (mask.w, mask.h);
    let mut label = vec![0u32; w * h];
    let mut next = 0;
    let mut stack = Vec::new();
    for start in 0..w * h {
        if !mask.data[start] || label[start] != 0 {
            continue;
        }
        next += 1;
        label[start] = next;
        stack.push(start);
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            let mut visit = |j: usize| {
                if mask.data[j] && label[j] == 0 {
                    label[j] = next;
                    stack.push(j);
                }
            };
            if x > 0 {
                visit(i - 1);
            }
            if x + 1 < w {
                visit(i + 1);
            }
            if y > 0 {
                visit(i - w);
            }
            if y + 1 < h {
                visit(i + w);
            }
        }
    }
    label
}

/// What `_screen_regions` finds: regions between contours, and each
/// region's screen mean, spread and confidence.
struct Regions {
    labels: Vec<u32>,
    mean: Vec<f64>,
    deviation: Vec<f64>,
    confidence: Vec<f64>,
    count: usize,
}

/// `_screen_regions`: the crop cut along the answer's contours and the
/// page's contours away from the mask (lettering is never a contour), grown
/// 4 px. In every region the mask reaches with 128 evidence pixels: the
/// evidence's local mean, std and the share of known pixels that are
/// evidence, as context fields over the region.
fn screen_regions(gray: &Grid, mask: &Mask, known: &Mask, evidence: &Mask, ink_gray: &Mask, ink_edited: &Mask) -> Regions {
    let (w, h) = (gray.w, gray.h);
    let source_known = mask.dilate(8).not();
    let barriers = ink_edited.or(&ink_gray.and(&source_known)).dilate(4);
    let labels = label4(&barriers.not());
    let mut ids: Vec<u32> = (0..w * h).filter(|&i| mask.data[i] && labels[i] != 0).map(|i| labels[i]).collect();
    ids.sort_unstable();
    ids.dedup();
    let mut out = Regions { mean: vec![0.0; w * h], deviation: vec![0.0; w * h], confidence: vec![0.0; w * h], count: 0, labels: Vec::new() };
    let squared = map(gray, |v| v * v);
    let evidence_grid = evidence.as_grid();
    for id in ids {
        let area = flags(w, h, |i| labels[i] == id);
        let samples = evidence.and(&area);
        if samples.count() < 128 {
            continue;
        }
        let probability = context_field(&[&evidence_grid], &known.and(&area), Some(&area)).remove(0);
        let moments = context_field(&[gray, &squared], &samples, Some(&area));
        for i in (0..w * h).filter(|&i| area.data[i]) {
            let m = moments[0].data[i];
            out.mean[i] = m;
            out.deviation[i] = (moments[1].data[i] - m * m).max(0.0).sqrt();
            out.confidence[i] = ((probability.data[i] - 0.65) / 0.25).clamp(0.0, 1.0);
        }
        out.count += 1;
    }
    out.labels = labels;
    out
}

/// `restore_periodic_screen`: continue the page's dot screen across the
/// mask with phase-aligned page samples, keeping the model's ink.
///
/// Repeat offsets within 128 px whose phase error over both frequencies is
/// under 0.0008 are tried cheapest first (160 at most), and kept where the
/// page's high-pass correlates 0.8 with itself across them. Each target (a
/// mask pixel with texture or flat grey, in a confident region with spread,
/// clear of the answer's contours) copies the page through the offset whose
/// source best matches its region, tone and spread, shifted by the tone
/// difference, and blends in by protection, confidence and spread.
fn restore_periodic_screen(reference: &[Grid], generated: &[Grid], mask: &Mask, page: &Mask) -> (Vec<Grid>, Value) {
    let declined = |reason: &str| (generated.to_vec(), json!({ "applied": false, "reason": reason }));
    let gray = grey(reference);
    let edited = grey(generated);
    let (w, h) = (gray.w, gray.h);
    let ink_gray = ink(&gray);
    let known = mask.dilate(2).not().and(&ink_gray.dilate(3).not()).and(page);
    let smooth = blur(&gray, 2.0);
    let high = zip(&gray, &smooth, |a, b| a - b);
    let frequencies = screen_frequencies(&high, &known);
    if frequencies.len() != 2 {
        return declined("No regular dot screen detected.");
    }
    let evidence = screen_evidence(&high, &known, &frequencies);
    if evidence.count() < 256 {
        return declined("No sufficiently reliable original screen found.");
    }
    let shifts = repeat_offsets(&high, &evidence, &frequencies);
    if shifts.is_empty() {
        return declined("Screen phase cannot be reproduced reliably.");
    }
    let ink_edited = ink(&edited);
    let regions = screen_regions(&gray, mask, &known, &evidence, &ink_gray, &ink_edited);
    let generated_high = zip(&edited, &blur(&edited, 2.0), |a, b| a - b);
    let texture = map(&blur(&map(&generated_high, |v| v * v), 4.0), f64::sqrt);
    let protection = structure_weight(&ink_edited);
    let mut result: Vec<Grid> = generated.to_vec();
    let mut count = 0;
    for t in 0..w * h {
        let e = edited.data[t];
        let textured = texture.data[t] > 5.0 || (e > 90.0 && e < 245.0);
        if !(mask.data[t] && textured && protection.data[t] > 0.0 && regions.confidence[t] > 0.0 && regions.deviation[t] > 8.0) {
            continue;
        }
        let (x, y) = ((t % w) as isize, (t / w) as isize);
        let mut best = f64::INFINITY;
        let mut sample = e;
        for &(xs, ys, error, correlation) in &shifts {
            let (cy, cx) = (y + ys, x + xs);
            let inside = cy >= 0 && cy < h as isize && cx >= 0 && cx < w as isize;
            let c = cy.clamp(0, h as isize - 1) as usize * w + cx.clamp(0, w as isize - 1) as usize;
            let available = inside && evidence.data[c] && regions.deviation[c] > 8.0 && regions.labels[c] == regions.labels[t];
            let difference = regions.mean[c] - regions.mean[t];
            let score = difference.abs() * 0.8
                + (regions.deviation[c] - regions.deviation[t]).abs() * 0.3
                + (xs as f64).hypot(ys as f64) * 0.02
                + error * 20000.0
                + (1.0 - correlation) * 10.0;
            if available && score < best {
                sample = gray.data[c] - difference;
                best = score;
            }
        }
        let strength = protection.data[t] * regions.confidence[t] * ((regions.deviation[t] - 6.0) / 10.0).clamp(0.0, 1.0);
        let delta = (sample - e) * strength;
        for channel in &mut result {
            channel.data[t] += delta;
        }
        count += usize::from(best.is_finite());
    }
    if count == 0 {
        return declined("No matching screen area detected in the target.");
    }
    let best_correlation = shifts.iter().map(|s| s.3).fold(f64::NEG_INFINITY, f64::max);
    let protected = (0..w * h).filter(|&i| mask.data[i] && protection.data[i] == 0.0).count();
    let details = json!({
        "applied": true,
        "pixels": count,
        "frequencies": frequencies.iter().map(|f| vec![f[0], f[1]]).collect::<Vec<_>>(),
        "best_correlation": best_correlation,
        "screen_regions": regions.count,
        "protected_pixels": protected,
    });
    (result.iter().map(image).collect(), details)
}

/// The repeat offsets `(dx, dy, phase error, correlation)` the screen is
/// copied through, cheapest first: up to 160 candidates whose phase lands
/// within `sqrt(0.0008)` cycles of a whole period on both frequencies, at
/// least 3 px out, tested on up to 3000 evidence pixels.
fn repeat_offsets(high: &Grid, evidence: &Mask, frequencies: &[[f64; 2]]) -> Vec<(isize, isize, f64, f64)> {
    let (w, h) = (high.w, high.h);
    let radius = 128.min(w.max(h) / 2) as isize;
    let mut candidates: Vec<(f64, isize, isize, f64)> = Vec::new();
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let mut error = 0.0;
            for f in frequencies {
                let phase = f[0] * dx as f64 + f[1] * dy as f64;
                error += (phase - phase.round_ties_even()).powi(2);
            }
            let distance = (dx as f64).hypot(dy as f64);
            if distance < 3.0 || error > 0.0008 {
                continue;
            }
            candidates.push((error * 100000.0 + distance * 0.08, dx, dy, error));
        }
    }
    // stable, so ties keep raster order (numpy's quicksort leaves them unordered)
    candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
    candidates.truncate(160);
    let mut at: Vec<usize> = (0..w * h).filter(|&i| evidence.data[i]).collect();
    if at.len() > 3000 {
        at = choose(at.len(), 3000, 1).into_iter().map(|k| at[k]).collect();
    }
    let mut shifts = Vec::new();
    for &(_, dx, dy, error) in &candidates {
        let mut a = Vec::new();
        let mut b = Vec::new();
        for &i in &at {
            let (cx, cy) = ((i % w) as isize + dx, (i / w) as isize + dy);
            if cx < 0 || cy < 0 || cx >= w as isize || cy >= h as isize {
                continue;
            }
            let c = cy as usize * w + cx as usize;
            if evidence.data[c] {
                a.push(high.data[i]);
                b.push(high.data[c]);
            }
        }
        if a.len() < 128 {
            continue;
        }
        let n = a.len() as f64;
        let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
        let (mut saa, mut sbb, mut sab) = (0.0, 0.0, 0.0);
        for (&u, &v) in a.iter().zip(&b) {
            saa += (u - ma) * (u - ma);
            sbb += (v - mb) * (v - mb);
            sab += (u - ma) * (v - mb);
        }
        if (saa / n).sqrt().min((sbb / n).sqrt()) < 4.0 {
            continue;
        }
        let correlation = (sab / (saa.sqrt() * sbb.sqrt())).clamp(-1.0, 1.0);
        if correlation >= 0.8 {
            shifts.push((dx, dy, error, correlation));
        }
    }
    shifts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finish_one(shown: &[Grid], answer: &[Grid], mask: &Mask, method: Method, grayscale: bool) -> (Vec<Grid>, Value) {
        let page = Mask::new(mask.w, mask.h, true);
        finish(&Crop { shown, answer, mask, page: &page }, method, grayscale)
    }

    fn disc(w: usize, h: usize, r: f64) -> Mask {
        flags(w, h, |i| {
            let (x, y) = ((i % w) as f64 - w as f64 / 2.0, (i / w) as f64 - h as f64 / 2.0);
            x.hypot(y) < r
        })
    }

    #[test]
    fn grey_is_pil_luma() {
        let (w, h) = (3, 1);
        let rgb = [
            Grid { w, h, data: vec![255.0, 0.0, 0.0] },
            Grid { w, h, data: vec![0.0, 255.0, 0.0] },
            Grid { w, h, data: vec![0.0, 0.0, 255.0] },
        ];
        // PIL's convert("L") of pure red, green and blue
        assert_eq!(grey(&rgb).data, vec![76.0, 150.0, 29.0]);
    }

    #[test]
    fn off_with_grayscale_writes_the_grey_answer_to_every_channel() {
        let (w, h) = (40, 30);
        let channel = |k: usize| Grid { w, h, data: (0..w * h).map(|i| ((i * (7 + k) + 13 * k) % 256) as f64).collect() };
        let answer = [channel(0), channel(1), channel(2)];
        let mask = disc(w, h, 8.0);
        let (out, details) = finish_one(&answer, &answer, &mask, Method::Off, true);
        let want = grey(&answer);
        assert!(out.len() == 3 && out.iter().all(|c| *c == want));
        assert_eq!(details["tone_match"], json!(false));
    }

    #[test]
    fn global_match_undoes_a_gain_and_offset() {
        let (w, h) = (96, 96);
        // a textured page, and an answer 10 % flat and 12 levels light
        let shown = Grid { w, h, data: (0..w * h).map(|i| 60.0 + ((i % w) * 3 + (i / w) * 5 % 97) as f64 % 150.0).collect() };
        let answer = map(&shown, |v| (v * 0.9 + 12.0).round());
        let mask = disc(w, h, 20.0);
        let (out, details) = finish_one(std::slice::from_ref(&shown), &[answer], &mask, Method::Global, false);
        let gain = details["global"][0]["gain"].as_f64().unwrap();
        assert!((gain - 1.0 / 0.9).abs() < 0.01, "gain {gain}");
        let worst = (0..w * h).filter(|&i| mask.data[i]).map(|i| (out[0].data[i] - shown.data[i]).abs()).fold(0.0, f64::max);
        assert!(worst <= 1.0, "worst {worst}");
    }

    #[test]
    fn raster_continues_a_dot_screen_in_phase() {
        let (w, h) = (192, 192);
        let f = 0.17;
        let screen = |x: f64, y: f64| {
            let t = std::f64::consts::TAU;
            150.0 + 45.0 * ((t * f * (x + y)).cos() + (t * f * (x - y)).cos())
        };
        let shown = Grid { w, h, data: (0..w * h).map(|i| screen((i % w) as f64, (i / w) as f64).round()).collect() };
        let mask = disc(w, h, 30.0);
        // the model filled the hole with the screen's flat average
        let answer = shown.select(&mask.not(), &Grid::new(w, h, 150.0));
        let (out, details) = finish_one(std::slice::from_ref(&shown), std::slice::from_ref(&answer), &mask, Method::Raster, true);
        assert_eq!(details["raster"]["applied"], json!(true), "{details}");
        for pair in details["raster"]["frequencies"].as_array().unwrap() {
            let (fx, fy) = (pair[0].as_f64().unwrap().abs(), pair[1].as_f64().unwrap().abs());
            assert!((fx - f).abs() < 1e-3 && (fy - f).abs() < 1e-3, "{pair}");
        }
        // well inside the hole the screen is back, in phase
        let inner = disc(w, h, 20.0);
        let error = |g: &Grid| (0..w * h).filter(|&i| inner.data[i]).map(|i| (g.data[i] - shown.data[i]).abs()).sum::<f64>() / inner.count() as f64;
        assert!(error(&out[0]) < 0.2 * error(&answer), "{} vs {}", error(&out[0]), error(&answer));
    }

    #[test]
    fn dirichlet_solve_rebuilds_a_harmonic_surface() {
        // u = x * y is harmonic: with its edge moved into b, the solve returns it
        let (n, m) = (13, 9);
        let u = |x: usize, y: usize| (x * y) as f64;
        let mut b = vec![0.0; n * m];
        for j in 0..m {
            for i in 0..n {
                let (x, y) = (i + 1, j + 1);
                let mut v = 0.0;
                if x == 1 {
                    v -= u(0, y);
                }
                if x == n {
                    v -= u(n + 1, y);
                }
                if y == 1 {
                    v -= u(x, 0);
                }
                if y == m {
                    v -= u(x, m + 1);
                }
                b[j * n + i] = v;
            }
        }
        let got = solve_dirichlet(&b, n, m);
        let worst = (0..n * m).map(|k| (got[k] - u(k % n + 1, k / n + 1)).abs()).fold(0.0, f64::max);
        assert!(worst < 1e-9, "worst {worst}");
    }
}
