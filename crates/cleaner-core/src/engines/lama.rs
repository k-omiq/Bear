//! Rung 2 - manga-finetuned LaMa, the default inpainter.
//!
//! The rung that exists for
//! what rung 0 cannot reconstruct: screentone, halftone and line art behind
//! the text, where a flat fill is a hole.
//!
//! ## What the model actually is, and what it wanted
//!
//! `lama-manga.onnx`, opset 17, one input pair and one output:
//!
//! ```text
//! image  Float32[batch, 3, 512, 512]
//! mask   Float32[batch, 1, 512, 512]
//! output Float32[batch, 3, 512, 512]
//! ```
//!
//! Four properties of that contract were **measured against the file** rather
//! than taken from the upstream convention. The ladder carried a second
//! inpainter at the time - MI-GAN, since removed - and it disagreed with this
//! one about three of the four, which is what made measuring rather than
//! assuming the point. A wrong guess here is not an error; it is a plausible
//! image with the background destroyed:
//!
//! - **The value range is 0..1.** Fed the same crop scaled to 0..255, the model
//!   returns a tensor whose mean is 1.000 and whose reconstruction error inside
//!   the hole is 0.74 in 0..1 units against 0.064 for the same crop at 0..1.
//!   It saturates; it does not rescale.
//! - **The mask's polarity is 1 = hole**, LaMa's own convention. With the mask
//!   inverted the error *outside* the hole goes from 0.010 to 0.743 - the model
//!   faithfully repaints the whole page and leaves the text alone, which is the
//!   failure this measurement exists to rule out.
//! - **Channels come back in the order they went in.** A crop whose red channel
//!   was darkened to 0.4× returns with channel 0 at mean 0.305 against 0.746 and
//!   0.748 on the other two. There is no BGR swap in the export.
//! - **The graph masks the image itself.** Feeding the crop with the text still
//!   in it and feeding the same crop with the masked region zeroed produce
//!   **bit-identical** output, so `image × (1 − mask)` is inside the exported
//!   graph. This module zeroes anyway: the zeroing is per *tile*, and a tile's
//!   mask is deliberately smaller than the region's, so relying on the graph
//!   would tie the tiling scheme below to a property of one export.
//!
//! And the fifth measurement, which is the one the compositor is built around:
//! **the model alters pixels it was never asked to touch.** Against a
//! ground-truth crop, the returned samples outside the mask are off by a mean
//! of 8.0 8-bit levels within 5 px of the hole, 3.4 at 6–16 px, and **2.5 levels
//! even 65 px away and further**, with a single worst sample at 88 levels. 68.6%
//! of all out-of-mask samples move by at least one level. That is why
//! the contract is enforced
//! by *writing through a mask* rather than by trusting a model, and why
//! [`Rendered::pixels`] carries the page's own pixels everywhere the mask does
//! not reach.
//!
//! That drift is also what the tone alignment reads. On 27 real screentone
//! crops (`docs/research/big-lama.md` section 7) the answer came back about
//! 2.5 levels darker than the page, and the ring outside the applied mask
//! drifts the same way. Each tile's raw answer is measured on that ring by the
//! same code the generated path uses ([`crate::engines::render::ToneSite`]),
//! and the curve is taken out of the answer before it is committed.
//!
//! ## Sessions are held and regions are not batched
//!
//! [`Inpainter`] owns a session for its whole life. Building costs seconds
//! and running costs one, so a session rebuilt per region would spend most of
//! the job in the loader. Batching is the optimisation that is **not** here and
//! must not be added - manga-LaMa's only dynamic axis is `batch`, the spatial
//! input is fixed at 512², and per-region cost is flat to batch 4 and worse
//! above it. There is no batching
//! path to build, which is the useful half of that result.
//!
//! ## Geometry: padding, tiling, and where the pixels come from
//!
//! > Padding to 512: only ever with **real surrounding page pixels**, and where
//! > the crop abuts the true page edge, **edge-replicate** - never reflection.
//!
//! `page` is whatever raster the caller decoded - in the pipeline it is a
//! decode window (rule 4), which
//! is sized to contain a 512² engine context and is clamped to the owning page
//! rectangle. So this module's "page edge" and rule 4's are the same edge, and
//! [`EdgePad::Replicate`] is recorded exactly when the clamp bit.
//!
//! Above 512 the region is tiled, and the tiling is where rule 4's second
//! sentence earns its place:
//!
//! > Box > 512 → **tile the mask with 128 px overlap**, run each tile against
//! > the running composite, and blend inside the isolation margin.
//!
//! Tiles are laid out in raster order and each one **commits only its core** -
//! the crop less half the overlap on any side another tile still follows. The
//! next tile therefore opens with 64 px of already-inpainted page as ordinary
//! unmasked context and fills onward from it. That is what "against the running
//! composite" buys: without it two neighbouring tiles invent two continuations
//! of the same screentone independently and the overlap is a cross-fade between
//! two guesses rather than one fill carried across.
//!
//! A tile's **hole**, though, is not cut at its commit. It is cut at what has
//! already been filled, which is a different line and the difference is the
//! whole point: the mask does not stop at the core, so the text in the hand-off
//! band sits inside the same crop, and a hole that stopped at the commit
//! boundary would leave that text in the tensor as context for the model to
//! read and continue back into the pixels the tile does keep. Measured at 254
//! 8-bit levels of dependence on pixels the mask exists to erase, against 0 on
//! every untiled region of the same page. The removed rung 3 stated the same
//! rule from the other side, and stated it first; this is now the only
//! statement of it.
//!
//! Past four model inputs - 2048 px in either dimension - the region is
//! [`Decline`]d rather than tiled. Twenty-five sequential model runs is not a
//! long wait, it is a different product, and the rule holds: leaving text is
//! recoverable and a bad fill is not.
//!
//! ## What this rung refuses outright
//!
//! Rungs 2-4
//! are disabled
//! for indexed and CMYK sources, and [`crate::image::ColorMode::allows_model_engines`] is
//! that rule. Bitonal is this module's own addition: a 1-bit sample is not
//! a level, so continuous-tone output
//! written into one is a per-pixel threshold - the nearest-entry snapping
//! that is refused, in two colours. Declining is the conservative half of an
//! undecided question.
//!
//! ## Where the geometry went
//!
//! The tiling, the applied mask, the write bound, the decline vocabulary and
//! the isolation ramp are **not in this file**. They were lifted into
//! [`crate::engines::model`] when rung 3 arrived, because rule 4 names one
//! engine context for both model rungs and a second copy of that number would
//! eventually disagree with this one. Everything this module used to export
//! under its own name it still exports, by re-export, so the rung's public
//! surface is unchanged; what is left below is the part that is genuinely
//! manga-LaMa's - a float32 tensor pair, `1 = hole`, and a graph that masks its
//! own input.

use std::path::Path;

use crate::accel::{self, Preference, Selection, SessionError};
use crate::constants::{ISOLATION_RADIUS, LAMA_INTRA_THREADS};
use crate::fit::Fitted;
use crate::image::{ColorMode, Raster};
use crate::mask::Mask;

pub use crate::engines::model::{
    Decline, Error, ISOLATION_FEATHER, MAX_BOX, MODEL_INPUT, Rendered, TILE_HANDOFF, TILE_OVERLAP,
    TILE_STRIDE, applied_mask, plan, tile_origins, write_bound,
};
use crate::engines::model::{AlphaRamp, AnswerMemo, answer_mode, ceiling_for, dither_at, f32_bytes, input_digest, page_crop, commit_srgb, tiles};
use crate::engines::postprocess::{Choice, Report};
use crate::engines::render::{Preprocessing, ToneReport, ToneSite, crop_opacity, crop_samples, tone_grids};
use crate::mask::Rect;
use crate::strip::window::EdgePad;

/// Whether this rung can run on this page at all.
///
/// Asked separately from [`Inpainter::render`] so a router can route around the
/// rung rather than reach it and be refused: [`crate::fit::fit_within`]
/// consults it before it names a route.
///
/// Rung 2 adds nothing to [`crate::engines::model::applies`]: its tensors are
/// float32, so unlike rung 3 it has no quarrel with a 16-bit source.
pub fn applies(page: &Raster) -> bool {
    crate::engines::model::applies(page)
}

/// Every reason this region would be declined, before a session is opened.
pub fn declines(page: &Raster, fitted: &Fitted) -> Option<Decline> {
    crate::engines::model::declines(page, fitted)
}

/// How the hole handed to the model is cut: the `lamaHole` setting (Settings >
/// Models > Cleaning). The written set is the same under all three; only what the
/// model is asked to invent inside it changes.
///
/// Each one wins somewhere, which is why it is a choice. Measured on 36 stored
/// detections of three real chapters (2026-11-03, `docs/findings.md`):
///
/// - [`HoleMode::Tight`] keeps art under lettering (hair lines, hatching,
///   screens) but can leave grey letter-shaped marks on black and flat paper,
///   where the mask stops inside a stroke's soft edge.
/// - [`HoleMode::Wide`] removes those marks but asks the model to invent 5 px of
///   drawing around every glyph, and over art it smears it.
/// - [`HoleMode::Adaptive`] is wide where the page beside the lettering is flat
///   and tight where it is art. On those 36 it matched Tight on art and Wide on
///   black and paper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HoleMode {
    /// The lettering itself ([`crate::fit::Fitted::ink`]), as 1.0.0-beta.15 and
    /// earlier cut it. No lettering extent and no flat tone. The default.
    #[default]
    Tight,
    /// The whole applied mask, the lettering grown by
    /// [`crate::constants::ISOLATION_RADIUS`], as 1.0.0-beta.16 to beta.18 cut
    /// it, with the lettering extent and the flat tone (`FlatParts`).
    Wide,
    /// The applied mask where the ring beside it is flat and the lettering where
    /// it is art ([`model_hole`]), with the lettering extent and the flat tone.
    Adaptive,
}

