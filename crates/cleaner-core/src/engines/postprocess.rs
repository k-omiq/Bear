//! Matching an edit model's answer to the page before it is written:
//! preprocessing `3.0.0`, the FLUX 3 edit recipe's composite.
//!
//! An edit model returns the whole crop redrawn. Inside the lettering it is
//! right about the drawing and often a little off in tone; its screentone is
//! a screen of its own, not the page's. These deterministic steps run on the
//! answer at the page's own resolution, per tone channel, after it is read in
//! the page's mode (a neutral page reads it as one grey channel, so the patch
//! stays grey: [`super::model::answer_mode`]), and before it is written
//! through the applied mask, which leaves every pixel the mask does not reach
//! exactly the page's ([`super::render::PreparedRender::blend`]).
//!
//! 1. **Contours.** Canny edges where the gradient structure tensor is
//!    oriented: drawn lines count, isotropic screen dots do not. Inside the
//!    mask they come from the answer, which drew them, found under a
//!    [`DESCREEN_SIGMA`] px blur so a model's moire does not read as lines;
//!    outside it from the page. Every correction below fades to nothing within [`PROTECT_PX`] of
//!    one, and a contour is a barrier the screen step does not copy across.
//! 2. **Tone.** A gain ([`GAIN`]) and offset ([`MAX_OFFSET`]) fitted on the
//!    low-passed page around the mask, then the residual carried in by
//!    normalised Gaussians, `blur(valid * difference) / blur(valid)`, coarse
//!    scale to fine ([`LOCAL_SIGMAS`]). The evidence is page near the mask,
//!    clear of lines, where the answer still shows the same page: a low-pass
//!    difference past [`DRIFT_MAX`] is the model having changed the drawing
//!    there, and a black stroke it erased would otherwise grey the patch.
//! 3. **Lighting.** The remaining low-frequency difference at the mask's
//!    edge, solved across the mask as a membrane (Laplace) over the page's
//!    context band, and added inside the mask only.
//! 4. **Screen** (grey pages): where a background region next to the mask is
//!    a regular dot screen, the screen is redrawn across the region's mask
//!    pixels in phase with the page's, at the answer's tone
//!    ([`super::halftone::redraw`]). Anywhere the evidence is not there keeps
//!    the answer.
//!
//! [`Finishing`] picks the version: every screen step the bench measured, the
//! steps without the screen, or the reference tool's own finishing
//! ([`super::finishing`]) in place of all four. The default is bench 7's.
//!
//! Prototyped in Python on the FLUX 3 bench (docs/research/flux3-klein-5_6b.md,
//! bench 3) and measured against synthetic lettering over real pages.

use serde::{Deserialize, Serialize};

use super::grid::{Grid, Mask, distance_to, gather, gaussian, masked_blur, median, percentile};
use super::poisson::membrane;

/// The gain the tone step may apply.
pub const GAIN: (f64, f64) = (0.85, 1.15);
/// The offset the tone step may apply, in 8-bit levels.
pub const MAX_OFFSET: f64 = 32.0;
/// The local tone correction's scales, coarse to fine, in px.
pub const LOCAL_SIGMAS: [f64; 3] = [48.0, 16.0, 6.0];
/// The local tone correction never moves a pixel further than this.
pub const LOCAL_CAP: f64 = 32.0;
/// A low-pass difference past this between page and answer outside the mask
/// is a change of drawing, not of tone, and is no evidence.
pub const DRIFT_MAX: f64 = 24.0;
/// Corrections fade to nothing within this many px of a contour.
pub const PROTECT_PX: f64 = 3.0;
/// The blur the answer's contours are found under, in px.
pub const DESCREEN_SIGMA: f64 = 1.5;
/// The repeat correlation [`Finishing::RepeatOffsets`] copies a screen at.
pub const REPEAT_CORR: f64 = 0.8;

/// The low-pass the tone step compares page and answer at, in px.
const TONE_SIGMA: f64 = 2.5;
/// The low-pass the lighting step solves, in px, and the most it adds.
const LIGHTING_SIGMA: f64 = 2.0;
const LIGHTING_CAP: f64 = 48.0;
/// Tone evidence lies within this many px of the mask, and further than
/// [`LINE_CLEARANCE`] px from a contour, where the low-pass mixes the line in.
const EVIDENCE_REACH: f64 = 64.0;
const LINE_CLEARANCE: f64 = 5.0;
const MIN_EVIDENCE: usize = 64;
/// A gain is fitted only over this tonal range (5th to 95th percentile);
/// paper alone fixes an offset and nothing else.
const MIN_GAIN_RANGE: f64 = 48.0;
/// A normalised blur takes over from the coarser scale where this much of its
/// window is evidence.
const FULL_SUPPORT: f64 = 0.25;

