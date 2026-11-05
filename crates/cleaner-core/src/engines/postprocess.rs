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
//! steps without the screen, a port of Manga Cleaner Reproduction's
//! finishing ([`super::finishing`]) in place of all four, or the renders' own
//! tone fix ([`Finishing::ToneFix`]). The default is bench 7's.
//!
//! Prototyped in Python on the FLUX 3 bench (docs/research/flux3-klein-5_6b.md,
//! bench 3) and measured against synthetic lettering over real pages.

use serde::{Deserialize, Serialize};

use super::grid::{Grid, Mask, components, distance_to, gather, gaussian, masked_blur, median, percentile};
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

/// A line the answer drew within this many px of the mask's edge continues
/// the page's drawing, and is never a stray mark ([`strays`]).
pub const STRAY_EDGE_PX: f64 = 4.0;
/// A stray mark is no wider or taller than this, in px.
pub const STRAY_MAX_SIDE: usize = 48;
/// A stray mark has no other line within this many px of it.
pub const STRAY_CLEAR_PX: usize = 8;
/// ... and the tone round it, under a [`STRAY_TONE_SIGMA`] px blur, varies
/// by no more than this many levels (standard deviation): a mark on flat
/// paper or an even screen, not one among hatching or texture.
pub const STRAY_FLAT: f64 = 12.0;
const STRAY_TONE_SIGMA: f64 = 3.0;
/// The blur a stray mark is filled from its surroundings with, in px.
const STRAY_FILL_SIGMA: f64 = 4.0;
/// Where less than this much of that blur's window is known (the middle of a
/// found area wider than the blur reaches), the fill is carried in from the
/// blurred outline as a membrane instead.
const STRAY_FILL_SUPPORT: f64 = 0.05;

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

/// The finishing an answer gets before it is written. The first seven are
/// this module's steps with one screen step or none; the next five are Manga
/// Cleaner Reproduction's finishing v2, ported ([`super::finishing::Method`]),
/// instead. Each of those still reads under the name it was first saved
/// with, a serde alias. The last is the tone fix the renders ran before any
/// of these existed.
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
    /// Tone only, then the page's dots redrawn in phase at the answer's tone
    /// wherever the answer is a screen of its own or plain grey
    /// ([`super::halftone::Over::Screen`]): a model's dots are fuzzy, out of
    /// phase or smoothed away, and its tone is right. Its drawing, flat
    /// black and paper stay as they came.
    ToneFillDots,
    /// The answer as it came, grey if asked (ported `off`).
    #[serde(alias = "reference-off")]
    AsIs,
    /// One gain and offset per channel from a ring round the mask (ported
    /// `global`).
    #[serde(alias = "reference-global")]
    GlobalTone,
    /// The global match, then a field of the residual (ported `local`).
    #[serde(alias = "reference-local")]
    LocalTone,
    /// The local match, then the lighting solved by Poisson (ported
    /// `poisson`).
    #[serde(alias = "reference-poisson")]
    Poisson,
    /// The page's own dots copied in phase, nothing else (ported `raster`).
    #[serde(alias = "reference-raster")]
    DotsOnly,
    /// The tone fix a Qwen, FLUX.2 Klein or LaMa answer gets when no
    /// finishing takes its place: a correction curve per channel, learned
    /// from box means on a ring outside the applied mask
    /// ([`super::render::ToneSite`]), then grey if asked. No screen step. It
    /// reads the lettering's ink and the page's opacity, which a [`Crop`]
    /// does not carry, so the renders run it themselves
    /// ([`super::render::PreparedRender::composite`], the LaMa rung) and
    /// [`settle_with`] leaves the answer as it came.
    ToneFix,
}

impl Finishing {
    pub const ALL: [Finishing; 13] = [
        Finishing::Copy,
        Finishing::CopyHops,
        Finishing::Redraw,
        Finishing::RedrawSharpContours,
        Finishing::RepeatOffsets,
        Finishing::ToneOnly,
        Finishing::ToneFillDots,
        Finishing::AsIs,
        Finishing::GlobalTone,
        Finishing::LocalTone,
        Finishing::Poisson,
        Finishing::DotsOnly,
        Finishing::ToneFix,
    ];

    /// The ported method, for the five Manga Cleaner Reproduction versions.
    pub fn ported_method(self) -> Option<super::finishing::Method> {
        use super::finishing::Method;
        match self {
            Finishing::AsIs => Some(Method::Off),
            Finishing::GlobalTone => Some(Method::Global),
            Finishing::LocalTone => Some(Method::Local),
            Finishing::Poisson => Some(Method::Poisson),
            Finishing::DotsOnly => Some(Method::Raster),
            _ => None,
        }
    }
}