impl HoleMode {
    pub const ALL: [HoleMode; 3] = [HoleMode::Tight, HoleMode::Wide, HoleMode::Adaptive];

    /// Whether a detector's mask is grown by [`crate::fit::lettering_extent`]
    /// before this hole is cut. The run does it, as it alone knows whether a
    /// mask came from a detector. Tight leaves the mask as it was found.
    pub fn extends_lettering(self) -> bool {
        self != HoleMode::Tight
    }
}

/// The hole handed to the model under `mode` ([`HoleMode`]).
///
/// Tight is [`crate::fit::Fitted::ink`]. Wide is all of [`applied_mask`]: a
/// segmentation mask, or the padded seed a run stores, stops on or just inside the
/// soft edge of a stroke, the model reads what it leaves as an outline and fills
/// the hole to match, so the letters come back in grey. On 12 real pages (SAM-TS-L
/// and small Ogkalu, padding 0, 34 regions reaching LaMa) the lettering-cut hole
/// had 18 declined and 5 px of growth 4; [`crate::fit::lettering_extent`] and the
/// flat tone took that to 1.
///
/// Adaptive decides each pixel of the margin by the stretch of ring beside it:
/// in the hole where [`HOLE_RING_SHARE`] of the ring pixels within [`LOCAL_REACH`]
/// are even with their neighbours, so a caption half on black and half on art is
/// wide on the black half only. A pixel with too little ring beside it, between
/// glyphs, takes the decision of its part's whole ring. One decision per part did
/// not work: white-outlined text on black next to art read as busy as text over
/// hair.
///
/// **This does not change the written set**: the hole lies inside the applied
/// mask, so it changes what the model invents and never what the patch covers.
pub fn model_hole(page: &Raster, fitted: &Fitted, mode: HoleMode) -> Result<Mask, Error> {
    let managed = crate::engines::model::editable_color(page)?;
    let applied = applied_mask(fitted, page.width, page.height);
    Ok(hole_of(page, fitted, &applied, &parts_of(page, &applied, &managed), mode))
}

/// How far around a part of the hole the paper is read, past the applied mask.
const FLAT_RING: u32 = ISOLATION_RADIUS + 3;
/// The fewest paper pixels that can call a part of the hole flat.
const FLAT_SAMPLES: usize = 32;
/// The share of those that must sit at one tone. A fold or a line running under
/// the lettering crosses the ring and is a fraction of it, and a looser share
/// erased such lines on real pages, where the model continues them.
const FLAT_RING_SHARE: f64 = 0.99;
/// The share of ring pixels that must be even for a pixel of the margin to be in
/// the model's hole under [`HoleMode::Adaptive`]. Under it the ring is art and the hole there
/// is the lettering, grown by [`BUSY_HOLE_MARGIN`]. 0.75 gave the same pictures
/// on the 36 regions it was tried on.
const HOLE_RING_SHARE: f64 = 0.9;
/// How far past the lettering the hole reaches on art.
const BUSY_HOLE_MARGIN: u32 = 0;
/// How far around a pixel of the applied mask its own stretch of ring is read.
const LOCAL_REACH: u32 = FLAT_RING + 2;
/// The fewest ring pixels that local reading needs; with fewer the part's whole
/// ring decides.
const LOCAL_SAMPLES: u32 = 12;

/// One connected part of a region's applied mask, and its ring on the page.
struct Part {
    pixels: Vec<(i64, i64)>,
    /// The share of the ring at its median tone, and that tone. `None` for a ring
    /// with fewer than [`FLAT_SAMPLES`] pixels.
    ring: Option<(f64, [f32; 3])>,
}

fn parts_of(page: &Raster, applied: &Mask, managed: &crate::image::color::ManagedColor) -> Vec<Part> {
    let bounds = applied.bounds;
    let page_rect = Rect::new(0, 0, page.width, page.height);
    let tolerance = 2.0 * crate::fit::deviation_threshold(crate::fit::page_noise_sigma(page));
    let margin = applied.dilated_within(1, page_rect);
    let mut seen = Mask::empty(bounds);
    let mut stack = Vec::new();
    let mut parts = Vec::new();
    for sy in bounds.y..bounds.bottom() {
        for sx in bounds.x..bounds.right() {
            if seen.contains(sx, sy) || !applied.contains(sx, sy) {
                continue;
            }
            let mut pixels = Vec::new();
            seen.set(sx, sy, true);
            stack.push((sx, sy));
            let (mut x0, mut y0, mut x1, mut y1) = (sx, sy, sx, sy);
            while let Some((x, y)) = stack.pop() {
                pixels.push((x, y));
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        let (nx, ny) = (x + dx, y + dy);
                        if applied.contains(nx, ny) && !seen.contains(nx, ny) {
                            seen.set(nx, ny, true);
                            stack.push((nx, ny));
                        }
                    }
                }
            }
            let mut this = Mask::empty(Rect::new(x0, y0, (x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32));
            for &(x, y) in &pixels {
                this.set(x, y, true);
            }
            let ring = this.dilated_within(FLAT_RING, page_rect);
            let mut levels = Vec::new();
            let mut colours = [Vec::new(), Vec::new(), Vec::new()];
            for y in ring.bounds.y..ring.bounds.bottom() {
                for x in ring.bounds.x..ring.bounds.right() {
                    if ring.contains(x, y) && !margin.contains(x, y) {
                        levels.push(page.luma16_at(x as u32, y as u32) as i32);
                        let rgb = managed.pixel(page, x as u32, y as u32);
                        for (channel, values) in colours.iter_mut().enumerate() {
                            values.push(rgb[channel]);
                        }
                    }
                }
            }
            let ring = (levels.len() >= FLAT_SAMPLES).then(|| {
                let middle = levels.len() / 2;
                let median = *levels.select_nth_unstable(middle).1;
                let at_median = levels.iter().filter(|&&level| f64::from((level - median).abs()) <= f64::from(tolerance)).count();
                let tone = colours.map(|mut values: Vec<f32>| *values.select_nth_unstable_by(middle, f32::total_cmp).1);
                (at_median as f64 / levels.len() as f64, tone)
            });
            parts.push(Part { pixels, ring });
        }
    }
    parts
}

fn hole_of(page: &Raster, fitted: &Fitted, applied: &Mask, parts: &[Part], mode: HoleMode) -> Mask {
    match mode {
        HoleMode::Tight => return fitted.ink.clone(),
        HoleMode::Wide => return applied.clone(),
        HoleMode::Adaptive => {}
    }
    let page_rect = Rect::new(0, 0, page.width, page.height);
    let tolerance = (2.0 * crate::fit::deviation_threshold(crate::fit::page_noise_sigma(page))) as i32;
    let lettering = fitted.ink.dilated_within(BUSY_HOLE_MARGIN, applied.bounds);
    let area = applied.bounds.grown(FLAT_RING + LOCAL_REACH + 1, page.width, page.height);
    let margin = applied.dilated_within(1, page_rect);
    let ring = applied.dilated_within(FLAT_RING, page_rect);
    let in_ring = |x: i64, y: i64| area.contains(x, y) && ring.contains(x, y) && !margin.contains(x, y);
    let luma = |x: i64, y: i64| page.luma16_at(x as u32, y as u32) as i32;
    // Summed tables over the area: ring pixels, and ring pixels whose ring
    // neighbours all sit within the tolerance of them.
    let (w, h) = (area.w as usize, area.h as usize);
    let (mut seen, mut flat) = (vec![0u32; (w + 1) * (h + 1)], vec![0u32; (w + 1) * (h + 1)]);
    for row in 0..h {
        for col in 0..w {
            let (x, y) = (area.x + col as i64, area.y + row as i64);
            let (mut s, mut f) = (0, 0);
            if in_ring(x, y) {
                s = 1;
                let level = luma(x, y);
                let even = (-1..=1).all(|dy| (-1..=1).all(|dx| !in_ring(x + dx, y + dy) || (luma(x + dx, y + dy) - level).abs() <= tolerance));
                f = u32::from(even);
            }
            let at = (row + 1) * (w + 1) + col + 1;
            seen[at] = s + seen[at - 1] + seen[at - w - 1] - seen[at - w - 2];
            flat[at] = f + flat[at - 1] + flat[at - w - 1] - flat[at - w - 2];
        }
    }
    let boxed = |table: &[u32], x: i64, y: i64| {
        let r = LOCAL_REACH as i64;
        let (c0, r0) = ((x - r - area.x).max(0) as usize, (y - r - area.y).max(0) as usize);
        let (c1, r1) = (((x + r + 1 - area.x) as usize).min(w), ((y + r + 1 - area.y) as usize).min(h));
        table[r1 * (w + 1) + c1] + table[r0 * (w + 1) + c0] - table[r0 * (w + 1) + c1] - table[r1 * (w + 1) + c0]
    };
    let mut hole = Mask::empty(applied.bounds);
    for part in parts {
        let whole = part.ring.is_none_or(|(share, _)| share >= HOLE_RING_SHARE);
        for &(x, y) in &part.pixels {
            let near = boxed(&seen, x, y);
            let wide = if near >= LOCAL_SAMPLES { f64::from(boxed(&flat, x, y)) >= HOLE_RING_SHARE * f64::from(near) } else { whole };
            if wide || lettering.contains(x, y) {
                hole.set(x, y, true);
            }
        }
    }
    hole
}

