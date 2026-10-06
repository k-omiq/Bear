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
use crate::constants::LAMA_INTRA_THREADS;
use crate::fit::Fitted;
use crate::image::{ColorMode, Raster};
use crate::mask::Mask;

pub use crate::engines::model::{
    Decline, Error, ISOLATION_FEATHER, MAX_BOX, MODEL_INPUT, Rendered, TILE_HANDOFF, TILE_OVERLAP,
    TILE_STRIDE, applied_mask, plan, tile_origins, write_bound,
};
use crate::engines::model::{AlphaRamp, answer_mode, ceiling_for, dither_at, page_crop, commit_srgb, tiles};
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

/// The hole handed to the model: [`crate::fit::Fitted::ink`] itself, and
/// **nothing added here**.
///
/// What `ink` *is* changed under this function, and the measurement below is
/// what it changed against. `ink` used to be the stroke-tight set - the seed
/// grown by the fit's first step - and on real scanned pages that hole left
/// glyph-shaped marks in every balloon the model was given, while the retry
/// button, whose hole is the stored applied mask (that set grown by the
/// isolation radius), came back clean. Ten regions of one page, before/first/
/// retry side by side, `spikes/gate-probe --lama`. So [`crate::fit`] now grows
/// `ink` by [`crate::constants::MODEL_HOLE_MARGIN`] where no strong edge
/// stops it, and the table below stands as what was true of the synthetic
/// fixtures it was measured on. This function still adds nothing, for the
/// reason the next paragraph gives.
///
/// It reads `ink` rather than `mask` because the hole is the set this rung
/// writes, [`applied_mask`] is that set plus the isolation ring, and a hole that
/// did not match would either leave text inside the written region or zero page
/// the composite then hands straight back. The measurement below is unaffected
/// and points the same way - the whole of it says that a hole **wider** than the
/// set being written is worse, and `ink` is the narrowest statement of that set
/// this pipeline has.
///
/// Stated as a function rather than left implicit at the one place that reads
/// it, because the opposite is what the removed rung 3 did - it grew its hole
/// by a fixed margin - and the difference is worth having written down rather
/// than inferred from an absence.
///
/// It is not a widened hole because a **measurement says a widened hole is
/// worse**, and it is worth being exact about what was asked and what came
/// back, since the reasoning that suggests widening is sound and only the
/// premise is wrong.
///
/// The suggestion is that an inpainter generates content consistent with the
/// boundary it is given, so a hole with the silhouette of the glyphs invites
/// the model to draw a glyph - which would make the leftover stroke-shaped
/// marks this rung produces the hole's own fault. Seven geometries were run
/// against one held session on each of the eleven regions of
/// `fixtures/pages/page-crowded.png` that route here: this one, the fitted mask
/// grown by 8 px and by 16 px, its filled convex hull, its filled bounding box,
/// and two edge-constrained variants that flood outward from the mask and stop
/// at [`crate::fit::EdgeMap`]'s strong edges. Counting the pixels left more than
/// 64 levels under the region's own paper inside the written set, against 1,143
/// to 4,736 before the edit:
///
/// | Region | this hole | ⊕ 8 px |
/// |---|---|---|
/// | r4 | 642 | 603 |
/// | r10 | **229** | 897 |
/// | r14 | **365** | 332 |
/// | r17 | **549** | 1312 |
/// | r18 | **379** | 722 |
/// | r19 | **443** | 911 |
///
/// The stroke-tight hole was the best of the seven on every region but one, and
/// the fatter the hole the more ink came back - the opposite of the prediction,
/// and a large effect rather than a marginal one.
///
/// **The premise is what fails.** The tensor is zeroed everywhere the hole
/// covers, and the fitted mask covers the glyphs: stretched eight times about
/// the paper level, the crop the model is shown has no trace of the text in it
/// at all. So nothing the model returns inside the hole can be the old text
/// surviving, and the control says as much - rendered over a crop with every
/// mark painted out, this rung leaves 0 marks on nine of the eleven regions and
/// 9 and 27 on the other two. The marks are **invented**, by a network
/// finetuned on 300k manga images and asked to complete a hole inside a speech
/// balloon; a wider hole gives it more room and it invents more.
pub fn model_hole(fitted: &Fitted) -> Mask {
    fitted.ink.clone()
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
    session: ort::session::Session,
    selection: Selection,
    /// This session's row in [`crate::registry`], which goes when the session
    /// does. Read through [`Inpainter::spent`].
    lease: crate::registry::Lease,
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
        Ok(Inpainter { session, selection, lease })
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
        self.lease.touch();
        if let Some(decline) = declines(page, fitted) {
            return Err(decline.into());
        }
        let hole = model_hole(fitted);

        let applied = applied_mask(fitted, page.width, page.height);
        let bounds = applied.bounds;
        let managed = crate::engines::model::editable_color(page)?;
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

            let out = self.run(image, model_mask)?;
            ran += 1;

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
                    let mut rgb = [out[at], out[plane + at], out[2 * plane + at]];
                    if matches!(reading, ColorMode::Gray | ColorMode::GrayAlpha) { rgb = [rgb.iter().sum::<f32>() / 3.0; 3]; }
                    commit_srgb(&managed, &mut patch, lx, ly, rgb, alpha, dither)?;
                    done.set(px, py, true);
                }
            }
            // Explicit, and the reason rule 6 is satisfiable at all: the tile's
            // output goes before the next tile's input is built, so the peak is
            // one tile and not two.
            drop(out);
        }

        Ok(Rendered { mask: applied, pixels: patch, pad, tiles: ran, tone: None })
    }

    fn run(&mut self, image: Vec<f32>, mask: Vec<f32>) -> Result<Vec<f32>, Error> {
        let side = MODEL_INPUT as usize;
        let image = ort::value::Tensor::from_array(([1usize, 3, side, side], image))
            .map_err(|e| Error::Run(e.to_string()))?;
        let mask = ort::value::Tensor::from_array(([1usize, 1, side, side], mask))
            .map_err(|e| Error::Run(e.to_string()))?;
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

        assert_eq!(model_hole(&fitted), fitted.ink, "the tensor's hole");
        let applied = applied_mask(&fitted, 200, 200);
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

    /// A real screentone page, which is what this rung exists for. Rungs 0 and
    /// 1 have synthetic fixtures because their arithmetic is checkable by hand;
    /// a model's is not.
    fn screentone_page() -> Option<Raster> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/pages/page-screentone.png");
        let bytes = std::fs::read(path).ok()?;
        crate::image::decode(&bytes).ok()
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
    /// MI-GAN's graph rather than this one's - LaMa's is `DFT` - so the rung
    /// this ladder kept is the determinable one. Confirmed rather than assumed,
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