/// Canny thresholds on the 3 x 3 Sobel magnitude, and the structure tensor's
/// window and the coherence that makes an edge a line.
const CANNY_LOW: f64 = 30.0;
const CANNY_HIGH: f64 = 90.0;
const TENSOR_SIGMA: f64 = 2.0;
const MIN_COHERENCE: f64 = 0.35;

/// The finishing an answer gets before it is written. The first six are this
/// module's steps with one screen step or none; the `Reference` ones are the
/// reference tool's finishing v2 ([`super::finishing::Method`]) instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Finishing {
    /// The page's own dots copied in phase, the table where no copy reaches
    /// (bench 7).
    #[default]
    Copy,
    /// The copy, then hops from copied pixels deeper into a wide mask.
    CopyHops,
    /// The table redraw alone, the answer's contours found under the
    /// descreen blur.
    Redraw,
    /// The table redraw with the answer's contours found sharp (bench 5).
    RedrawSharpContours,
    /// Repeat offsets from the page's autocorrelation and an onion copy,
    /// contours found sharp (bench 3b, [`super::screentone::restore`]).
    RepeatOffsets,
    /// Contours, tone and lighting; no screen step.
    ToneOnly,
    ReferenceOff,
    ReferenceGlobal,
    ReferenceLocal,
    ReferencePoisson,
    ReferenceRaster,
}

impl Finishing {
    pub const ALL: [Finishing; 11] = [
        Finishing::Copy,
        Finishing::CopyHops,
        Finishing::Redraw,
        Finishing::RedrawSharpContours,
        Finishing::RepeatOffsets,
        Finishing::ToneOnly,
        Finishing::ReferenceOff,
        Finishing::ReferenceGlobal,
        Finishing::ReferenceLocal,
        Finishing::ReferencePoisson,
        Finishing::ReferenceRaster,
    ];

    /// The reference tool's method, for the `Reference` versions.
    pub fn reference(self) -> Option<super::finishing::Method> {
        use super::finishing::Method;
        match self {
            Finishing::ReferenceOff => Some(Method::Off),
            Finishing::ReferenceGlobal => Some(Method::Global),
            Finishing::ReferenceLocal => Some(Method::Local),
            Finishing::ReferencePoisson => Some(Method::Poisson),
            Finishing::ReferenceRaster => Some(Method::Raster),
            _ => None,
        }
    }
}

/// A finishing as the `cloudFinishing` settings and a cloud recipe keep it:
/// the version, and whether a colour answer is made grey first (the
/// reference tool's `force_grayscale`; a grey page's answer is grey already).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Choice {
    pub finishing: Finishing,
    #[serde(default)]
    pub grayscale: bool,
}

/// What the post-processing did, for the patch's provenance
/// (`params_snapshot["postprocess"]`, [`Report::provenance`]).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    /// Per tone channel: the fitted gain and offset, or none where there was
    /// too little evidence.
    pub gain: Vec<Option<f64>>,
    pub offset: Vec<Option<f64>>,
    /// Page pixels the tone step measured on.
    pub evidence: usize,
    /// Protected contour pixels inside the mask.
    pub contours: usize,
    /// Mask pixels the screen step copied the page's dots into.
    pub screen: usize,
    /// The version that ran, and whether it made the patch grey.
    #[serde(flatten)]
    pub choice: Choice,
    /// The reference finishing's own report, for the `Reference` versions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<serde_json::Value>,
}

/// The crop as the model was shown it and as it answered, one grid per tone
/// channel in 8-bit levels, and where they may be compared and written.
pub struct Crop<'a> {
    pub shown: &'a [Grid],
    pub answer: &'a [Grid],
    /// Where the answer is written: the applied mask at non-zero weight.
    pub mask: &'a Mask,
    /// Page pixels, fully opaque: the rest of the crop is edge replication or
    /// a colour nobody sees, and is no evidence.
    pub page: &'a Mask,
}