/// The parts of the applied mask whose surround is one tone, and that tone.
///
/// Measured on 12 real pages: on flat paper or flat black the model still invents
/// marks, and a hole near a light edge can come back as a white blob the quality
/// check refuses. Where every side of a part is one tone there is nothing for the
/// model to reconstruct, so the answer there is that tone, read per part so a hole
/// lying half on tone keeps the model's answer on that half.
///
/// The patch keeps the name `lama`: the model ran, and the tone is its answer
/// corrected.
struct FlatParts {
    bounds: Rect,
    part: Vec<u32>,
    tones: Vec<[f32; 3]>,
}

impl FlatParts {
    /// None under [`HoleMode::Tight`], which keeps the model's answer everywhere.
    fn of(applied: &Mask, parts: &[Part], mode: HoleMode) -> FlatParts {
        let bounds = applied.bounds;
        let mut flat = FlatParts { bounds, part: vec![0; bounds.w as usize * bounds.h as usize], tones: Vec::new() };
        if mode == HoleMode::Tight {
            return flat;
        }
        for part in parts {
            let Some((share, tone)) = part.ring else { continue };
            if share < FLAT_RING_SHARE {
                continue;
            }
            flat.tones.push(tone);
            for &(x, y) in &part.pixels {
                flat.part[(y - bounds.y) as usize * bounds.w as usize + (x - bounds.x) as usize] = flat.tones.len() as u32;
            }
        }
        flat
    }

    fn at(&self, x: i64, y: i64) -> Option<[f32; 3]> {
        if !self.bounds.contains(x, y) {
            return None;
        }
        let i = (y - self.bounds.y) as usize * self.bounds.w as usize + (x - self.bounds.x) as usize;
        self.part[i].checked_sub(1).map(|tone| self.tones[tone as usize])
    }
}

/// A held manga-LaMa session.
///
/// Owned by the job, not by the region. Dropping it frees the session and the
/// execution provider's arena together, which is what
/// the "unload LaMa" pressure step
/// actually does.
/// What one of these sessions actually holds, **measured** -
/// which corrects an earlier ~250 MB estimate.
///
/// It is not the weights' 207 MB and reporting the file's size in the
/// loaded-models tab would understate the largest thing this process holds by
/// choice, by a factor of two and a half. [`crate::registry::Basis`] is what
/// keeps this figure apart from the ones that *are* file sizes.
pub const RESIDENT_BYTES: u64 = 510 * 1024 * 1024;

pub struct Inpainter {
    session: crate::accel::Model,
    selection: Selection,
    /// This session's row in [`crate::registry`], which goes when the session
    /// does. Read through [`Inpainter::spent`].
    lease: crate::registry::Lease,
    /// The last few tiles' answers, by their input: a change of a result's
    /// finishing renders the region again without running the model again.
    answers: AnswerMemo<Vec<f32>>,
}

impl Inpainter {
    /// Open the session. Costs seconds; do it once.
    pub fn open(model: &Path, preference: Preference) -> Result<Inpainter, SessionError> {
        let (session, selection) =
            accel::open_session(model, &accel::LAMA, preference, Some(LAMA_INTRA_THREADS))?;
        let lease = crate::registry::register_named(
            crate::registry::Kind::Inpainter,
            crate::registry::Footprint::measured(RESIDENT_BYTES),
            crate::registry::Device::accelerator(selection.accelerator),
            Some("LaMa Manga".into()),
        );
        Ok(Inpainter { session, selection, lease, answers: AnswerMemo::new() })
    }

    /// Whether the holder should give this session back at its next safe
    /// point: somebody asked for it through the loaded-models tab, or nothing
    /// has routed to rung 2 for [`crate::registry::ONNX_IDLE_GRACE`].
    ///
    /// This is the *voluntary* half of what
    /// the pressure ladder does under duress, and
    /// it is deliberately not the same mechanism: the ladder's step is a latch
    /// on a machine that is short of memory, and this is a session nobody is
    /// using on a machine that is fine. So a run may open another one
    /// afterwards, where a surrendered one stays gone.
    pub fn spent(&self) -> bool {
        self.lease.spent()
    }

    /// Give this session up for good, because it has failed in a way another
    /// run on it cannot survive - a reset adapter reported as device-removed.
    /// [`crate::registry::Lease::poison`] states what that costs and why it is
    /// not the same signal as an unload the user asked for.
    ///
    /// It is the third way rung 2 can go away and the only involuntary one:
    /// the ladder surrenders a session under memory pressure, `spent` gives an
    /// idle one back, and this drops one whose device is gone. Like the idle
    /// case and unlike the ladder's, a later run may open another - the machine
    /// is not short of anything, and the next session is built on whatever the
    /// driver came back as.
    pub fn poison(&self) {
        self.lease.poison();
    }

    /// Where the session landed and why - straight into
    /// [`crate::patch::Provenance::execution_provider`].
    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    /// Inpaint one region.
    ///
    /// `page` is the raster the region was decoded from: the whole page for a
    /// single-page project, a decode window on a strip. Its edges are treated
    /// as the true page edge, which is what rule 4's clamp makes true.
    ///
    /// The buffers this allocates are one tile's tensors and one patch. Both
    /// are dropped before the next tile begins and the patch is the result, so
    /// the peak is flat in the number of regions a job contains -
    /// rule 6, and Phase 3's
    /// memory exit.
    pub fn render(&mut self, page: &Raster, fitted: &Fitted) -> Result<Rendered, Error> {
        self.render_with(page, fitted, None, HoleMode::default())
    }