/// A finishing as the `cloudFinishing` settings and a cloud recipe keep it:
/// the version, and whether a colour answer is made grey first (the
/// `force_grayscale` of Manga Cleaner Reproduction; a grey page's answer is
/// grey already).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Choice {
    pub finishing: Finishing,
    #[serde(default)]
    pub grayscale: bool,
    /// Whether small marks the model drew on its own inside the mask are
    /// filled from the tone round them first ([`strays`]). Written only when
    /// on, so a choice without it reads, compares and is stored as before.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub strays: bool,
}

impl Choice {
    /// `finishing`, grey or not, with no stray marks taken out.
    pub const fn of(finishing: Finishing, grayscale: bool) -> Choice {
        Choice { finishing, grayscale, strays: false }
    }
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
    /// Mask pixels filled because the model drew a stray mark there
    /// ([`strays`]). Written only when the choice asked for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stray_pixels: Option<usize>,
    /// The version that ran, and whether it made the patch grey.
    #[serde(flatten)]
    pub choice: Choice,
    /// The ported finishing's own details record, for the five ported
    /// versions.
    #[serde(rename = "ported", skip_serializing_if = "Option::is_none")]
    pub method_report: Option<serde_json::Value>,
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
    // The tone fit and the ported finishing work on the whole crop; only
    // the mask keeps what they made.
    let (finished, report) = finish(crop, choice);
    let kept = finished.iter().zip(crop.answer).map(|(finished, answer)| finished.select(crop.mask, answer)).collect();
    (kept, report)
}