/// The answer, matched to the page by the default finishing.
pub fn settle(crop: &Crop<'_>) -> (Vec<Grid>, Report) {
    settle_with(crop, Choice::default())
}

/// The answer, matched to the page: one grid per tone channel. Outside
/// `mask` it is the answer unchanged; the caller writes only inside it.
pub fn settle_with(crop: &Crop<'_>, choice: Choice) -> (Vec<Grid>, Report) {
    // The tone fit and the reference finishing work on the whole crop; only
    // the mask keeps what they made.
    let (finished, report) = finish(crop, choice);
    let kept = finished.iter().zip(crop.answer).map(|(finished, answer)| finished.select(crop.mask, answer)).collect();
    (kept, report)
}

/// [`settle_with`] over the whole crop.
fn finish(crop: &Crop<'_>, choice: Choice) -> (Vec<Grid>, Report) {
    let Choice { finishing, grayscale } = choice;
    let mask = crop.mask;
    let (w, h) = (mask.w, mask.h);
    let mut report = Report { gain: Vec::new(), offset: Vec::new(), evidence: 0, contours: 0, screen: 0, choice, reference: None };
    if let Some(method) = finishing.reference() {
        let (finished, details) = super::finishing::finish(crop, method, grayscale);
        report.reference = Some(details);
        return (finished, report);
    }
    // A colour crop made grey is finished as one grey channel, then written
    // to every channel.
    let channels = crop.answer.len();
    let grey;
    let crop = if grayscale && channels > 1 {
        grey = ([super::finishing::grey(crop.shown)], [super::finishing::grey(crop.answer)]);
        &Crop { shown: &grey.0, answer: &grey.1, mask: crop.mask, page: crop.page }
    } else {
        crop
    };
    // The answer's lines are found under a blur that takes out a screen's
    // dots and a model's moire at their scale: oriented stripes would
    // otherwise read as drawing and hold every step off the patch. Benches 3b
    // and 5 found them sharp.
    let sharp = matches!(finishing, Finishing::RedrawSharpContours | Finishing::RepeatOffsets);
    let drawn = if sharp { luma(crop.answer) } else { gaussian(&luma(crop.answer), DESCREEN_SIGMA) };
    let protected = contours(&drawn).and(mask).or(&contours(&luma(crop.shown)).and_not(mask)).dilate(1);
    let fade = fade(&distance_to(&protected));
    let outside = mask.not().and(crop.page);
    let from_mask = distance_to(mask);
    let clear = distance_to(&protected);
    let near = Mask {
        w,
        h,
        data: (0..w * h).map(|i| outside.data[i] && clear.data[i] > LINE_CLEARANCE && from_mask.data[i] <= EVIDENCE_REACH).collect(),
    };
    // The lighting step's edge is page, not drawing: a line crossing it is no
    // evidence of light.
    let edge = outside.and_not(&protected.dilate(2 * LIGHTING_SIGMA as usize));

    report.contours = protected.and(mask).count();
    let mut settled = Vec::with_capacity(crop.answer.len());
    for (shown, answer) in crop.shown.iter().zip(crop.answer) {
        let (toned, fit, evidence) = tone(shown, answer, &near, &fade);
        report.gain.push(fit.map(|f| f.0));
        report.offset.push(fit.map(|f| f.1));
        report.evidence = report.evidence.max(evidence);
        settled.push(lighting(shown, &toned, mask, &edge, &fade));
    }
    if let ([shown], [answer]) = (crop.shown, settled.as_mut_slice()) {
        use super::halftone::{Reach, redraw};
        let screen = match finishing {
            Finishing::Copy => redraw(shown, answer, mask, &protected, crop.page, &fade, Reach::Copy),
            Finishing::CopyHops => redraw(shown, answer, mask, &protected, crop.page, &fade, Reach::Hops),
            Finishing::Redraw | Finishing::RedrawSharpContours => redraw(shown, answer, mask, &protected, crop.page, &fade, Reach::Table),
            // Restored pixels fade near a contour like every other correction.
            Finishing::RepeatOffsets => super::screentone::restore(shown, answer, mask, &protected, crop.page, REPEAT_CORR).map(|(copy, strength)| {
                let weight = Grid { w, h, data: (0..w * h).map(|i| strength.data[i] * fade.data[i]).collect() };
                (copy, weight)
            }),
            _ => None,
        };
        if let Some((screen, weight)) = screen {
            for i in 0..w * h {
                let s = weight.data[i];
                if s > 0.0 {
                    answer.data[i] = answer.data[i] * (1.0 - s) + screen.data[i] * s;
                    report.screen += 1;
                }
            }
        }
    }
    for grid in &mut settled {
        grid.data.iter_mut().for_each(|v| *v = v.clamp(0.0, 255.0));
    }
    if settled.len() < channels {
        settled = vec![settled[0].clone(); channels];
    }
    (settled, report)
}