    /// [`Inpainter::render`], with each tile's answer finished by `finishing`
    /// ([`crate::engines::postprocess::settle_with`]) in place of the tone
    /// alignment, and the hole cut by `hole`. `None` aligns tone, as
    /// [`Inpainter::render`] does.
    pub fn render_with(&mut self, page: &Raster, fitted: &Fitted, finishing: Option<Choice>, hole: HoleMode) -> Result<Rendered, Error> {
        self.lease.touch();
        if let Some(decline) = declines(page, fitted) {
            return Err(decline.into());
        }
        let applied = applied_mask(fitted, page.width, page.height);
        let bounds = applied.bounds;
        let managed = crate::engines::model::editable_color(page)?;
        // Read only where they are used: Tight has no flat tone and no ring rule.
        let parts = if hole == HoleMode::Tight { Vec::new() } else { parts_of(page, &applied, &managed) };
        let flat = FlatParts::of(&applied, &parts, hole);
        let hole = hole_of(page, fitted, &applied, &parts, hole);
        let ceiling = ceiling_for(page.depth);
        let reading = answer_mode(page);

        // The running composite. It starts as the page over the patch's bounds,
        // so a tile that reads it before anything has been written reads the
        // page - and every pixel no tile ever reaches keeps the page's value,
        // which is the contract's outside-the-mask half satisfied by
        // construction rather than by a later copy.
        let mut patch = page_crop(page, bounds);

        // The alpha ramp, as three dilations of the fitted mask taken directly
        // rather than composed. Its core is where the model's answer is written
        // whole; the two rings outside it ramp it into the page.
        let ramp = AlphaRamp::new(fitted, page.width, page.height);

        // Which pixels of the patch already hold a model answer. A tile reads
        // these as ordinary context and never repaints them.
        let mut done = Mask::empty(bounds);
        let mut pad = EdgePad::None;
        let mut ran = 0u32;
        let page_rect = Rect::new(0, 0, page.width, page.height);
        // The best-evidenced tile's alignment: the one whose ring held the
        // most samples of the page.
        let mut tone: Option<ToneReport> = None;
        // With a finishing picked: the report of the tile that finished the
        // most pixels, and how many that was.
        let mut matched: Option<(usize, Report)> = None;

        for tile in tiles(bounds) {
            let (crop, commit) = (tile.crop, tile.commit);

            let mut model_mask = vec![0f32; (MODEL_INPUT * MODEL_INPUT) as usize];
            let mut holes = 0usize;
            for y in 0..MODEL_INPUT as i64 {
                for x in 0..MODEL_INPUT as i64 {
                    let (px, py) = (crop.x + x, crop.y + y);
                    // The hole is cut at `done` and **not** at `commit`. A tile
                    // commits only its core, but the mask does not stop at the
                    // core: the text in the hand-off band is inside this crop,
                    // and a hole cut at the commit boundary would leave it in
                    // the tensor as ordinary context, where the model reads it
                    // and continues its strokes back into the part of the tile
                    // that *is* committed. Measured, on the one tiled region of
                    // `page-crowded.png`: cut at the commit boundary, the
                    // written pixels move by up to **254 8-bit levels** when the
                    // pixels under the mask are replaced with noise, against
                    // **0** on every untiled region of the same page. Cut here,
                    // it is 0 on all of them. The removed rung 3 stated the
                    // same rule from the other side and was written first.
                    if hole.contains(px, py) && !done.contains(px, py) {
                        model_mask[(y * MODEL_INPUT as i64 + x) as usize] = 1.0;
                        if commit.contains(px, py) {
                            holes += 1;
                        }
                    }
                }
            }
            // Nothing left for this tile *to commit*. Skipping is not an
            // optimisation detail: a 900×1400 sound effect's mask is sparse, and
            // a model run costs about a second whatever is in the tensor.
            if holes == 0 {
                continue;
            }

            let mut image = vec![0f32; 3 * (MODEL_INPUT * MODEL_INPUT) as usize];
            let plane = (MODEL_INPUT * MODEL_INPUT) as usize;
            for y in 0..MODEL_INPUT as i64 {
                for x in 0..MODEL_INPUT as i64 {
                    let (px, py) = (crop.x + x, crop.y + y);
                    // Edge-replicate, and never reflect: §3 is absolute about
                    // it, and `EdgePad` has no `Reflect` variant to select.
                    let cx = px.clamp(0, page.width as i64 - 1);
                    let cy = py.clamp(0, page.height as i64 - 1);
                    if (cx, cy) != (px, py) {
                        pad = EdgePad::Replicate;
                    }
                    let at = (y * MODEL_INPUT as i64 + x) as usize;
                    // The graph zeroes the hole itself - measured, module note
                    // - but this tile's mask is narrower than the region's and
                    // the surrounding fill is context, so the zeroing is done
                    // here against this tile's own mask.
                    if model_mask[at] != 0.0 {
                        continue;
                    }
                    let rgb = if bounds.contains(cx, cy) {
                        managed.pixel(&patch, (cx - bounds.x) as u32, (cy - bounds.y) as u32)
                    } else { managed.pixel(page, cx as u32, cy as u32) };
                    for c in 0..3 { image[c * plane + at] = rgb[c]; }
                }
            }

            let out = self.answer(image, model_mask)?;
            ran += 1;

            // The answer's drift, learned on the ring outside the applied mask
            // and taken out before the commit - per tile, so the next tile's
            // context is already the page's level. The graph moves the pixels
            // it was not asked about by the same drift it puts in the hole,
            // which is what makes the drift measurable there: the ring is read
            // from the raw answer, and nothing outside the mask is written.
            let answer: Vec<u8> = (0..plane)
                .flat_map(|at| [0, 1, 2].map(|c| (out[c * plane + at] * 255.0).round().clamp(0.0, 255.0) as u8))
                .collect();
            let (_, shown) = crop_samples(page, crop, &managed);
            let opaque = crop_opacity(page, crop, ceiling);
            let site = ToneSite {
                preprocessing: Preprocessing::V2,
                extent: page_rect,
                crop,
                applied: &applied,
                ink: &fitted.ink,
                opaque: opaque.as_deref(),
                shown: &shown,
                reading,
            };
            let aligned = site.measure(&answer);
            let written = commit.intersection(bounds);
            let levels = site.box_levels(&answer, written);
            // The pixels this tile writes, finished as one crop against the
            // page. The rest of the applied mask is no evidence: the page
            // there is the lettering, both in the hand-off band a later tile
            // writes and where an earlier tile wrote, as V2's ring leaves it
            // out. Read as floats, so a 16-bit page keeps its levels.
            let writes = |px: i64, py: i64| {
                commit.contains(px, py) && applied.contains(px, py) && !done.contains(px, py) && ramp.at(px, py) > 0.0
            };
            let finished = finishing.map(|finishing| {
                let (w, h) = (crop.w as usize, crop.h as usize);
                let mut mask = crate::engines::grid::Mask::new(w, h, false);
                let mut on_page = crate::engines::grid::Mask::new(w, h, false);
                let mut page_rgb = vec![[0f32; 3]; w * h];
                for cy in 0..h {
                    for cx in 0..w {
                        let (px, py, i) = (crop.x + cx as i64, crop.y + cy as i64, cy * w + cx);
                        mask.data[i] = writes(px, py);
                        on_page.data[i] = page_rect.contains(px, py)
                            && opaque.as_ref().is_none_or(|opaque| opaque[i])
                            && (mask.data[i] || !applied.contains(px, py));
                        let (sx, sy) = (px.clamp(0, page.width as i64 - 1), py.clamp(0, page.height as i64 - 1));
                        page_rgb[i] = managed.pixel(page, sx as u32, sy as u32);
                    }
                }
                let shown = tone_grids(w, h, reading, |i, c| f64::from(page_rgb[i][c]));
                let answer = tone_grids(w, h, reading, |i, c| f64::from(out[c * plane + i]));
                let evidence = crate::engines::postprocess::Crop { shown: &shown, answer: &answer, mask: &mask, page: &on_page };
                let (settled, report) = crate::engines::postprocess::settle_with(&evidence, finishing);
                let count = mask.data.iter().filter(|&&on| on).count();
                if matched.as_ref().is_none_or(|(most, _)| count > *most) {
                    matched = Some((count, report));
                }
                settled
            });

            for y in 0..MODEL_INPUT as i64 {
                for x in 0..MODEL_INPUT as i64 {
                    let (px, py) = (crop.x + x, crop.y + y);
                    if !commit.contains(px, py) || !applied.contains(px, py) || done.contains(px, py) {
                        continue;
                    }
                    let alpha = ramp.at(px, py);
                    if alpha == 0.0 {
                        continue;
                    }
                    let (lx, ly) = ((px - bounds.x) as u32, (py - bounds.y) as u32);
                    let at = (y * MODEL_INPUT as i64 + x) as usize;
                    let dither = dither_at(page.depth, alpha, px, py, ceiling);
                    let rgb = match &finished {
                        // One grid on a grey page, so grey there.
                        Some(settled) => [0, 1, 2].map(|c| (settled[c.min(settled.len() - 1)].data[at] / 255.0) as f32),
                        None => {
                            let mut rgb = [out[at], out[plane + at], out[2 * plane + at]];
                            if matches!(reading, ColorMode::Gray | ColorMode::GrayAlpha) { rgb = [rgb.iter().sum::<f32>() / 3.0; 3]; }
                            let mean_at = ((py - written.y) * written.w as i64 + (px - written.x)) as usize;
                            for (channel, value) in rgb.iter_mut().enumerate() {
                                let mean = levels[channel.min(levels.len() - 1)][mean_at];
                                *value = aligned.corrected(channel, f64::from(*value), mean) as f32;
                            }
                            rgb
                        }
                    };
                    let rgb = flat.at(px, py).unwrap_or(rgb);
                    commit_srgb(&managed, &mut patch, lx, ly, rgb, alpha, dither)?;
                    done.set(px, py, true);
                }
            }
            // Explicit, and the reason rule 6 is satisfiable at all: the tile's
            // output goes before the next tile's input is built, so the peak is
            // one tile and not two.
            drop(out);
            if finishing.is_none() && tone.as_ref().is_none_or(|best| aligned.samples > best.samples) {
                tone = Some(aligned);
            }
        }

        Ok(Rendered { mask: applied, pixels: patch, pad, tiles: ran, tone, matched: matched.map(|(_, report)| report) })
    }

    /// The model's answer to one tile: the one it gave this same input
    /// before, or a run.
    fn answer(&mut self, image: Vec<f32>, mask: Vec<f32>) -> Result<Vec<f32>, Error> {
        let key = input_digest(&[&f32_bytes(&image), &f32_bytes(&mask)]);
        if let Some(out) = self.answers.get(&key) {
            return Ok(out);
        }
        let out = self.run(image, mask)?;
        self.answers.put(key, out.clone());
        Ok(out)
    }

    fn run(&mut self, image: Vec<f32>, mask: Vec<f32>) -> Result<Vec<f32>, Error> {
        let side = MODEL_INPUT as usize;
        let image = ort::value::Tensor::from_array(([1usize, 3, side, side], image))
            .map_err(|e| Error::Run(e.to_string()))?;
        let mask = ort::value::Tensor::from_array(([1usize, 1, side, side], mask))
            .map_err(|e| Error::Run(e.to_string()))?;
        let _turn = self.session.turn();
        let outputs =
            self.session.run(ort::inputs![image, mask]).map_err(|e| Error::Run(e.to_string()))?;
        let (_, data) =
            outputs[0].try_extract_tensor::<f32>().map_err(|e| Error::Run(e.to_string()))?;
        Ok(data.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::ColorMode;
    use crate::composite::{changed_pixels, composite, permitted_region};
    use crate::constants::{EDIT_MARGIN, ISOLATION_RADIUS};
    use crate::image::BitDepth;
    use crate::fit::{self, Fitted};
    use crate::mask::Rect;
    use crate::image::fixtures;
    use crate::patch::{Engine, Patch, Provenance};
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};

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

    fn fitted_over(page: &Raster, rect: Rect) -> Fitted {
        let seed = Mask::filled(rect);
        fit::fit(page, &seed, 1.0, 0.0, &fit::EdgeMap::none(page.width, page.height), true)
    }

    fn patch_of(mask: Mask, pixels: Raster) -> Patch {
        Patch {
            id: "l1".into(),
            ink: mask.clone(),
            mask,
            pixels,
            order: 0,
            visible: true,
            provenance: Provenance {
                engine: Engine::Lama,
                engine_version: "test".into(),
                model_sha256: None,
                execution_provider: "cpu".into(),
                params_snapshot: serde_json::Value::Null,
                mask_sha256: "0".repeat(64),
                source_sha256: "0".repeat(64),
                cloud: None,
                created: 0,
            },
        }
    }

    // ── the geometry, which needs no model ─────────────────────────────────

    #[test]
    fn a_region_inside_the_model_input_is_one_crop_centred_on_it() {
        // The whole of §3's first case: pad to 512, run, crop back. "Pad" is
        // this centring, and the padding is page pixels because the crop is
        // read from the page.
        assert_eq!(tile_origins(1000, 160), vec![1000 + 80 - 256]);
        assert_eq!(tile_origins(0, 512), vec![0]);
        let tiles = plan(Rect::new(1000, 2000, 160, 290));
        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles[0].w, MODEL_INPUT);
        assert_eq!(tiles[0].h, MODEL_INPUT);
        // Centred: the region has page on all four sides of it.
        assert!(tiles[0].x < 1000 && tiles[0].right() > 1160);
    }