/// [`settle_with`] over the whole crop.
fn finish(crop: &Crop<'_>, choice: Choice) -> (Vec<Grid>, Report) {
    let Choice { finishing, grayscale, strays: take_strays } = choice;
    let mask = crop.mask;
    let (w, h) = (mask.w, mask.h);
    let mut report = Report {
        gain: Vec::new(), offset: Vec::new(), evidence: 0, contours: 0, screen: 0, stray_pixels: None, choice, method_report: None,
    };
    if finishing == Finishing::ToneFix {
        // The renders run the tone fix, stray marks included: it needs more
        // of the page than a crop.
        return (crop.answer.to_vec(), Report::tone_fix(choice));
    }
    // Stray marks go first, so every version after finishes the answer as if
    // the model had not drawn them.
    let filled;
    let crop = if take_strays {
        let (answer, found) = without_strays(crop.answer, mask);
        report.stray_pixels = Some(found.count());
        filled = answer;
        &Crop { shown: crop.shown, answer: &filled, mask, page: crop.page }
    } else {
        crop
    };
    if let Some(method) = finishing.ported_method() {
        let (finished, details) = super::finishing::finish(crop, method, grayscale);
        report.method_report = Some(details);
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
        use super::halftone::{Over, Reach, redraw};
        let screen = match finishing {
            Finishing::Copy => redraw(shown, answer, mask, &protected, crop.page, &fade, Reach::Copy, Over::All),
            Finishing::CopyHops => redraw(shown, answer, mask, &protected, crop.page, &fade, Reach::Hops, Over::All),
            Finishing::Redraw | Finishing::RedrawSharpContours => redraw(shown, answer, mask, &protected, crop.page, &fade, Reach::Table, Over::All),
            Finishing::ToneFillDots => redraw(shown, answer, mask, &protected, crop.page, &fade, Reach::Table, Over::Screen),
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
    /// The record of a [`Finishing::ToneFix`] run. What it measured is the
    /// tone alignment's own record ([`super::render::ToneReport`]).
    /// `choice` is the tone fix as it was asked for, grey and stray marks
    /// included; the render that took the stray marks out says how many
    /// pixels it filled.
    pub fn tone_fix(choice: Choice) -> Report {
        debug_assert_eq!(choice.finishing, Finishing::ToneFix);
        Report { gain: Vec::new(), offset: Vec::new(), evidence: 0, contours: 0, screen: 0, stray_pixels: None, choice, method_report: None }
    }

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

/// Small marks the answer drew inside `mask` on its own: the white crescent or
/// flecks an edit model invents on dark tone. A mark is a piece of the
/// answer's lines ([`contours`], found under the [`DESCREEN_SIGMA`] blur) that
///
/// - comes no nearer than [`STRAY_EDGE_PX`] to the mask's edge, so it does not
///   continue a line of the page's,
/// - is no wider or taller than [`STRAY_MAX_SIDE`],
/// - has no other line within [`STRAY_CLEAR_PX`], and flat tone round it
///   ([`STRAY_FLAT`]): hatching and texture are lines among lines and stay,
/// - and, where its outline closes, encloses flat tone with no line in it:
///   a small face or shape the model drew with detail inside is drawing.
///
/// The crop's border counts as the mask's edge where the mask touches it: a
/// line there may run on into page the crop does not show.
///
/// What is returned is each mark with the area its outline encloses, grown
/// by 2 px to take its soft edge.
pub fn strays(answer: &[Grid], mask: &Mask) -> Mask {
    let (w, h) = (mask.w, mask.h);
    let tone = luma(answer);
    let lines = contours(&gaussian(&tone, DESCREEN_SIGMA));
    let flat = gaussian(&tone, STRAY_TONE_SIGMA);
    let frame = Mask { w, h, data: (0..w * h).map(|i| i % w == 0 || i / w == 0 || i % w == w - 1 || i / w == h - 1).collect() };
    let inside = distance_to(&mask.not().or(&frame));
    let (label, sizes) = components(&lines.dilate(1).and(mask));
    // Each piece's box, and whether it reaches the mask's edge.
    let mut boxes = vec![(usize::MAX, usize::MAX, 0usize, 0usize, false); sizes.len()];
    for (i, &l) in label.iter().enumerate() {
        if l == 0 {
            continue;
        }
        let (x, y) = (i % w, i / w);
        let b = &mut boxes[l];
        *b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y), b.4 || inside.data[i] <= STRAY_EDGE_PX);
    }
    let mut found = Mask::new(w, h, false);
    let reach = STRAY_CLEAR_PX + 3;
    for (l, &(x0, y0, x1, y1, edge)) in boxes.iter().enumerate().skip(1) {
        if edge || x1 - x0 + 1 > STRAY_MAX_SIDE || y1 - y0 + 1 > STRAY_MAX_SIDE {
            continue;
        }
        // Worked in a window round the piece.
        let (wx0, wy0) = (x0.saturating_sub(reach), y0.saturating_sub(reach));
        let (wx1, wy1) = ((x1 + reach).min(w - 1), (y1 + reach).min(h - 1));
        let (ww, wh) = (wx1 - wx0 + 1, wy1 - wy0 + 1);
        let at = |x: usize, y: usize| (wy0 + y) * w + wx0 + x;
        let piece = Mask { w: ww, h: wh, data: (0..ww * wh).map(|i| label[at(i % ww, i / ww)] == l).collect() };
        let grown = piece.dilate(2);
        let area = filled(&grown);
        // The tone round the mark, and what its outline encloses past its
        // own soft edge, each at the same distance from the line.
        let near = area.dilate(STRAY_CLEAR_PX).and_not(&area);
        let enclosed = area.and_not(&grown);
        let read = |set: &Mask| {
            let mut tones = Vec::new();
            let mut crowded = false;
            for i in (0..ww * wh).filter(|&i| set.data[i]) {
                let j = at(i % ww, i / ww);
                crowded |= lines.data[j];
                tones.push(flat.data[j]);
            }
            (crowded, tones)
        };
        let (crowded, ring) = read(&near);
        if crowded || ring.is_empty() || spread(&ring) > STRAY_FLAT {
            continue;
        }
        let (detailed, inner) = read(&enclosed);
        if detailed || (!inner.is_empty() && spread(&inner) > STRAY_FLAT) {
            continue;
        }
        for i in 0..ww * wh {
            if area.data[i] {
                found.data[at(i % ww, i / ww)] = true;
            }
        }
    }
    found.and(mask)
}

/// The standard deviation of `values`.
fn spread(values: &[f64]) -> f64 {
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    (values.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / values.len() as f64).sqrt()
}

/// `piece` with every hole in it filled: what its outline encloses.
fn filled(piece: &Mask) -> Mask {
    let (w, h) = (piece.w, piece.h);
    let mut outside = vec![false; w * h];
    let mut stack: Vec<usize> = (0..w * h).filter(|&i| {
        let (x, y) = (i % w, i / w);
        (x == 0 || y == 0 || x == w - 1 || y == h - 1) && !piece.data[i]
    }).collect();
    for &i in &stack {
        outside[i] = true;
    }
    while let Some(i) = stack.pop() {
        let (x, y) = (i % w, i / w);
        let mut step = |j: usize| {
            if !piece.data[j] && !outside[j] {
                outside[j] = true;
                stack.push(j);
            }
        };
        if x > 0 { step(i - 1) }
        if x + 1 < w { step(i + 1) }
        if y > 0 { step(i - w) }
        if y + 1 < h { step(i + w) }
    }
    Mask { w, h, data: outside.into_iter().map(|o| !o).collect() }
}

/// `answer` with its stray marks ([`strays`]) filled from the tone round
/// them, and where they were.
pub fn without_strays(answer: &[Grid], mask: &Mask) -> (Vec<Grid>, Mask) {
    let found = strays(answer, mask);
    (fill_strays(answer, &found, mask), found)
}

/// `answer` with the `found` marks filled from the tone round them: a
/// normalised blur of the mask's other pixels, so the steps after it see the
/// surrounding tone there, and a screen step redraws its dots. Deeper in a
/// found area than the blur reaches ([`STRAY_FILL_SUPPORT`]), the blurred
/// tone at its outline is carried in as a membrane, worked in a window round
/// each area.
fn fill_strays(answer: &[Grid], found: &Mask, mask: &Mask) -> Vec<Grid> {
    if !found.any() {
        return answer.to_vec();
    }
    let (w, h) = (found.w, found.h);
    let hole = found.dilate(1);
    let known = hole.not();
    // Each area's box, grown so the window's border is known pixels.
    let (label, sizes) = components(&hole);
    let mut boxes = vec![(usize::MAX, usize::MAX, 0usize, 0usize); sizes.len()];
    for (i, &l) in label.iter().enumerate().filter(|(_, l)| **l != 0) {
        let (x, y, b) = (i % w, i / w, &mut boxes[l]);
        *b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y));
    }
    answer
        .iter()
        .map(|channel| {
            let (mut around, weight) = masked_blur(channel, &known, STRAY_FILL_SIGMA);
            for &(x0, y0, x1, y1) in boxes.iter().skip(1) {
                let (wx0, wy0) = (x0.saturating_sub(2), y0.saturating_sub(2));
                let (wx1, wy1) = ((x1 + 2).min(w - 1), (y1 + 2).min(h - 1));
                let (ww, wh) = (wx1 - wx0 + 1, wy1 - wy0 + 1);
                let at = |i: usize| (wy0 + i / ww) * w + wx0 + i % ww;
                let held = Mask { w: ww, h: wh, data: (0..ww * wh).map(|i| weight.data[at(i)] >= STRAY_FILL_SUPPORT).collect() };
                if held.data.iter().all(|&k| k) || !held.any() {
                    continue;
                }
                let values = Grid { w: ww, h: wh, data: (0..ww * wh).map(|i| around.data[at(i)]).collect() };
                let field = membrane(&values, &held);
                for i in (0..ww * wh).filter(|&i| !held.data[i]) {
                    around.data[at(i)] = field.data[i];
                }
            }
            let mut out = channel.clone();
            for i in 0..out.data.len() {
                if found.data[i] && mask.data[i] {
                    out.data[i] = around.data[i];
                }
            }
            out
        })
        .collect()
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
        for (i, correction) in correction.iter_mut().enumerate() {
            let weight = (support.data[i] / FULL_SUPPORT).clamp(0.0, 1.0);
            *correction = *correction * (1.0 - weight) + estimate.data[i] * weight;
        }
    }
    for (i, correction) in correction.iter().enumerate() {
        out.data[i] += correction.clamp(-LOCAL_CAP, LOCAL_CAP) * fade.data[i];
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
        settle_with(&Crop { shown, answer, mask, page }, Choice::of(finishing, grayscale))
    }

    /// **Every finishing writes only the mask and says which one ran.**
    /// Outside the mask the answer comes back as it was, the ported ones
    /// included, and a ported finishing reports its method.
    #[test]
    fn every_finishing_keeps_the_answer_outside_the_mask() {
        let (w, h) = (160, 160);
        let (page, answer, mask) = flat_hole_on_a_screen(square(w, h, 50, 50, 110, 110));
        let all = Mask::new(w, h, true);
        for finishing in Finishing::ALL {
            let (out, report) = settle_as(std::slice::from_ref(&page), std::slice::from_ref(&answer), &mask, &all, finishing, false);
            assert!((0..w * h).all(|i| mask.data[i] || out[0].data[i] == answer.data[i]), "{finishing:?} wrote outside the mask");
            assert_eq!(report.choice.finishing, finishing);
            let method = report.method_report.as_ref().map(|details| details["method"].clone());
            assert_eq!(method, finishing.ported_method().map(|m| serde_json::json!(m.name())), "{finishing:?}");
        }
    }

    /// **Old names still load.** A settings file or journal written before the
    /// five ported versions were renamed reads as the new version, and the
    /// new name is what is written back.
    #[test]
    fn the_ported_versions_read_under_their_old_names() {
        let old = [("reference-off", Finishing::AsIs), ("reference-global", Finishing::GlobalTone), ("reference-local", Finishing::LocalTone), ("reference-poisson", Finishing::Poisson), ("reference-raster", Finishing::DotsOnly)];
        for (name, finishing) in old {
            assert_eq!(serde_json::from_value::<Finishing>(serde_json::json!(name)).unwrap(), finishing);
        }
        let names: Vec<_> = Finishing::ALL[7..12].iter().map(|f| serde_json::json!(f)).collect();
        assert_eq!(names, ["as-is", "global-tone", "local-tone", "poisson", "dots-only"].map(|n| serde_json::json!(n)));
        let choice: Choice = serde_json::from_value(serde_json::json!({"finishing": "reference-poisson", "grayscale": true})).unwrap();
        assert_eq!(choice, Choice::of(Finishing::Poisson, true));
    }

    /// **Every screen finishing gets the dots back.** The default's
    /// neighbours, the repeat offsets and the ported raster each bring the
    /// page's screen back into a hole the model filled flat.
    #[test]
    fn every_screen_finishing_gets_the_pages_dots_back() {
        let (w, h) = (160, 160);
        let (page, answer, mask) = flat_hole_on_a_screen(square(w, h, 50, 50, 110, 110));
        let all = Mask::new(w, h, true);
        let core = square(w, h, 56, 56, 104, 104);
        let error = |g: &Grid| (0..w * h).filter(|&i| core.data[i]).map(|i| (g.data[i] - page.data[i]).abs()).sum::<f64>() / core.count() as f64;
        let screens = [Finishing::CopyHops, Finishing::Redraw, Finishing::RedrawSharpContours, Finishing::RepeatOffsets, Finishing::ToneFillDots, Finishing::DotsOnly];
        for finishing in screens {
            let (out, report) = settle_as(std::slice::from_ref(&page), std::slice::from_ref(&answer), &mask, &all, finishing, false);
            assert!(error(&out[0]) < 0.3 * error(&answer), "{finishing:?}: {} against {}", error(&out[0]), error(&answer));
            if finishing.ported_method().is_none() {
                assert!(report.screen > 0, "{finishing:?}: {report:?}");
            }
        }
    }

    /// **Filling lost dots puts the model's own dots in phase, at its tone.**
    /// An answer that drew dots of its own in the hole, half a cycle out of
    /// phase and 6 levels dark, gets the page's dots back in phase, and their
    /// mean over the hole is the answer's tone (Tone only's), not the table's.
    #[test]
    fn filling_lost_dots_redraws_the_models_dots_in_phase_at_its_tone() {
        let (w, h) = (160, 160);
        let (page, _, mask) = flat_hole_on_a_screen(square(w, h, 50, 50, 110, 110));
        let shifted = Grid { w, h, data: (0..w * h).map(|i| page.data[(i / w) * w + (i % w + 3) % w] - 6.0).collect() };
        let answer = page.select(&mask.not(), &shifted);
        let all = Mask::new(w, h, true);
        let finish = |finishing| settle_as(std::slice::from_ref(&page), std::slice::from_ref(&answer), &mask, &all, finishing, false);
        let ((filled, report), (toned, _)) = (finish(Finishing::ToneFillDots), finish(Finishing::ToneOnly));
        assert!(report.screen > 0, "{report:?}");
        let core = square(w, h, 62, 62, 98, 98);
        let mean = |g: &Grid| (0..w * h).filter(|&i| core.data[i]).map(|i| g.data[i]).sum::<f64>() / core.count() as f64;
        let phase = |g: &Grid| (0..w * h).filter(|&i| core.data[i]).map(|i| (g.data[i] - mean(g) - (page.data[i] - 128.0)).abs()).sum::<f64>() / core.count() as f64;
        assert!(phase(&filled[0]) < 0.3 * phase(&toned[0]), "{} against {}", phase(&filled[0]), phase(&toned[0]));
        assert!((mean(&filled[0]) - mean(&toned[0])).abs() < 1.0, "{} against {}", mean(&filled[0]), mean(&toned[0]));
    }

    /// **Filling lost dots leaves the model's drawing.** Flat black hair and
    /// stipple the model drew over a screen stay as Tone only left them;
    /// plain grey beside them gets the page's dots.
    #[test]
    fn filling_lost_dots_keeps_flat_black_and_drawn_texture() {
        let (w, h) = (200, 160);
        let (page, _, mask) = flat_hole_on_a_screen(square(w, h, 40, 50, 160, 110));
        let (black, stippled) = (square(w, h, 40, 50, 80, 110), square(w, h, 120, 50, 160, 110));
        let answer = Grid {
            w,
            h,
            data: (0..w * h)
                .map(|i| match () {
                    _ if black.data[i] => 0.0,
                    // Drawn stipple at the page's mean tone: no lines to
                    // protect, and no screen at its two frequencies.
                    _ if stippled.data[i] => 128.0 + 60.0 * (((i as u64).wrapping_mul(2654435761) >> 7) % 1000) as f64 / 500.0 - 60.0,
                    _ if mask.data[i] => 128.0,
                    _ => page.data[i],
                })
                .collect(),
        };
        let all = Mask::new(w, h, true);
        let finish = |finishing| settle_as(std::slice::from_ref(&page), std::slice::from_ref(&answer), &mask, &all, finishing, false);
        let ((filled, _), (toned, _)) = (finish(Finishing::ToneFillDots), finish(Finishing::ToneOnly));
        for (name, part) in [("black", square(w, h, 46, 56, 74, 104)), ("stipple", square(w, h, 126, 56, 154, 104))] {
            let moved = (0..w * h).filter(|&i| part.data[i]).map(|i| (filled[0].data[i] - toned[0].data[i]).abs()).sum::<f64>() / part.count() as f64;
            assert!(moved < 0.5, "{name} moved {moved} levels on average");
        }
        let grey = square(w, h, 90, 60, 110, 100);
        let swing = (0..w * h).filter(|&i| grey.data[i]).map(|i| (filled[0].data[i] - 128.0).abs()).sum::<f64>() / grey.count() as f64;
        assert!(swing > 20.0, "the plain grey got no dots: {swing}");
    }

    /// **Off the page is no evidence.** A black band the crop holds beyond the
    /// page, 6 px from the mask, does not darken a patch of paper under any
    /// ported finishing, the Poisson boundary included.
    #[test]
    fn what_lies_off_the_page_does_not_pull_the_ported_finishings() {
        let (w, h) = (256, 256);
        let mask = square(w, h, 36, 96, 220, 160);
        let off = square(w, h, 226, 0, w, h);
        let shown = Grid { w, h, data: (0..w * h).map(|i| if off.data[i] { 0.0 } else if mask.data[i] && (i % w / 6) % 2 == 0 { 20.0 } else { 235.0 }).collect() };
        let answer = Grid::new(w, h, 235.0);
        for finishing in [Finishing::GlobalTone, Finishing::LocalTone, Finishing::Poisson] {
            let (out, _) = settle_as(std::slice::from_ref(&shown), std::slice::from_ref(&answer), &mask, &off.not(), finishing, false);
            let worst = (0..w * h).filter(|&i| mask.data[i]).map(|i| (out[0].data[i] - 235.0).abs()).fold(0.0, f64::max);
            assert!(worst == 0.0, "{finishing:?}: {worst} levels off the paper");
        }
    }

    /// **A mask on the crop's edge.** The screen is still copied back and the
    /// ported Poisson still solves, padded past the edge.
    #[test]
    fn a_mask_on_the_crops_edge_is_finished() {
        let (w, h) = (160, 160);
        let (page, answer, mask) = flat_hole_on_a_screen(square(w, h, 0, 50, 40, 110));
        let all = Mask::new(w, h, true);
        let core = square(w, h, 0, 56, 34, 104);
        let error = |g: &Grid| (0..w * h).filter(|&i| core.data[i]).map(|i| (g.data[i] - page.data[i]).abs()).sum::<f64>() / core.count() as f64;
        let (out, _) = settle_as(std::slice::from_ref(&page), std::slice::from_ref(&answer), &mask, &all, Finishing::Copy, false);
        assert!(error(&out[0]) < 0.3 * error(&answer), "{} against {}", error(&out[0]), error(&answer));
        let (out, report) = settle_as(std::slice::from_ref(&page), std::slice::from_ref(&answer), &mask, &all, Finishing::Poisson, false);
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
        for finishing in [Finishing::Copy, Finishing::Poisson] {
            let (out, report) = settle_as(&shown, &answers, &mask, &all, finishing, true);
            assert_eq!(out.len(), 3);
            assert!((0..w * h).filter(|&i| mask.data[i]).all(|i| out[0].data[i] == out[1].data[i] && out[1].data[i] == out[2].data[i]), "{finishing:?}");
            if finishing == Finishing::Copy {
                assert!(report.screen > 0, "{report:?}");
            }
        }
    }

    /// A dark dot screen with a hole the model filled with the screen, and
    /// `draw` painted into its answer.
    fn dark_screen_with(draw: impl Fn(usize, usize) -> Option<f64>) -> (Grid, Grid, Mask) {
        let (w, h) = (160, 160);
        let period = std::f64::consts::TAU / 5.0;
        let page = Grid {
            w,
            h,
            data: (0..w * h).map(|i| 50.0 + 40.0 * ((i % w) as f64 * period).cos() * ((i / w) as f64 * period).cos()).collect(),
        };
        let mask = square(w, h, 30, 30, 130, 130);
        let answer = Grid {
            w,
            h,
            data: (0..w * h).map(|i| match draw(i % w, i / w) {
                Some(v) if mask.data[i] => v,
                _ => page.data[i],
            }).collect(),
        };
        (page, answer, mask)
    }

    /// A white crescent, the mark the 13_00 answer had, centred at (80, 80).
    fn crescent(x: usize, y: usize) -> Option<f64> {
        let (dx, dy) = (x as f64 - 80.0, y as f64 - 80.0);
        let outer = dx * dx / 144.0 + dy * dy / 25.0 <= 1.0;
        let inner = dx * dx / 120.0 + (dy + 2.5) * (dy + 2.5) / 16.0 <= 1.0;
        (outer && !inner).then_some(245.0)
    }

    /// **A mark the model drew on its own is filled from the tone round it.**
    /// A white crescent on a dark screen, well inside the mask, is found and
    /// comes back at the screen's tone; without the step it stays white.
    #[test]
    fn a_stray_mark_on_flat_tone_is_filled() {
        let (page, answer, mask) = dark_screen_with(crescent);
        let found = strays(std::slice::from_ref(&answer), &mask);
        let mark = Mask { w: mask.w, h: mask.h, data: (0..mask.w * mask.h).map(|i| crescent(i % mask.w, i / mask.w).is_some()).collect() };
        assert!(mark.and_not(&found).count() == 0, "{} of the mark's {} px missed", mark.and_not(&found).count(), mark.count());
        assert_eq!(found.and_not(&mark.dilate(4)).count(), 0, "pixels taken more than 4 px from the mark");
        let all = Mask::new(mask.w, mask.h, true);
        let crop = Crop { shown: std::slice::from_ref(&page), answer: std::slice::from_ref(&answer), mask: &mask, page: &all };
        let mean = |g: &Grid| (0..g.data.len()).filter(|&i| mark.data[i]).map(|i| g.data[i]).sum::<f64>() / mark.count() as f64;
        let (kept, report) = settle_with(&crop, Choice::of(Finishing::Copy, false));
        assert!(mean(&kept[0]) > 200.0, "the plain finishing kept the mark: {}", mean(&kept[0]));
        assert_eq!(report.stray_pixels, None);
        let (taken, report) = settle_with(&crop, Choice { strays: true, ..Choice::default() });
        assert!(mean(&taken[0]) < 90.0, "the mark is still there: {}", mean(&taken[0]));
        assert_eq!(report.stray_pixels, Some(found.count()));
        assert!((0..page.data.len()).all(|i| mask.data[i] || taken[0].data[i] == answer.data[i]), "written outside the mask");
    }

    /// **The page's drawing and texture stay.** A line that runs out of the
    /// mask continues the page's, and close hatching is lines among lines:
    /// neither is a stray mark, however small its pieces.
    #[test]
    fn lines_from_the_edge_and_hatching_are_not_stray() {
        let line = |x: usize, y: usize| ((78..82).contains(&x) && (20..90).contains(&y)).then_some(245.0);
        let (_, answer, mask) = dark_screen_with(line);
        assert_eq!(strays(std::slice::from_ref(&answer), &mask).count(), 0, "a line from the edge");
        let hatching = |x: usize, y: usize| {
            ((50..110).contains(&x) && (50..110).contains(&y) && (x + y) % 6 < 2 && (x / 10 + y / 10) % 2 == 0).then_some(245.0)
        };
        let (_, answer, mask) = dark_screen_with(hatching);
        assert_eq!(strays(std::slice::from_ref(&answer), &mask).count(), 0, "hatching");
    }

    /// Flat paper with a dark mask-sized square hole, and `draw` inked into
    /// the answer inside it, centred at (100, 100).
    fn paper_with(draw: impl Fn(f64, f64) -> bool) -> (Grid, Grid, Mask) {
        let (w, h) = (200, 200);
        let mask = square(w, h, 20, 20, 180, 180);
        let page = Grid::new(w, h, 245.0);
        let answer = Grid {
            w,
            h,
            data: (0..w * h).map(|i| {
                let (x, y) = ((i % w) as f64 - 100.0, (i / w) as f64 - 100.0);
                if mask.data[i] && draw(x, y) { 20.0 } else { 245.0 }
            }).collect(),
        };
        (page, answer, mask)
    }

    fn ring(outer: f64, inner: f64) -> impl Fn(f64, f64) -> bool {
        move |x, y| (inner..=outer).contains(&x.hypot(y))
    }

    /// **A found area wider than the fill's blur comes back as paper.** A
    /// ghost ring 44 px across on white paper encloses flat paper, so all of
    /// it is found; its middle lies past the blur's reach, and is carried in
    /// from the outline, not left black. Through the stray step alone and
    /// through a finishing.
    #[test]
    fn a_wide_stray_area_is_filled_to_its_middle() {
        let (page, answer, mask) = paper_with(ring(22.0, 19.0));
        let (filled, found) = without_strays(std::slice::from_ref(&answer), &mask);
        assert!(found.at(100, 100), "the ring's inside was not found");
        assert!(found.count() > 1500, "{}", found.count());
        let darkest = (0..found.data.len()).filter(|&i| found.data[i]).map(|i| filled[0].data[i]).fold(f64::MAX, f64::min);
        assert!(darkest > 200.0, "a found pixel came back at {darkest}");
        let all = Mask::new(mask.w, mask.h, true);
        let crop = Crop { shown: std::slice::from_ref(&page), answer: std::slice::from_ref(&answer), mask: &mask, page: &all };
        for finishing in [Finishing::Copy, Finishing::ToneOnly, Finishing::Poisson] {
            let (out, _) = settle_with(&crop, Choice { finishing, grayscale: false, strays: true });
            let dark = out[0].data.iter().filter(|v| **v < 128.0).count();
            assert_eq!(dark, 0, "{finishing:?} left {dark} dark px");
        }
    }

    /// **A closed shape with detail inside is drawing.** The same ring with
    /// two eyes in it is a small face: neither the outline nor the eyes are
    /// taken.
    #[test]
    fn a_closed_outline_with_content_inside_stays() {
        let eyes = |x: f64, y: f64| (x + 7.0).hypot(y + 5.0) <= 2.5 || (x - 7.0).hypot(y + 5.0) <= 2.5;
        let ring = ring(22.0, 19.0);
        let (_, answer, mask) = paper_with(|x, y| ring(x, y) || eyes(x, y));
        assert_eq!(strays(std::slice::from_ref(&answer), &mask).count(), 0);
        // The ring alone is still a ghost to take out.
        let (_, answer, mask) = paper_with(ring);
        assert!(strays(std::slice::from_ref(&answer), &mask).at(100, 100));
    }

    /// **The crop's border is the mask's edge where the mask meets it.** A
    /// short line that comes in across the crop's border continues page the
    /// crop does not show, and stays.
    #[test]
    fn a_line_from_the_crop_border_is_not_stray() {
        let (w, h) = (160, 160);
        let mask = square(w, h, 0, 30, 130, 130);
        let answer = Grid { w, h, data: (0..w * h).map(|i| if (60..62).contains(&(i / w)) && i % w < 20 { 20.0 } else { 245.0 }).collect() };
        assert_eq!(strays(std::slice::from_ref(&answer), &mask).count(), 0);
        // The same line clear of the border is a stray.
        let moved = Grid { w, h, data: (0..w * h).map(|i| if (60..62).contains(&(i / w)) && (40..60).contains(&(i % w)) { 20.0 } else { 245.0 }).collect() };
        assert!(strays(std::slice::from_ref(&moved), &mask).count() > 0);
    }

    /// **A choice without the step is stored as before.** `strays` is written
    /// only when on, and a record without it reads as off.
    #[test]
    fn strays_is_written_only_when_on() {
        assert_eq!(serde_json::json!(Choice::of(Finishing::Copy, true)), serde_json::json!({"finishing": "copy", "grayscale": true}));
        let on = Choice { strays: true, ..Choice::of(Finishing::ToneOnly, false) };
        assert_eq!(serde_json::json!(on), serde_json::json!({"finishing": "tone-only", "grayscale": false, "strays": true}));
        assert_eq!(serde_json::from_value::<Choice>(serde_json::json!(on)).unwrap(), on);
        assert_eq!(serde_json::from_value::<Choice>(serde_json::json!({"finishing": "copy"})).unwrap(), Choice::default());
    }
}