impl Report {
    /// The record a patch's provenance keeps, with the version of this
    /// post-processing that made it. A Qwen (2.0.0) or LaMa patch finished
    /// here carries it too, so it names the post-processing and not the
    /// crop's preprocessing.
    pub fn provenance(&self) -> serde_json::Value {
        let mut record = serde_json::json!(self);
        record["postprocess_version"] = serde_json::json!(super::render::EDIT_PREPROCESSING_VERSION);
        record
    }
}

fn luma(channels: &[Grid]) -> Grid {
    let n = channels.len().max(1) as f64;
    let (w, h) = (channels[0].w, channels[0].h);
    Grid { w, h, data: (0..w * h).map(|i| channels.iter().map(|c| c.data[i]).sum::<f64>() / n).collect() }
}

fn fade(distance: &Grid) -> Grid {
    Grid { w: distance.w, h: distance.h, data: distance.data.iter().map(|d| (d / PROTECT_PX).clamp(0.0, 1.0)).collect() }
}

/// Step 2 over one channel: the answer with the global fit and the local
/// correction applied, the fit, and how many pixels it was measured on.
fn tone(shown: &Grid, answer: &Grid, near: &Mask, fade: &Grid) -> (Grid, Option<(f64, f64)>, usize) {
    let (lo_shown, lo_answer) = (gaussian(shown, TONE_SIGMA), gaussian(answer, TONE_SIGMA));
    let n = answer.data.len();
    let valid = Mask { w: near.w, h: near.h, data: (0..n).map(|i| near.data[i] && (lo_shown.data[i] - lo_answer.data[i]).abs() <= 2.0 * DRIFT_MAX).collect() };
    let count = valid.count();
    if count < MIN_EVIDENCE {
        return (answer.clone(), None, count);
    }
    let (x, y) = (gather(&lo_answer, &valid), gather(&lo_shown, &valid));
    let gain = {
        let mut sorted = x.clone();
        if percentile(&mut sorted, 95.0) - percentile(&mut sorted, 5.0) >= MIN_GAIN_RANGE {
            let (mx, my) = (x.iter().sum::<f64>() / count as f64, y.iter().sum::<f64>() / count as f64);
            let cov: f64 = x.iter().zip(&y).map(|(a, b)| (a - mx) * (b - my)).sum();
            let var: f64 = x.iter().map(|a| (a - mx) * (a - mx)).sum();
            (cov / var.max(1e-9)).clamp(GAIN.0, GAIN.1)
        } else {
            1.0
        }
    };
    let mut gaps: Vec<f64> = x.iter().zip(&y).map(|(a, b)| b - gain * a).collect();
    let offset = median(&mut gaps).clamp(-MAX_OFFSET, MAX_OFFSET);
    let mut out = Grid { w: answer.w, h: answer.h, data: answer.data.iter().map(|v| v * gain + offset).collect() };

    let lo_out = gaussian(&out, TONE_SIGMA);
    let residual = Grid { w: out.w, h: out.h, data: (0..n).map(|i| lo_shown.data[i] - lo_out.data[i]).collect() };
    let valid = Mask { w: valid.w, h: valid.h, data: (0..n).map(|i| valid.data[i] && residual.data[i].abs() <= DRIFT_MAX).collect() };
    let mut correction = vec![0.0; n];
    for sigma in LOCAL_SIGMAS {
        let (estimate, support) = masked_blur(&residual, &valid, sigma);
        for i in 0..n {
            let weight = (support.data[i] / FULL_SUPPORT).clamp(0.0, 1.0);
            correction[i] = correction[i] * (1.0 - weight) + estimate.data[i] * weight;
        }
    }
    for i in 0..n {
        out.data[i] += correction[i].clamp(-LOCAL_CAP, LOCAL_CAP) * fade.data[i];
    }
    (out, Some((gain, offset)), count)
}