    #[test]
    fn a_larger_region_is_tiled_with_at_least_the_documented_overlap() {
        for extent in [513u32, 600, 640, 896, 900, 1400, 2048] {
            let origins = tile_origins(0, extent);
            assert!(origins.len() >= 2, "{extent} was not tiled");
            assert_eq!(origins[0], 0, "{extent} does not start at the region");
            assert_eq!(
                origins[origins.len() - 1] + MODEL_INPUT as i64,
                extent as i64,
                "{extent} is not covered to its end"
            );
            for pair in origins.windows(2) {
                let overlap = MODEL_INPUT as i64 - (pair[1] - pair[0]);
                assert!(
                    overlap >= TILE_OVERLAP as i64,
                    "{extent}: {overlap} px between tiles, under the documented {TILE_OVERLAP}"
                );
            }
        }
        // A sound effect at the ceiling: five crops each way, and 25 model runs
        // is the cost §3 accepts rather than the one it declines.
        assert_eq!(plan(Rect::new(0, 0, MAX_BOX, MAX_BOX)).len(), 25);
    }

    #[test]
    fn a_box_past_four_model_inputs_is_declined_rather_than_tiled() {
        let page = gray_page(64, 64, |_, _| 200);
        let mut fitted = fitted_over(&page, Rect::new(10, 10, 20, 20));
        // `ink`'s bounds, because that is the box this rung would be run over
        // - see `model::declines`.
        fitted.ink.bounds = Rect::new(0, 0, MAX_BOX + 1, 100);
        let declined = declines(&page, &fitted).expect("a 2049 px box is declined");
        assert_eq!(declined, Decline::TooLarge { w: MAX_BOX + 1, h: 100 });
        assert_eq!(declined.reason_key(), "decline.reason.tooLarge");

        fitted.ink.bounds = Rect::new(0, 0, 100, MAX_BOX);
        assert_eq!(declines(&page, &fitted), None, "exactly 4× is still tiled");
    }

    #[test]
    fn the_modes_and_depths_a_model_rung_may_not_write_into_are_refused() {
        // Indexed and CMYK for the first two; the module note for the third.
        for name in ["indexed-p", "cmyk8"] {
            let page = fixtures::by_name(name).raster;
            assert!(!applies(&page), "{name}");
            let fitted = fitted_over(&page, Rect::new(4, 4, 8, 8));
            assert_eq!(declines(&page, &fitted).unwrap().reason_key(), "decline.reason.unrepresentableMode");
        }
        let bitonal = fixtures::by_name("bitonal").raster;
        assert!(!applies(&bitonal));
        let fitted = fitted_over(&bitonal, Rect::new(4, 4, 8, 8));
        assert_eq!(
            declines(&bitonal, &fitted).unwrap().reason_key(),
            "decline.reason.unrepresentableDepth"
        );

        for name in ["l8", "la8", "rgb8", "rgba8", "l16", "rgb16"] {
            assert!(applies(&fixtures::by_name(name).raster), "{name}");
        }
    }

    #[test]
    fn the_applied_mask_is_the_isolation_cut_and_stays_inside_the_edit_margin() {
        let page = gray_page(96, 96, |x, y| ((x * 7 + y * 3) % 200 + 40) as u8);
        let fitted = fitted_over(&page, Rect::new(36, 36, 24, 24));
        let applied = applied_mask(&fitted, 96, 96);
        let bound = write_bound(&fitted, 96, 96);
        for y in applied.bounds.y..applied.bounds.bottom() {
            for x in applied.bounds.x..applied.bounds.right() {
                if applied.contains(x, y) {
                    assert!(bound.contains(x, y), "({x}, {y}) escaped edit_margin");
                }
            }
        }
        // And the margin is exactly the isolation cut: no rung writes wider.
        assert_eq!(applied, bound);
    }

    /// **Every part of this rung's geometry is measured from the lettering.**
    ///
    /// A flat page, so the fit's zero case ratchets to the end of the candidate
    /// series and `fitted.mask` is the blob [`crate::fit::Fitted::ink`] exists to
    /// keep out of a model's answer. The hole, the applied mask, the write bound
    /// and the ramp must all come off `ink` - one of them left on `mask` would put
    /// the box back, and each is a separate call.
    #[test]
    fn the_hole_the_cut_and_the_ramp_are_all_measured_from_the_lettering() {
        let page = gray_page(200, 200, |_, _| 250);
        let seed = Mask::filled(Rect::new(90, 90, 20, 16));
        let edges = fit::EdgeMap::none(200, 200);
        let fitted = fit::fit(&page, &seed, 1.0, fit::page_noise_sigma(&page), &edges, false);
        // Two rather than four: `ink` carries the hole margin now, so on a
        // 20×16 seed it is 38×34 against the ratchet's 72×68.
        assert!(
            fitted.ink.count() * 2 < fitted.mask.count(),
            "the premise is gone: ink {} against mask {}",
            fitted.ink.count(),
            fitted.mask.count()
        );

        let applied = applied_mask(&fitted, 200, 200);
        assert_eq!(model_hole(&page, &fitted, HoleMode::Tight).unwrap(), fitted.ink, "the tight hole");
        for mode in [HoleMode::Wide, HoleMode::Adaptive] {
            assert_eq!(model_hole(&page, &fitted, mode).unwrap(), applied, "{mode:?} on flat paper: the whole applied mask");
        }
        assert_eq!(applied, fitted.ink.dilated(ISOLATION_RADIUS, 200, 200), "the isolation cut");
        assert_eq!(
            write_bound(&fitted, 200, 200),
            fitted.ink.dilated(EDIT_MARGIN, 200, 200),
            "the write bound"
        );

        // The ramp: one inside the lettering, zero everywhere the applied mask
        // does not reach, and never non-zero out in the growth.
        let ramp = AlphaRamp::new(&fitted, 200, 200);
        assert_eq!(ramp.at(99, 97), 1.0);
        let mut beyond = 0;
        for y in fitted.mask.bounds.y..fitted.mask.bounds.bottom() {
            for x in fitted.mask.bounds.x..fitted.mask.bounds.right() {
                if !applied.contains(x, y) {
                    assert_eq!(ramp.at(x, y), 0.0, "({x}, {y}) is ramped outside the cut");
                    beyond += 1;
                }
            }
        }
        assert!(beyond > 0, "the growth and the cut are the same set; rewrite this test");
    }

    #[test]
    fn the_feather_is_two_hard_rings_and_nothing_beyond_them() {
        // A σ=1 Gaussian would still be putting alpha 2–3 px past this.
        assert_eq!(ISOLATION_FEATHER, 2);
        assert_eq!(ISOLATION_RADIUS - ISOLATION_FEATHER, 3);
        const { assert!(ISOLATION_RADIUS <= EDIT_MARGIN) };
    }

    #[test]
    fn the_dither_is_deterministic_and_confined_to_the_isolation_boundary() {
        let ceiling = ceiling_for(BitDepth::Sixteen);
        // Nothing inside the mask, nothing outside it, and nothing at 8 bits.
        assert_eq!(dither_at(BitDepth::Sixteen, 1.0, 3, 4, ceiling), 0.0);
        assert_eq!(dither_at(BitDepth::Sixteen, 0.0, 3, 4, ceiling), 0.0);
        assert_eq!(dither_at(BitDepth::Eight, 0.5, 3, 4, 255.0), 0.0);
        // One model quantum wide, and the same at the same coordinates.
        let a = dither_at(BitDepth::Sixteen, 0.5, 7, 9, ceiling);
        assert_eq!(a, dither_at(BitDepth::Sixteen, 0.5, 7, 9, ceiling));
        assert!(a.abs() <= ceiling / 255.0 / 2.0, "{a} is wider than one model quantum");
    }

    // ── the model, when it is there ────────────────────────────────────────