/// Step 3 over one channel: the low-pass difference on `edge`, carried across
/// the mask as a membrane and added inside it.
fn lighting(shown: &Grid, toned: &Grid, mask: &Mask, edge: &Mask, fade: &Grid) -> Grid {
    if !edge.any() || !mask.any() {
        return toned.clone();
    }
    let (lo_shown, lo_toned) = (gaussian(shown, LIGHTING_SIGMA), gaussian(toned, LIGHTING_SIGMA));
    let n = toned.data.len();
    // Where the edge's difference is a change of drawing, the edge holds the
    // field at zero rather than spreading that change into the mask.
    let diff = Grid {
        w: toned.w,
        h: toned.h,
        data: (0..n)
            .map(|i| {
                let d = lo_shown.data[i] - lo_toned.data[i];
                if edge.data[i] && d.abs() <= DRIFT_MAX { d } else { 0.0 }
            })
            .collect(),
    };
    let field = membrane(&diff, edge);
    Grid {
        w: toned.w,
        h: toned.h,
        data: (0..n)
            .map(|i| {
                if mask.data[i] {
                    toned.data[i] + field.data[i].clamp(-LIGHTING_CAP, LIGHTING_CAP) * fade.data[i]
                } else {
                    toned.data[i]
                }
            })
            .collect(),
    }
}

/// Step 1: the drawing's lines in `g`. Canny edges (3 x 3 Sobel, L2
/// magnitude, hysteresis) kept where the structure tensor is coherent.
pub fn contours(g: &Grid) -> Mask {
    let (gx, gy) = sobel(g);
    let (w, h) = (g.w, g.h);
    let n = w * h;
    let magnitude: Vec<f64> = (0..n).map(|i| gx.data[i].hypot(gy.data[i])).collect();
    let product = |a: &Grid, b: &Grid| gaussian(&Grid { w, h, data: (0..n).map(|i| a.data[i] * b.data[i]).collect() }, TENSOR_SIGMA);
    let (jxx, jyy, jxy) = (product(&gx, &gx), product(&gy, &gy), product(&gx, &gy));
    let coherent: Vec<bool> = (0..n)
        .map(|i| {
            let (a, b, c) = (jxx.data[i], jyy.data[i], jxy.data[i]);
            let half = (((a - b) / 2.0).powi(2) + c * c).sqrt();
            let (l1, l2) = ((a + b) / 2.0 + half, (a + b) / 2.0 - half);
            ((l1 - l2) / (l1 + l2 + 1e-9)).powi(2) > MIN_COHERENCE
        })
        .collect();
    let edges = canny(&magnitude, &gx, &gy, w, h);
    Mask { w, h, data: (0..n).map(|i| edges[i] && coherent[i]).collect() }
}

/// 3 x 3 Sobel derivatives, borders replicated.
fn sobel(g: &Grid) -> (Grid, Grid) {
    let (w, h) = (g.w, g.h);
    let at = |x: isize, y: isize| g.data[y.clamp(0, h as isize - 1) as usize * w + x.clamp(0, w as isize - 1) as usize];
    let mut gx = Grid::new(w, h, 0.0);
    let mut gy = Grid::new(w, h, 0.0);
    for y in 0..h as isize {
        for x in 0..w as isize {
            let i = y as usize * w + x as usize;
            gx.data[i] = (at(x + 1, y - 1) + 2.0 * at(x + 1, y) + at(x + 1, y + 1)) - (at(x - 1, y - 1) + 2.0 * at(x - 1, y) + at(x - 1, y + 1));
            gy.data[i] = (at(x - 1, y + 1) + 2.0 * at(x, y + 1) + at(x + 1, y + 1)) - (at(x - 1, y - 1) + 2.0 * at(x, y - 1) + at(x + 1, y - 1));
        }
    }
    (gx, gy)
}