    /// The weights are not in the repository
    /// and the ONNX Runtime is fetched rather than bundled, so a machine
    /// that has run neither fetch script cannot run these and should not be
    /// told it has broken something. The session is shared across them because
    /// building one costs seconds and holding one is the design.
    fn inpainter() -> Option<&'static Mutex<Inpainter>> {
        static HELD: OnceLock<Option<Mutex<Inpainter>>> = OnceLock::new();
        HELD.get_or_init(|| {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
            let model = root.join("models/lama-manga.onnx");
            if !model.exists() {
                eprintln!("skipped: no lama-manga.onnx - run scripts/fetch-models.sh");
                return None;
            }
            let runtime = crate::runtime::find(None).ok().or_else(|| {
                let dev = root
                    .join("runtimes/onnxruntime-osx-arm64-1.28.0/lib")
                    .join(crate::runtime::DYLIB_NAME);
                dev.exists().then_some(dev)
            })?;
            if crate::runtime::load(&runtime).is_err() {
                eprintln!("skipped: the ONNX Runtime would not load");
                return None;
            }
            // CPU, not `Automatic`: these assert the engine's behaviour, and
            // `Preference::CpuOnly` is the setting the golden tests
            // pin for exactly that reason.
            match Inpainter::open(&model, Preference::CpuOnly) {
                Ok(held) => Some(Mutex::new(held)),
                Err(e) => {
                    eprintln!("skipped: the session would not build: {e}");
                    None
                }
            }
        })
        .as_ref()
    }

    /// **The wide hole reaches the lettering's soft edge, and the written set does
    /// not grow to do it.** A mask on the stroke leaves its fringe a few pixels out.
    /// On flat paper the adaptive hole is the same.
    #[test]
    fn the_hole_takes_the_fringe_the_mask_stopped_short_of() {
        let page = gray_page(200, 200, |_, _| 250);
        let seed = Mask::filled(Rect::new(90, 90, 20, 16));
        let fitted = fit::fit(&page, &seed, 1.0, 0.0, &fit::EdgeMap::none(200, 200), true);
        assert_eq!(fitted.ink, seed, "a stored mask is authoritative, so it has no margin of its own");
        for mode in [HoleMode::Wide, HoleMode::Adaptive] {
            let hole = model_hole(&page, &fitted, mode).unwrap();
            for reach in 1..=ISOLATION_RADIUS as i64 {
                assert!(hole.contains(89 - reach + 1, 95), "{mode:?}: {reach} px out is not in the hole");
            }
            assert!(!hole.contains(89 - ISOLATION_RADIUS as i64, 95), "{mode:?}: the hole passed the applied mask");
            assert_eq!(hole, applied_mask(&fitted, 200, 200));
        }
    }

    /// **Beside art the adaptive hole stops at the lettering.** One mark half on paper and
    /// half on hatching: the margin on the paper side is in the hole, the margin on
    /// the hatching side is drawing and is not, and the written set is the same.
    #[test]
    fn the_hole_keeps_its_margin_off_art() {
        let page = gray_page(200, 200, |x, y| if x >= 100 && y % 3 == 0 { 30 } else { 250 });
        let seed = Mask::filled(Rect::new(70, 90, 60, 16));
        let fitted = fit::fit(&page, &seed, 1.0, 0.0, &fit::EdgeMap::none(200, 200), true);
        let hole = model_hole(&page, &fitted, HoleMode::Adaptive).unwrap();
        let applied = applied_mask(&fitted, 200, 200);
        assert!(hole.contains(67, 98), "the margin on paper is not in the hole");
        assert!(!hole.contains(132, 98), "the margin on the hatching is in the hole");
        assert!(hole.contains(129, 98), "the lettering on the hatching is not in the hole");
        for y in applied.bounds.y..applied.bounds.bottom() {
            for x in applied.bounds.x..applied.bounds.right() {
                assert!(!hole.contains(x, y) || applied.contains(x, y), "({x}, {y}) is past the applied mask");
            }
        }
    }

    /// The defect the wide hole exists for, on the real model: a soft-edged stroke
    /// with a mask that stops inside the edge. Cut to the mask (Tight), the model
    /// reads the fringe as an outline and fills the hole dark, and
    /// [`crate::quality::assess`] declines it. Wide and Adaptive both clear it.
    #[test]
    fn a_soft_edged_stroke_does_not_come_back_in_the_hole() {
        let Some(held) = inpainter() else { return };
        // A cross on flat paper, its edge falling off over 4 px.
        let depth = |x: u32, y: u32| -> u8 {
            let bar = |v: u32, from: i64, to: i64| {
                let v = v as i64;
                (from - v).max(v - to).max(0)
            };
            let off = bar(x, 150, 162).max(bar(y, 80, 220)).min(bar(y, 150, 162).max(bar(x, 80, 220)));
            match off { 0 => 20, 1 => 70, 2 => 130, 3 => 190, 4 => 230, _ => 250 }
        };
        let page = gray_page(312, 312, depth);
        let mut seed = Mask::empty(Rect::new(70, 70, 172, 172));
        for y in 0..312u32 {
            for x in 0..312u32 {
                if depth(x, y) == 20 {
                    seed.set(x as i64, y as i64, true);
                }
            }
        }
        let fitted = fit::fit(&page, &seed, 1.0, 0.0, &fit::EdgeMap::none(312, 312), true);
        for mode in [HoleMode::Wide, HoleMode::Adaptive] {
            let rendered = held.lock().unwrap().render_with(&page, &fitted, None, mode).expect("the region rendered");
            let verdict = crate::quality::assess(&page, &rendered.mask, &rendered.pixels, 0.0);
            // Cut to the stroke this read 40707. Not "not declined": a surround this
            // flat exists only on a synthetic page, and it floors the metric at half
            // an 8-bit level, which the model's own grain sits right at.
            assert!(verdict.interior_edge_energy < 4000.0, "{mode:?}: {verdict:?}");
            let b = rendered.mask.bounds;
            let mut dark = 0;
            for y in b.y..b.bottom() {
                for x in b.x..b.right() {
                    if seed.contains(x, y) && rendered.pixels.sample8((x - b.x) as u32, (y - b.y) as u32, 0) < 200 {
                        dark += 1;
                    }
                }
            }
            assert!(dark * 100 < seed.count(), "{mode:?}: {dark} of {} stroke pixels came back dark", seed.count());
        }
    }

    /// A real screentone page, which is what this rung exists for. Rungs 0 and
    /// 1 have synthetic fixtures because their arithmetic is checkable by hand;
    /// a model's is not.
    fn screentone_page() -> Option<Raster> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/pages/page-screentone.png");
        let bytes = std::fs::read(path).ok()?;
        let mut page = crate::image::decode(&bytes).ok()?;
        // Lettering in the region most tests here mask. The paper there is flat,
        // and under Wide or Adaptive a flat part of a hole comes back as the
        // paper, which is no change.
        for y in 830..860 {
            for x in 640..720 {
                for channel in 0..page.mode.samples() {
                    page.set_sample(x, y, channel, 0);
                }
            }
        }
        Some(page)
    }

    /// **A picked finishing takes the place of each tile's tone alignment.**
    /// The patch is still written only through the applied mask, and the
    /// report names the finishing that ran.
    #[test]
    fn a_picked_finishing_replaces_the_tone_alignment() {
        use crate::engines::postprocess::{Choice, Finishing};
        let Some(held) = inpainter() else { return };
        let Some(page) = screentone_page() else { return };
        let mut held = held.lock().unwrap();
        let fitted = fitted_over(&page, Rect::new(600, 800, 160, 90));
        let aligned = held.render(&page, &fitted).expect("the region rendered");
        let choice = Choice { finishing: Finishing::Copy, grayscale: false };
        let finished = held.render_with(&page, &fitted, Some(choice), HoleMode::default()).expect("the region rendered");
        assert!(aligned.tone.is_some() && aligned.matched.is_none());
        assert!(finished.tone.is_none());
        assert_eq!(finished.matched.as_ref().map(|report| report.choice), Some(choice));
        assert_eq!(finished.mask, aligned.mask);
        let b = finished.mask.bounds;
        for y in b.y..b.bottom() {
            for x in b.x..b.right() {
                if !finished.mask.contains(x, y) {
                    let (lx, ly) = ((x - b.x) as u32, (y - b.y) as u32);
                    assert_eq!(finished.pixels.sample8(lx, ly, 0), page.sample8(x as u32, y as u32, 0), "({x}, {y}) is outside the mask");
                }
            }
        }
    }

    /// **A tiled region's lettering is no evidence of its page.** Paper with
    /// solid black lettering across three tiles: each tile's crop holds the
    /// lettering a later tile writes and the page under what an earlier tile
    /// wrote. Read as page, it pulls the finished fill towards black; left out,
    /// the fill is paper, as the tone alignment's is. Both this module's
    /// finishing and the ported local tone (Manga Cleaner Reproduction's),
    /// which has no guard of its own against a drawing change.
    #[test]
    fn a_tiled_region_is_finished_against_the_page_and_not_its_lettering() {
        use crate::engines::postprocess::{Choice, Finishing};
        let Some(held) = inpainter() else { return };
        let mut held = held.lock().unwrap();
        const PAPER: u8 = 228;
        let page = gray_page(1500, 800, |x, y| {
            let glyph = (120..1380).contains(&x) && (x - 120) % 80 < 48 && (340..460).contains(&y);
            if glyph { 18 } else { PAPER }
        });
        let seed = Rect::new(100, 300, 1300, 200);
        let fitted = fitted_over(&page, seed);
        let off = |rendered: &Rendered| {
            let b = rendered.mask.bounds;
            let (mut sum, mut n, mut worst) = (0u64, 0u64, 0u8);
            for y in seed.y + 8..seed.bottom() - 8 {
                for x in seed.x + 8..seed.right() - 8 {
                    let d = rendered.pixels.sample8((x - b.x) as u32, (y - b.y) as u32, 0).abs_diff(PAPER);
                    (sum, n, worst) = (sum + u64::from(d), n + 1, worst.max(d));
                }
            }
            (sum as f64 / n as f64, worst)
        };
        let aligned = held.render(&page, &fitted).expect("the region rendered");
        assert!(aligned.tiles >= 3, "{} tiles", aligned.tiles);
        let (aligned_mean, _) = off(&aligned);
        for finishing in [Finishing::Copy, Finishing::LocalTone] {
            let finished = held.render_with(&page, &fitted, Some(Choice { finishing, grayscale: false }), HoleMode::default()).expect("the region rendered");
            let (mean, worst) = off(&finished);
            eprintln!("{finishing:?}: mean {mean:.2}, worst {worst}; aligned mean {aligned_mean:.2}");
            assert!(mean <= aligned_mean + 1.0 && worst <= 24, "{finishing:?}: mean {mean:.2}, worst {worst}, aligned {aligned_mean:.2}");
        }
    }

    /// **A 16-bit page keeps its levels under a finishing.** The answer is
    /// finished as floats, so where it is written whole it is not cut to the
    /// 256 levels of 8 bits (multiples of 257).
    #[test]
    fn a_finished_16_bit_patch_keeps_more_than_8_bits() {
        use crate::engines::postprocess::Choice;
        let Some(held) = inpainter() else { return };
        let mut held = held.lock().unwrap();
        let (w, h) = (900u32, 700u32);
        let level = |x: u32, y: u32| -> u16 {
            if (300..420).contains(&x) && (320..380).contains(&y) { 3000 } else { 52000 + (y * 7 + x * 3) as u16 % 900 }
        };
        let data = (0..h).flat_map(|y| (0..w).flat_map(move |x| level(x, y).to_be_bytes())).collect();
        let page = Raster { depth: BitDepth::Sixteen, data, ..gray_page(w, h, |_, _| 0) };
        let seed = Rect::new(280, 300, 160, 100);
        let fitted = fitted_over(&page, seed);
        let finished = held.render_with(&page, &fitted, Some(Choice::default()), HoleMode::default()).expect("the region rendered");
        assert_eq!(finished.pixels.depth, BitDepth::Sixteen);
        let b = finished.mask.bounds;
        let core: Vec<u16> = (seed.y + 12..seed.bottom() - 12)
            .flat_map(|y| (seed.x + 12..seed.right() - 12).map(move |x| (x, y)))
            .map(|(x, y)| finished.pixels.sample((x - b.x) as u32, (y - b.y) as u32, 0))
            .collect();
        let finer = core.iter().filter(|&&v| v % 257 != 0).count();
        assert!(finer * 2 > core.len(), "{finer} of {} core samples are off the 8-bit grid", core.len());
    }

    #[test]
    fn a_grey_page_stored_as_rgba_gets_a_grey_patch() {
        let Some(held) = inpainter() else { return };
        let Some(grey) = screentone_page() else { return };
        let mut held = held.lock().unwrap();
        // The same page as a scanner that writes RGBA stores it.
        let data = (0..grey.height)
            .flat_map(|y| (0..grey.width).map(move |x| (x, y)))
            .flat_map(|(x, y)| { let v = grey.sample8(x, y, 0); [v, v, v, 255] })
            .collect();
        let page = Raster { mode: ColorMode::Rgba, depth: BitDepth::Eight, icc: None, data, ..grey };

        let fitted = fitted_over(&page, Rect::new(600, 800, 160, 90));
        let rendered = held.render(&page, &fitted).expect("the region rendered");
        assert_eq!(rendered.pixels.mode, ColorMode::Rgba);
        // Exactly grey, not only within the neutral tolerance: on this page
        // the model's own answer is 2 levels apart at worst, and 13 on a real
        // RGBA scan (Deli Health chapter 11 page 5).
        let pixels = &rendered.pixels;
        let parted = (0..pixels.height)
            .flat_map(|y| (0..pixels.width).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let [r, g, b] = [0, 1, 2].map(|c| pixels.sample(x, y, c));
                r != g || g != b
            })
            .count();
        assert_eq!(parted, 0, "the colour model's chroma reached a grey page");
    }

    /// **The fill lands on the page's grey level.** Left as it came back, the
    /// model's answer on a flat light grey sits 2.8 levels under the page at
    /// 220 and 2.6 at 245; the ring around the applied mask shows the same
    /// drift, and the curve learned there takes it out.
    #[test]
    fn a_grey_pages_fill_is_aligned_to_the_page_level() {
        let Some(held) = inpainter() else { return };
        let mut held = held.lock().unwrap();
        for level in [220u8, 245] {
            let page = gray_page(900, 900, |_, _| level);
            let fitted = fitted_over(&page, Rect::new(380, 400, 160, 90));
            let rendered = held.render(&page, &fitted).expect("the region rendered");

            let tone = rendered.tone.as_ref().expect("LaMa reports its tone");
            assert_eq!(tone.abstained, None, "{level}: the alignment abstained");
            let added = tone.correction(0, f64::from(level));
            assert!(added >= 1.5, "{level}: the ring showed no drift to take out ({added:.2} levels)");

            let pixels = &rendered.pixels;
            let (mut sum, mut count) = (0.0, 0.0);
            for y in 0..pixels.height {
                for x in 0..pixels.width {
                    let (px, py) = (rendered.mask.bounds.x + x as i64, rendered.mask.bounds.y + y as i64);
                    if rendered.mask.contains(px, py) {
                        sum += f64::from(pixels.sample(x, y, 0));
                        count += 1.0;
                    }
                }
            }
            let mean = sum / count;
            assert!(
                (mean - f64::from(level)).abs() <= 1.0,
                "{level}: the fill averages {mean:.2}, more than a level off the page"
            );
        }
    }

    #[test]
    fn a_masked_region_comes_back_filled_in_the_pages_own_mode_and_depth() {
        let Some(held) = inpainter() else { return };
        let Some(page) = screentone_page() else { return };
        let mut held = held.lock().unwrap();

        let fitted = fitted_over(&page, Rect::new(600, 800, 160, 90));
        let rendered = held.render(&page, &fitted).expect("the region rendered");

        assert_eq!(rendered.tiles, 1, "a 160×90 region is one model run");
        assert_eq!(rendered.pixels.mode, page.mode, "a grayscale page came back in another mode");
        assert_eq!(rendered.pixels.depth, page.depth);
        assert_eq!(rendered.pixels.width, rendered.mask.bounds.w);
        assert_eq!(rendered.pixels.height, rendered.mask.bounds.h);
        assert_eq!(rendered.pad, EdgePad::None, "an interior region needed no padding");

        // Something actually happened inside the mask.
        let mut moved = 0;
        for y in 0..rendered.pixels.height {
            for x in 0..rendered.pixels.width {
                let (px, py) = (rendered.mask.bounds.x + x as i64, rendered.mask.bounds.y + y as i64);
                if rendered.mask.contains(px, py)
                    && rendered.pixels.sample(x, y, 0) != page.sample(px as u32, py as u32, 0)
                {
                    moved += 1;
                }
            }
        }
        assert!(moved > 100, "the inpainter returned the page: {moved} samples moved");
    }

    /// The measurement in the module note, turned into an assertion: the model
    /// alters pixels it was not asked about, and the only thing that keeps
    /// the contract true is that this
    /// rung writes through a mask.
    #[test]
    fn nothing_outside_the_isolation_cut_moves_even_though_the_model_moved_it() {
        let Some(held) = inpainter() else { return };
        let Some(page) = screentone_page() else { return };
        let mut held = held.lock().unwrap();

        let fitted = fitted_over(&page, Rect::new(600, 800, 160, 90));
        let rendered = held.render(&page, &fitted).expect("the region rendered");
        let bound = write_bound(&fitted, page.width, page.height);

        for y in 0..rendered.pixels.height {
            for x in 0..rendered.pixels.width {
                let (px, py) = (rendered.mask.bounds.x + x as i64, rendered.mask.bounds.y + y as i64);
                if !rendered.mask.contains(px, py) {
                    assert_eq!(
                        rendered.pixels.sample(x, y, 0),
                        page.sample(px as u32, py as u32, 0),
                        "({px}, {py}) is outside the applied mask and moved"
                    );
                }
            }
        }

        let patch = patch_of(rendered.mask.clone(), rendered.pixels.clone());
        let out = composite(&page, std::slice::from_ref(&patch)).unwrap();
        let permitted = permitted_region(std::slice::from_ref(&patch), page.width, page.height);
        let changed = changed_pixels(&page, &out);
        assert!(!changed.is_empty(), "the composite wrote nothing");
        for (x, y) in changed {
            assert!(permitted.contains(x as i64, y as i64), "({x}, {y}) escaped the contract");
            assert!(bound.contains(x as i64, y as i64), "({x}, {y}) escaped edit_margin");
        }
    }

    /// **The reported defect, as an assertion**: the model's answer does not
    /// reach the growth the fit settled on.
    ///
    /// The test above runs on a *manual* fit, where the mask and the written set
    /// are the same thing by design, so it cannot see this at all. Here the fit
    /// runs its candidate search on a real page and the mask ends up several
    /// times the written set - and every pixel of the difference has to come back
    /// byte-identical, because the whole visible artefact was those pixels
    /// arriving at a tone the model had picked for them.
    #[test]
    fn the_growth_between_the_written_set_and_the_fitted_mask_is_byte_identical() {
        let Some(held) = inpainter() else { return };
        let Some(page) = screentone_page() else { return };
        let mut held = held.lock().unwrap();

        // A glyph-shaped seed, the way the detector's segmentation arrives: the
        // dark pixels inside one balloon's box. A filled rectangle would not do
        // - its border sits on the art, every candidate is refused, and the
        // fallback hands back the rectangle itself with nothing grown at all.
        let box_ = Rect::new(841, 1456, 76, 185);
        let mut seed = Mask::empty(box_);
        for y in box_.y..box_.bottom() {
            for x in box_.x..box_.right() {
                if page.sample(x as u32, y as u32, 0) < 128 {
                    seed.set(x, y, true);
                }
            }
        }
        assert!(seed.count() > 1_000, "no lettering under the box");
        let edges = fit::EdgeMap::sobel(&page);
        let fitted =
            fit::fit(&page, &seed, 2.344, fit::page_noise_sigma(&page), &edges, false);
        assert!(
            fitted.ink.count() * 3 < fitted.mask.count(),
            "the premise is gone: ink {} against mask {}",
            fitted.ink.count(),
            fitted.mask.count()
        );

        let rendered = held.render(&page, &fitted).expect("the region rendered");
        let patch = patch_of(rendered.mask.clone(), rendered.pixels.clone());
        let out = composite(&page, std::slice::from_ref(&patch)).unwrap();

        let mut moved_outside = 0;
        let mut moved_in_the_growth = 0;
        for (x, y) in changed_pixels(&page, &out) {
            if !rendered.mask.contains(x as i64, y as i64) {
                moved_outside += 1;
            }
            if fitted.mask.contains(x as i64, y as i64)
                && !rendered.mask.contains(x as i64, y as i64)
            {
                moved_in_the_growth += 1;
            }
        }
        assert_eq!(moved_outside, 0, "pixels moved outside the applied mask");
        assert_eq!(moved_in_the_growth, 0, "the model's answer reached into the growth");

        // And the growth really is a large set of pixels that were spared, rather
        // than an empty difference that would make the two counts trivial.
        let spared = (fitted.mask.bounds.y..fitted.mask.bounds.bottom())
            .flat_map(|y| (fitted.mask.bounds.x..fitted.mask.bounds.right()).map(move |x| (x, y)))
            .filter(|(x, y)| fitted.mask.contains(*x, *y) && !rendered.mask.contains(*x, *y))
            .count();
        assert!(spared > 10_000, "only {spared} px were spared");
    }

    /// `ScatterND` is the CPU-EP non-determinism case. That operator was in
    /// MI-GAN's graph and is not in this one's, so the rung this ladder kept
    /// is the determinable one. Confirmed rather than assumed,
    /// which is why the test outlived the engine it was distinguishing this one
    /// from.
    #[test]
    fn the_same_region_twice_is_the_same_pixels_twice() {
        let Some(held) = inpainter() else { return };
        let Some(page) = screentone_page() else { return };
        let mut held = held.lock().unwrap();

        let fitted = fitted_over(&page, Rect::new(600, 800, 160, 90));
        let first = held.render(&page, &fitted).expect("the region rendered");
        let second = held.render(&page, &fitted).expect("the region rendered again");
        assert_eq!(first.pixels.data, second.pixels.data);
    }

    #[test]
    fn a_region_wider_than_the_model_is_tiled_and_still_writes_only_through_its_mask() {
        let Some(held) = inpainter() else { return };
        let Some(page) = screentone_page() else { return };
        let mut held = held.lock().unwrap();

        // A sound effect: past 512 in both directions, so the tiling runs in
        // both, and small enough that four model runs is a test rather than a
        // wait.
        let fitted = fitted_over(&page, Rect::new(400, 600, 700, 620));
        let rendered = held.render(&page, &fitted).expect("the region rendered");
        assert_eq!(rendered.tiles, 4, "a 700×620 region is a 2×2 tiling");

        for y in 0..rendered.pixels.height {
            for x in 0..rendered.pixels.width {
                let (px, py) = (rendered.mask.bounds.x + x as i64, rendered.mask.bounds.y + y as i64);
                if !rendered.mask.contains(px, py) {
                    assert_eq!(
                        rendered.pixels.sample(x, y, 0),
                        page.sample(px as u32, py as u32, 0),
                        "({px}, {py}) is outside the applied mask and moved"
                    );
                }
            }
        }

        // The join between two tiles is not a step. Measured against the
        // region's own local variation rather than an absolute number, because
        // screentone has plenty of legitimate 200-level steps in it.
        let seam = tile_origins(rendered.mask.bounds.x, rendered.mask.bounds.w)[1]
            + (MODEL_INPUT - TILE_HANDOFF) as i64;
        let read = |px: i64, py: i64| -> f64 {
            rendered.pixels.sample(
                (px - rendered.mask.bounds.x) as u32,
                (py - rendered.mask.bounds.y) as u32,
                0,
            ) as f64
        };
        let (mut across, mut along, mut count) = (0.0, 0.0, 0u32);
        for py in rendered.mask.bounds.y + 8..rendered.mask.bounds.bottom() - 8 {
            if !fitted.mask.contains(seam, py) || !fitted.mask.contains(seam - 1, py) {
                continue;
            }
            across += (read(seam, py) - read(seam - 1, py)).abs();
            along += (read(seam - 1, py) - read(seam - 2, py)).abs();
            count += 1;
        }
        assert!(count > 0, "the seam column is not inside the mask");
        assert!(
            across <= along * 2.0 + count as f64,
            "the tile join is a step: {:.1} across against {:.1} beside it",
            across / count as f64,
            along / count as f64
        );
    }

    /// **The tile hand-off does not let the text back in.**
    ///
    /// A tile commits only its core, and for a while this rung cut the hole at
    /// that core as well. The mask does not stop there: the text in the
    /// hand-off band sits inside the same 512² crop, so a hole cut at the commit
    /// boundary left it in the tensor as ordinary context, where the model read
    /// it and continued its strokes back into the part of the tile that *was*
    /// committed. On the one tiled region of `fixtures/pages/page-crowded.png`
    /// that showed up as 254 8-bit levels of dependence on pixels the mask
    /// exists to erase, and in the picture as a legible `口` inside a cleaned
    /// balloon. Every untiled region of the same page measured 0, which is why
    /// it survived the tests that existed: they were all single-tile.
    ///
    /// This was the removed rung 3's rule first, and it is asserted the way
    /// that rung asserted it - render the region, render it again over
    /// violently scribbled input, and require the written pixels to be the same
    /// pixels.
    #[test]
    fn the_text_under_the_mask_does_not_reach_the_fill_across_a_tile_join() {
        let Some(held) = inpainter() else { return };
        let Some(page) = screentone_page() else { return };
        let mut held = held.lock().unwrap();

        // Past 512 in one dimension, so the region is two tiles and there is a
        // hand-off band; small in the other, so the test is two model runs
        // twice rather than eight.
        let region = Rect::new(300, 700, 600, 200);
        let fitted = fitted_over(&page, region);
        let from_page = held.render(&page, &fitted).expect("the region rendered");
        assert_eq!(from_page.tiles, 2, "the region was not tiled, so nothing is being tested");

        let mut scribbled = page.clone();
        for y in region.y..region.bottom() {
            for x in region.x..region.right() {
                if fitted.mask.contains(x, y) {
                    let v = if (x + y) % 3 == 0 { 0 } else { 255 };
                    scribbled.set_sample(x as u32, y as u32, 0, v);
                }
            }
        }
        let from_scribble = held.render(&scribbled, &fitted).expect("the region rendered");

        assert_eq!(from_page.mask, from_scribble.mask, "the two renders covered different sets");
        let (mut differing, mut worst) = (0u32, 0i64);
        for y in 0..from_page.pixels.height {
            for x in 0..from_page.pixels.width {
                let (px, py) =
                    (from_page.mask.bounds.x + x as i64, from_page.mask.bounds.y + y as i64);
                if !from_page.mask.contains(px, py) {
                    continue;
                }
                let d = (from_page.pixels.sample(x, y, 0) as i64
                    - from_scribble.pixels.sample(x, y, 0) as i64)
                    .abs();
                if d != 0 {
                    differing += 1;
                }
                worst = worst.max(d);
            }
        }
        assert_eq!(
            differing, 0,
            "{differing} written samples depend on what was under the mask, worst {worst} - the \
             hole is being cut at the commit boundary rather than at the settled fill"
        );
    }

    /// A page-edge region. §3 and rule 4 both say edge-replicate and never
    /// reflect, and the record of which was used goes into `params_snapshot`.
    #[test]
    fn a_region_at_the_page_edge_is_padded_by_replication_and_says_so() {
        let Some(held) = inpainter() else { return };
        let Some(page) = screentone_page() else { return };
        let mut held = held.lock().unwrap();

        let fitted = fitted_over(&page, Rect::new(0, 0, 120, 80));
        let rendered = held.render(&page, &fitted).expect("the region rendered");
        assert_eq!(rendered.pad, EdgePad::Replicate);
        assert_eq!(rendered.pad.as_str(), "edge-replicate");
    }

    #[test]
    fn a_sixteen_bit_page_is_inpainted_at_sixteen_bits() {
        let Some(held) = inpainter() else { return };
        let mut held = held.lock().unwrap();

        let page = fixtures::by_name("l16").raster;
        let fitted = fitted_over(&page, Rect::new(20, 16, 12, 10));
        let rendered = held.render(&page, &fitted).expect("the region rendered");
        assert_eq!(rendered.pixels.depth, BitDepth::Sixteen);
        assert_eq!(rendered.pixels.mode, ColorMode::Gray);
        // A patch quantised through 8 bits would have every sample a multiple
        // of 257. u16 → f32 directly, never via u8.
        let mut off_the_eight_bit_grid = 0;
        for y in 0..rendered.pixels.height {
            for x in 0..rendered.pixels.width {
                if rendered.pixels.sample(x, y, 0) % 257 != 0 {
                    off_the_eight_bit_grid += 1;
                }
            }
        }
        assert!(off_the_eight_bit_grid > 0, "the patch quantised to 8 bits");
    }

    #[test]
    fn an_alpha_channel_is_copied_rather_than_inpainted() {
        let Some(held) = inpainter() else { return };
        let mut held = held.lock().unwrap();

        let page = fixtures::by_name("rgba8").raster;
        let fitted = fitted_over(&page, Rect::new(20, 16, 12, 10));
        let rendered = held.render(&page, &fitted).expect("the region rendered");
        for y in 0..rendered.pixels.height {
            for x in 0..rendered.pixels.width {
                let (px, py) = (rendered.mask.bounds.x as u32 + x, rendered.mask.bounds.y as u32 + y);
                assert_eq!(
                    rendered.pixels.sample(x, y, 3),
                    page.sample(px, py, 3),
                    "alpha changed at {px},{py}"
                );
            }
        }
    }
}