/// Non-maximum suppression along the gradient, then hysteresis: a weak edge
/// pixel survives only when 8-connected to a strong one.
fn canny(magnitude: &[f64], gx: &Grid, gy: &Grid, w: usize, h: usize) -> Vec<bool> {
    let mut thin = vec![false; w * h];
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let i = y * w + x;
            let m = magnitude[i];
            if m <= CANNY_LOW {
                continue;
            }
            let angle = gy.data[i].atan2(gx.data[i]).to_degrees().rem_euclid(180.0);
            let (a, b) = if !(22.5..157.5).contains(&angle) {
                (i - 1, i + 1)
            } else if angle < 67.5 {
                (i - w - 1, i + w + 1)
            } else if angle < 112.5 {
                (i - w, i + w)
            } else {
                (i - w + 1, i + w - 1)
            };
            thin[i] = m > magnitude[a] && m >= magnitude[b];
        }
    }
    let mut keep = vec![false; w * h];
    let mut stack: Vec<usize> = (0..w * h).filter(|&i| thin[i] && magnitude[i] > CANNY_HIGH).collect();
    for &i in &stack {
        keep[i] = true;
    }
    while let Some(i) = stack.pop() {
        let (x, y) = ((i % w) as isize, (i / w) as isize);
        for dy in -1..=1 {
            for dx in -1..=1 {
                let (nx, ny) = (x + dx, y + dy);
                if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                    continue;
                }
                let j = ny as usize * w + nx as usize;
                if thin[j] && !keep[j] {
                    keep[j] = true;
                    stack.push(j);
                }
            }
        }
    }
    keep
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(w: usize, h: usize, x0: usize, y0: usize, x1: usize, y1: usize) -> Mask {
        Mask { w, h, data: (0..w * h).map(|i| (x0..x1).contains(&(i % w)) && (y0..y1).contains(&(i / w))).collect() }
    }

    fn settle_one(shown: &Grid, answer: &Grid, mask: &Mask) -> (Grid, Report) {
        let page = Mask::new(shown.w, shown.h, true);
        let (mut out, report) = settle(&Crop { shown: std::slice::from_ref(shown), answer: std::slice::from_ref(answer), mask, page: &page });
        (out.remove(0), report)
    }

    #[test]
    fn a_line_is_a_contour_and_screen_dots_are_not() {
        let (w, h) = (96, 96);
        let mut g = Grid { w, h, data: (0..w * h).map(|i| if (i % w) % 4 < 2 && (i / w) % 4 < 2 { 90.0 } else { 200.0 }).collect() };
        for y in 0..h {
            for x in 46..50 {
                g.data[y * w + x] = 0.0;
            }
        }
        let lines = contours(&g);
        assert!((10..86).all(|y| (44..52).any(|x| lines.at(x, y))), "the line is found");
        let dots = (0..h).flat_map(|y| (0..36).map(move |x| (x, y))).filter(|&(x, y)| lines.at(x, y)).count();
        assert!(dots < 20, "{dots} dot pixels taken for a line");
    }

    #[test]
    fn a_drifted_answer_is_brought_back_to_the_page() {
        let (w, h) = (128, 128);
        let shown = Grid { w, h, data: (0..w * h).map(|i| 120.0 + (i % w) as f64 * 0.5).collect() };
        // The model returned the same gradient 20 levels light and slightly contrasty.
        let answer = Grid { w, h, data: shown.data.iter().map(|v| v * 1.05 + 14.0).collect() };
        let mask = square(w, h, 40, 40, 88, 88);
        let (out, report) = settle_one(&shown, &answer, &mask);
        let worst = (0..w * h).filter(|&i| mask.data[i]).map(|i| (out.data[i] - shown.data[i]).abs()).fold(0.0, f64::max);
        assert!(worst < 2.0, "worst {worst}, {report:?}");
        assert!(report.gain[0].is_some());
    }

    #[test]
    fn a_stroke_the_model_erased_outside_the_mask_does_not_grey_the_patch() {
        let (w, h) = (128, 128);
        // Paper with a black bar beside the mask; the model answered all paper.
        let mut shown = Grid::new(w, h, 250.0);
        for y in 0..h {
            for x in 90..110 {
                shown.data[y * w + x] = 0.0;
            }
        }
        let answer = Grid::new(w, h, 250.0);
        let mask = square(w, h, 50, 40, 86, 88);
        let (out, _) = settle_one(&shown, &answer, &mask);
        let darkest = (0..w * h).filter(|&i| mask.data[i]).map(|i| out.data[i]).fold(255.0, f64::min);
        assert!(darkest > 244.0, "darkest {darkest}");
    }

    #[test]
    fn a_flat_patch_on_a_dot_screen_gets_the_pages_dots_back_in_phase() {
        let (w, h) = (160, 160);
        let period = std::f64::consts::TAU / 6.0;
        let page = Grid {
            w,
            h,
            data: (0..w * h).map(|i| 128.0 + 80.0 * ((i % w) as f64 * period).cos() * ((i / w) as f64 * period).cos()).collect(),
        };
        let mask = square(w, h, 50, 50, 110, 110);
        // The model kept the tone and lost the dots.
        let answer = page.select(&mask.not(), &Grid::new(w, h, 128.0));
        let (out, report) = settle_one(&page, &answer, &mask);
        let core = square(w, h, 56, 56, 104, 104);
        let error = |g: &Grid| (0..w * h).filter(|&i| core.data[i]).map(|i| (g.data[i] - page.data[i]).abs()).sum::<f64>() / core.count() as f64;
        assert!(report.screen > 0, "{report:?}");
        assert!(error(&out) < 0.2 * error(&answer), "{} against {}", error(&out), error(&answer));
    }

    #[test]
    fn flat_paper_and_no_evidence_leave_the_answer() {
        let (w, h) = (64, 64);
        let shown = Grid::new(w, h, 255.0);
        let answer = Grid::new(w, h, 250.0);
        let all = Mask::new(w, h, true);
        let (out, report) = settle_one(&shown, &answer, &all);
        assert_eq!(out, answer, "no page outside the mask: nothing to match");
        assert_eq!(report.gain, vec![None]);
    }

    /// A dot screen with a square hole the model filled with the screen's
    /// flat tone: the page, the answer and the mask.
    fn flat_hole_on_a_screen(mask: Mask) -> (Grid, Grid, Mask) {
        let (w, h) = (mask.w, mask.h);
        let period = std::f64::consts::TAU / 6.0;
        let page = Grid {
            w,
            h,
            data: (0..w * h).map(|i| 128.0 + 80.0 * ((i % w) as f64 * period).cos() * ((i / w) as f64 * period).cos()).collect(),
        };
        let answer = page.select(&mask.not(), &Grid::new(w, h, 128.0));
        (page, answer, mask)
    }

    fn settle_as(shown: &[Grid], answer: &[Grid], mask: &Mask, page: &Mask, finishing: Finishing, grayscale: bool) -> (Vec<Grid>, Report) {
        settle_with(&Crop { shown, answer, mask, page }, Choice { finishing, grayscale })
    }

    /// **Every finishing writes only the mask and says which one ran.**
    /// Outside the mask the answer comes back as it was, the reference ones
    /// included, and a reference finishing reports its method.
    #[test]
    fn every_finishing_keeps_the_answer_outside_the_mask() {
        let (w, h) = (160, 160);
        let (page, answer, mask) = flat_hole_on_a_screen(square(w, h, 50, 50, 110, 110));
        let all = Mask::new(w, h, true);
        for finishing in Finishing::ALL {
            let (out, report) = settle_as(std::slice::from_ref(&page), std::slice::from_ref(&answer), &mask, &all, finishing, false);
            assert!((0..w * h).all(|i| mask.data[i] || out[0].data[i] == answer.data[i]), "{finishing:?} wrote outside the mask");
            assert_eq!(report.choice.finishing, finishing);
            let name = serde_json::json!(finishing);
            let method = report.reference.as_ref().map(|details| details["method"].clone());
            assert_eq!(method, name.as_str().and_then(|n| n.strip_prefix("reference-")).map(|m| serde_json::json!(m)), "{finishing:?}");
        }
    }

    /// **Every screen finishing gets the dots back.** The default's
    /// neighbours, the repeat offsets and the reference raster each bring the
    /// page's screen back into a hole the model filled flat.
    #[test]
    fn every_screen_finishing_gets_the_pages_dots_back() {
        let (w, h) = (160, 160);
        let (page, answer, mask) = flat_hole_on_a_screen(square(w, h, 50, 50, 110, 110));
        let all = Mask::new(w, h, true);
        let core = square(w, h, 56, 56, 104, 104);
        let error = |g: &Grid| (0..w * h).filter(|&i| core.data[i]).map(|i| (g.data[i] - page.data[i]).abs()).sum::<f64>() / core.count() as f64;
        let screens = [Finishing::CopyHops, Finishing::Redraw, Finishing::RedrawSharpContours, Finishing::RepeatOffsets, Finishing::ReferenceRaster];
        for finishing in screens {
            let (out, report) = settle_as(std::slice::from_ref(&page), std::slice::from_ref(&answer), &mask, &all, finishing, false);
            assert!(error(&out[0]) < 0.3 * error(&answer), "{finishing:?}: {} against {}", error(&out[0]), error(&answer));
            if finishing.reference().is_none() {
                assert!(report.screen > 0, "{finishing:?}: {report:?}");
            }
        }
    }

    /// **Off the page is no evidence.** A black band the crop holds beyond the
    /// page, 6 px from the mask, does not darken a patch of paper under any
    /// reference finishing, the Poisson boundary included.
    #[test]
    fn what_lies_off_the_page_does_not_pull_the_reference_finishings() {
        let (w, h) = (256, 256);
        let mask = square(w, h, 36, 96, 220, 160);
        let off = square(w, h, 226, 0, w, h);
        let shown = Grid { w, h, data: (0..w * h).map(|i| if off.data[i] { 0.0 } else if mask.data[i] && (i % w / 6) % 2 == 0 { 20.0 } else { 235.0 }).collect() };
        let answer = Grid::new(w, h, 235.0);
        for finishing in [Finishing::ReferenceGlobal, Finishing::ReferenceLocal, Finishing::ReferencePoisson] {
            let (out, _) = settle_as(std::slice::from_ref(&shown), std::slice::from_ref(&answer), &mask, &off.not(), finishing, false);
            let worst = (0..w * h).filter(|&i| mask.data[i]).map(|i| (out[0].data[i] - 235.0).abs()).fold(0.0, f64::max);
            assert!(worst == 0.0, "{finishing:?}: {worst} levels off the paper");
        }
    }

    /// **A mask on the crop's edge.** The screen is still copied back and the
    /// reference Poisson still solves, padded past the edge.
    #[test]
    fn a_mask_on_the_crops_edge_is_finished() {
        let (w, h) = (160, 160);
        let (page, answer, mask) = flat_hole_on_a_screen(square(w, h, 0, 50, 40, 110));
        let all = Mask::new(w, h, true);
        let core = square(w, h, 0, 56, 34, 104);
        let error = |g: &Grid| (0..w * h).filter(|&i| core.data[i]).map(|i| (g.data[i] - page.data[i]).abs()).sum::<f64>() / core.count() as f64;
        let (out, _) = settle_as(std::slice::from_ref(&page), std::slice::from_ref(&answer), &mask, &all, Finishing::Copy, false);
        assert!(error(&out[0]) < 0.3 * error(&answer), "{} against {}", error(&out[0]), error(&answer));
        let (out, report) = settle_as(std::slice::from_ref(&page), std::slice::from_ref(&answer), &mask, &all, Finishing::ReferencePoisson, false);
        assert!(out[0].data.iter().all(|v| (0.0..=255.0).contains(v)), "{report:?}");
    }

    /// **A colour crop made grey.** Three channels with grayscale on are
    /// finished as one grey channel, screen step included, and every channel
    /// of the mask gets the same value.
    #[test]
    fn a_colour_crop_made_grey_gets_one_grey_finish() {
        let (w, h) = (160, 160);
        let (page, answer, mask) = flat_hole_on_a_screen(square(w, h, 50, 50, 110, 110));
        let tint = |g: &Grid, d: f64| Grid { w, h, data: g.data.iter().map(|v| (v + d).clamp(0.0, 255.0)).collect() };
        let shown = [tint(&page, 6.0), page.clone(), tint(&page, -6.0)];
        let answers = [tint(&answer, 6.0), answer.clone(), tint(&answer, -6.0)];
        let all = Mask::new(w, h, true);
        for finishing in [Finishing::Copy, Finishing::ReferencePoisson] {
            let (out, report) = settle_as(&shown, &answers, &mask, &all, finishing, true);
            assert_eq!(out.len(), 3);
            assert!((0..w * h).filter(|&i| mask.data[i]).all(|i| out[0].data[i] == out[1].data[i] && out[1].data[i] == out[2].data[i]), "{finishing:?}");
            if finishing == Finishing::Copy {
                assert!(report.screen > 0, "{report:?}");
            }
        }
    }
}
