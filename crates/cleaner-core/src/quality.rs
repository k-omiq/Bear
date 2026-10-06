//! The decline metric: the one check every rung's output is scored by.
//!
//! It is stated in three lines:
//!
//! > reject when post-edit interior edge-energy exceeds the surround annulus
//! > by more than 2x, or when the interior histogram falls outside the
//! > annulus range
//!
//! and then says the thing that makes this a module rather than a branch inside
//! an engine: it is the "**same measurement as §5.2 step 7, applied to every
//! rung, not only cloud**". Rung 0's fill and every model above it are scored
//! by one function, so that a review row saying
//! *declined* means the same thing whichever rung produced the pixels. §6 also
//! records why it did not exist until now - revision 2 introduced the `decline`
//! outcome without defining the metric it depends on, "which made it
//! unimplementable".
//!
//! ## The surround is the prior, and that is the whole design
//!
//! §5.3 is blunt about the error the obvious implementation makes:
//!
//! > **The structural prior comes from the annulus.** Revision 2 compared
//! > against the pre-edit interior - which contains the text being removed, so
//! > post-edit density is always lower and "the model added strokes" could
//! > never fire.
//!
//! So both sides of every comparison here are the *surround*, and the pre-edit
//! interior is never read at all.
//! [`tests::an_added_stroke_field_is_declined_even_though_the_text_it_replaced_carried_more_ink`]
//! is the test that pins it: the region is built so that revision 2's
//! comparison would have *passed* it, and it declines.
//!
//! ## Two measurements, both relative to the page
//!
//! **Edge energy** is the mean Sobel gradient magnitude
//! ([`crate::fit::sobel_magnitude`]) over a pixel set, in 16-bit luma. The
//! interior's is measured on the page **with the patch composited over it** -
//! §6 says *post-edit* - read exactly as [`crate::composite::composite`] would
//! write it, so the pixels scored are the pixels that would ship.
//!
//! **The histogram test** asks whether the region's levels belong to the paper
//! around it at all. The surround's range is its `p5..p95`, which is the
//! reading §5.2 step 5 already uses for a ring's range (`p95-p5 > 20`), widened
//! by [`crate::fit::deviation_threshold`] - the same noise-floor-relative
//! allowance the mask fit is judged against, for the reason given for every
//! other threshold in this pipeline: flat white on a JPEG q75 raw already
//! measures 5-12 levels from blocking alone, so a fixed number is either
//! dead or fires on every region of every JPEG source.
//!
//! ## What "interior" and "surround" mean to a 3×3 kernel
//!
//! An edge-energy sample counts only where its **entire 3×3 neighbourhood lies
//! on one side of the edit boundary**. Without that rule the two quantities
//! contaminate each other, and both errors favour accepting: an interior sample
//! at the mask border reads the seam rather than the region's own content, and
//! a surround sample beside the mask reaches into the pre-edit interior and
//! picks up exactly the text ink §5.3 says must not enter the prior. For the
//! same reason the surround excludes the mask's whole **bounding box** and not
//! merely the mask: the paper between a text-shaped mask's strokes is the
//! region's own interior wearing a different name.
//!
//! The histogram test needs no neighbourhood and uses every pixel of each set.
//!
//! ## The surround is 32 px wide, and 4 px was measured to fail
//!
//! The obvious surround is [`crate::fit::ring`]'s: the annulus between
//! `dilate(mask, k)` and `dilate(mask, k + ANNULUS_WIDTH)`, four native pixels
//! wide, which the fit already measures for routing. It cannot carry this
//! statistic, and the reason is the one §5.3 gives for the cloud gate's ring in
//! almost the same words - "**Ring width is declared, and it is ≥32 px**",
//! because at ten pixels "the outer half gives one block row" and the statistic
//! degenerates into the one it claimed to replace. Here the degeneration is
//! worse than a bad estimate. Screentone has a period, `ring.rs`'s own
//! periodicity check looks for periods up to 24 px, and a four-pixel band
//! around a rectangle samples that period at essentially one phase: a band
//! landing between two rows of dots reads an edge energy of **zero**, and any
//! reconstruction of the tone then exceeds it by an unbounded factor.
//!
//! Measured, by sweeping a synthetic halftone over every phase offset - dot
//! pitch 6 to 20 px, dot size 2 to 4 px - and reconstructing the tone exactly,
//! which is an output the metric must accept. The worst ratio of interior to
//! surround energy over all phases:
//!
//! | Surround width | 24×20 mask | 14×12 mask |
//! |---|---|---|
//! | 4 px | 39230 | 50338 |
//! | 8 px | 39230 | 50338 |
//! | 16 px | 5.00 | 3.95 |
//! | 24 px | 2.94 | 3.64 |
//! | **32 px** | **1.99** | 5.20 |
//!
//! At 32 px the band is wider than the longest period the pipeline claims to
//! detect, so it holds a whole cell of anything it can see, and a correct
//! reconstruction stays inside §6's factor of two.
//!
//! It is a rectangular band around the mask's bounding box rather than a
//! dilation, which is also what §5.2's ring is - a band of real page pixels
//! around a crop - and which matters practically: `Mask::dilated(32)` applies a
//! 65×65 disc at every pixel, and this runs once per region per rung.
//!
//! **What the table's last cell says, and it is a real limit.** A 14×12 mask on
//! a 20 px pitch still reaches 5.20, because the region is then smaller than
//! one cell of the texture it sits in and the two means stop being estimates of
//! the same quantity. A legitimate reconstruction can be declined there. The
//! direction is the safe one - §6's own "Leaving text is recoverable; a bad
//! fill is not" - but it is a false decline, it is not what §6 describes, and
//! it wants calibration against real fixtures rather than a synthetic sweep.
//!
//! ## Where it cannot be measured, it says so
//!
//! The failure this module
//! is most at risk of: "none of them is measured by the same function used to
//! make the decision - revision 1 had two that were unfailable". A check that
//! silently answers *accepted* because it had nothing to measure is that defect
//! wearing a verdict, so the cases where it has nothing to measure are their
//! own outcome, [`Outcome::Unmeasured`], and a caller can count them:
//!
//! - **A bitonal source.** Levels are 0 and 65535 and nothing else. A flat fill
//!   has an interior edge energy of zero, and any surround holding one ink
//!   pixel has a `p5..p95` covering the whole scale - so both tests pass by
//!   construction, on any output whatsoever. Indexed and CMYK are **not**
//!   excluded: [`crate::image::Raster::luma16_at`] resolves both to real luma
//!   through the palette or the ink model, and a luma measurement over them
//!   means what it says.
//! - **No surround.** A mask whose bounding box is the page leaves the band
//!   empty, and [`crate::fit::RingStats`] already refuses to report a statistic
//!   over nothing for the same reason.
//!
//! Neither declines the region, because a declined region leaves text on the
//! page and §6's justification for accepting that cost - "Leaving text is
//! recoverable; a bad fill is not" - is an argument for declining when the
//! metric *fails*, not when it is absent.
//!
//! ## What it does not see
//!
//! Everything here is luma. A patch whose luma matches its surround and whose
//! chroma does not is accepted, and
//! [`tests::a_chroma_only_error_is_invisible_to_a_luma_metric`] says so out
//! loud rather than leaving it to be discovered. §6 describes no chroma test;
//! §5.2 step 6c gives the cloud rung one, and it is the cloud rung's, measured
//! after that rung's shared tone fit.

use crate::fit::{deviation_threshold, sobel_magnitude};
use crate::image::{BitDepth, Raster};
use crate::mask::Mask;

/// §6's factor, verbatim: interior edge energy exceeding the surround "by more
/// than 2x".
const EDGE_ENERGY_RATIO: f32 = 2.0;

/// How wide the surround band is, in native pixels.
///
/// §5.3's declared minimum for the cloud gate's ring, adopted here because the
/// measurement in this module's documentation says a narrower one cannot hold
/// the statistic: `crate::constants::ANNULUS_WIDTH` is 4, and at 4 the band
/// samples a screentone at one phase and reads zero energy between two rows of
/// dots.
const SURROUND_WIDTH: u32 = 32;

/// What a Sobel magnitude reads on pure noise of unit standard deviation.
///
/// Derived, not measured, and the derivation is short enough to state: each of
/// `gx` and `gy` weights six independent samples by `(1, 2, 1, -1, -2, -1)`, so
/// its standard deviation is `sqrt(12)·σ`, and the magnitude of two independent
/// zero-mean normals has mean `sqrt(π/2)` times theirs. `sqrt(π/2)·sqrt(12) ≈
/// 4.34`.
///
/// It exists so that a *quiet* surround does not divide by zero and, worse,
/// does not make the ratio test infinitely strict on a page whose grain the
/// engine is allowed to reproduce. The surround's own energy is compared
/// against this floor and the larger of the two wins.
const SOBEL_NOISE_GAIN: f32 = 4.34;

/// The floor under the page's noise floor, in 16-bit luma: half an 8-bit level.
///
/// A synthetic page measures a noise sigma of exactly zero, and so does a real
/// page that is genuinely flat where it was sampled. On such a page the test
/// becomes very nearly "is the interior flat too", which is the honest
/// question to ask of flat paper.
const QUIET_FLOOR: f32 = 0.5 * 257.0;

/// The percentiles that bound "the surround range".
///
/// §5.2 step 5 measures a ring's range as `p95 - p5`, and this is that range
/// read as an interval. Not the minimum and the maximum: one dust speck would
/// otherwise widen the range to the whole scale and the test would never fire
/// again.
const RANGE_LOW_PERCENTILE: usize = 5;
const RANGE_HIGH_PERCENTILE: usize = 95;

/// How much of the interior has to sit outside the surround's range before the
/// region is declined.
///
/// **A guess, stated as a guess.** §6 says "the interior histogram falls
/// outside the annulus range" and does not quantify *falls*. Half is the
/// weakest reading that is still a statement about the region rather than about
/// one of its pixels: below it a legitimate reconstruction continuing a dark
/// object into the masked area declines, and at "every pixel" nothing ever
/// would. Read as a median test it is exactly "the typical interior level does
/// not belong to this paper", and it additionally catches the two-sided case a
/// median misses, where an output is half far too dark and half far too light.
const INTERIOR_OUTSIDE_FRACTION: f32 = 0.5;

/// Which of §6's two tests failed.
///
/// In memory only, and deliberately not serialised: the persisted and
/// seam-facing form of a decline is [`Cause::reason_key`], because the seam
/// allows no English across it and the catalogue is the fixed vocabulary -
/// the core may name a key and may not invent
/// one. The catalogue holds one `decline.reason.*` entry for this metric, so
/// both causes answer it; the distinction is kept here so a counter can tell
/// them apart, and so splitting them costs one line on the day a second entry
/// exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// The interior carries more than twice the surround's edge energy: strokes
    /// the paper around it does not account for.
    EdgeEnergy,
    /// The interior's levels are not the surround's levels.
    Histogram,
}

impl Cause {
    /// The i18n key a declined region carries into review and onto disk.
    pub fn reason_key(self) -> &'static str {
        match self {
            Cause::EdgeEnergy | Cause::Histogram => "decline.reason.qualityMetric",
        }
    }
}

/// Why the metric had nothing to measure. Not a pass, and not a decline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unmeasured {
    /// A bitonal source, where both tests pass on any output at all.
    Bitonal,
    /// The mask covers nothing.
    EmptyMask,
    /// The surround band is empty - a mask whose bounding box is the page.
    NoSurround,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Measured, and the patch is kept.
    Accepted,
    /// Measured, and the region is left untouched.
    Declined(Cause),
    Unmeasured(Unmeasured),
}

/// One rung's output, scored. Every number the verdict rests on is on it,
/// because a verdict whose inputs are invisible cannot be argued with in a
/// review row or in a test.
#[derive(Debug, Clone, PartialEq)]
pub struct Assessment {
    pub outcome: Outcome,
    /// Mean Sobel magnitude over the scored interior, in 16-bit luma.
    pub interior_edge_energy: f32,
    /// The same over the scored surround.
    pub surround_edge_energy: f32,
    /// What the interior was actually compared against: the surround's energy
    /// or the page's noise floor, whichever is larger.
    pub edge_energy_floor: f32,
    /// The surround's `p5..p95`, already widened by the noise-floor allowance.
    pub surround_range: (f32, f32),
    /// The share of interior pixels whose post-edit level lies outside it.
    pub interior_outside_fraction: f32,
    /// Interior pixels, and the subset whose whole neighbourhood was interior.
    pub interior_pixels: usize,
    pub scored_interior_pixels: usize,
    /// Surround pixels, and the subset whose whole neighbourhood was clear of
    /// the edit.
    pub surround_pixels: usize,
    pub scored_surround_pixels: usize,
}

impl Assessment {
    pub fn declined(&self) -> bool {
        matches!(self.outcome, Outcome::Declined(_))
    }

    pub fn cause(&self) -> Option<Cause> {
        match self.outcome {
            Outcome::Declined(cause) => Some(cause),
            _ => None,
        }
    }

    /// The i18n key for a declined region, and nothing for any other outcome.
    pub fn reason_key(&self) -> Option<&'static str> {
        self.cause().map(Cause::reason_key)
    }

    fn unmeasured(why: Unmeasured) -> Assessment {
        Assessment {
            outcome: Outcome::Unmeasured(why),
            interior_edge_energy: 0.0,
            surround_edge_energy: 0.0,
            edge_energy_floor: 0.0,
            surround_range: (0.0, 0.0),
            interior_outside_fraction: 0.0,
            interior_pixels: 0,
            scored_interior_pixels: 0,
            surround_pixels: 0,
            scored_surround_pixels: 0,
        }
    }
}

/// Whether §6's metric means anything on this source. See [`Unmeasured::Bitonal`].
pub fn applies(page: &Raster) -> bool {
    page.depth != BitDepth::One
}

/// Score one rung's output against the paper around it.
///
/// `mask` is the **applied** mask - the one the patch will composite through -
/// because the surround has to begin outside everything the edit wrote. `patch` covers `mask.bounds` in the page's own mode and depth,
/// which is the shape every engine in [`crate::engines`] returns. `noise_sigma`
/// is the page's floor from [`crate::fit::page_noise_sigma`], passed in rather
/// than measured here because it is a per-page measurement and this runs per
/// region.
pub fn assess(page: &Raster, mask: &Mask, patch: &Raster, noise_sigma: f32) -> Assessment {
    if !applies(page) {
        return Assessment::unmeasured(Unmeasured::Bitonal);
    }
    if mask.is_empty() {
        return Assessment::unmeasured(Unmeasured::EmptyMask);
    }

    let bounds = mask.bounds;
    let outer = bounds.grown(SURROUND_WIDTH, page.width, page.height);

    // The page as it would be after compositing. `composite` writes a sample
    // only where the applied mask says so, so anywhere else the page is the
    // answer even if the patch buffer holds something else there.
    let after = |x: i64, y: i64| -> f32 {
        let cx = x.clamp(0, page.width as i64 - 1);
        let cy = y.clamp(0, page.height as i64 - 1);
        if mask.contains(cx, cy) {
            let (lx, ly) = ((cx - bounds.x) as u32, (cy - bounds.y) as u32);
            if lx < patch.width && ly < patch.height {
                return patch.luma16_at(lx, ly) as f32;
            }
        }
        page.luma16_at(cx as u32, cy as u32) as f32
    };

    // The 3×3 rule, stated once for both sides: a sample counts only where its
    // whole neighbourhood is on one side of the edit boundary.
    let neighbourhood_inside_mask = |x: i64, y: i64| -> bool {
        (-1..=1).all(|dy| (-1..=1).all(|dx| mask.contains(x + dx, y + dy)))
    };
    let neighbourhood_clear_of_mask = |x: i64, y: i64| -> bool {
        (-1..=1).all(|dy| (-1..=1).all(|dx| !mask.contains(x + dx, y + dy)))
    };
    let energy_at = |x: i64, y: i64| -> f32 {
        sobel_magnitude(|dx, dy| after(x + dx as i64, y + dy as i64))
    };

    let mut interior_levels: Vec<f32> = Vec::new();
    let mut interior_energy_sum = 0f64;
    let mut scored_interior = 0usize;
    for y in bounds.y..bounds.bottom() {
        for x in bounds.x..bounds.right() {
            if !mask.contains(x, y) {
                continue;
            }
            interior_levels.push(after(x, y));
            if neighbourhood_inside_mask(x, y) {
                interior_energy_sum += energy_at(x, y) as f64;
                scored_interior += 1;
            }
        }
    }

    let mut surround_levels: Vec<f32> = Vec::new();
    let mut surround_energy_sum = 0f64;
    let mut scored_surround = 0usize;
    for y in outer.y..outer.bottom() {
        for x in outer.x..outer.right() {
            if x < 0 || y < 0 || x >= page.width as i64 || y >= page.height as i64 {
                continue;
            }
            // Outside the mask's bounding box, not merely outside the mask: the
            // paper between a text-shaped mask's strokes is the region's own
            // interior, and putting it in the prior would let a region vouch
            // for itself.
            if bounds.contains(x, y) {
                continue;
            }
            surround_levels.push(page.luma16_at(x as u32, y as u32) as f32);
            if neighbourhood_clear_of_mask(x, y) {
                surround_energy_sum += energy_at(x, y) as f64;
                scored_surround += 1;
            }
        }
    }

    if surround_levels.is_empty() {
        return Assessment::unmeasured(Unmeasured::NoSurround);
    }

    let interior_edge_energy = if scored_interior == 0 {
        0.0
    } else {
        (interior_energy_sum / scored_interior as f64) as f32
    };
    let surround_edge_energy = if scored_surround == 0 {
        0.0
    } else {
        (surround_energy_sum / scored_surround as f64) as f32
    };
    let edge_energy_floor =
        surround_edge_energy.max(SOBEL_NOISE_GAIN * noise_sigma.max(QUIET_FLOOR));

    let tolerance = deviation_threshold(noise_sigma);
    let low = percentile(&mut surround_levels, RANGE_LOW_PERCENTILE) - tolerance;
    let high = percentile(&mut surround_levels, RANGE_HIGH_PERCENTILE) + tolerance;
    let outside = interior_levels.iter().filter(|v| **v < low || **v > high).count();
    let interior_outside_fraction = outside as f32 / interior_levels.len() as f32;

    // §6's order: edge energy, then the histogram.
    let outcome = if scored_interior > 0
        && interior_edge_energy > EDGE_ENERGY_RATIO * edge_energy_floor
    {
        Outcome::Declined(Cause::EdgeEnergy)
    } else if interior_outside_fraction > INTERIOR_OUTSIDE_FRACTION {
        Outcome::Declined(Cause::Histogram)
    } else {
        Outcome::Accepted
    };

    Assessment {
        outcome,
        interior_edge_energy,
        surround_edge_energy,
        edge_energy_floor,
        surround_range: (low, high),
        interior_outside_fraction,
        interior_pixels: interior_levels.len(),
        scored_interior_pixels: scored_interior,
        surround_pixels: surround_levels.len(),
        scored_surround_pixels: scored_surround,
    }
}

/// The `p`-th percentile by nearest rank, over values this sorts in place.
fn percentile(values: &mut [f32], p: usize) -> f32 {
    values.sort_by(f32::total_cmp);
    values[((values.len() - 1) * p) / 100]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::fill;
    use crate::fit::{self, EdgeMap};
    use crate::image::{ColorMode, fixtures};
    use crate::mask::Rect;

    /// Every synthetic page is this size and every region sits in the middle of
    /// it, so the 32 px surround has room on all four sides.
    const PAGE: u32 = 200;
    const REGION: Rect = Rect { x: 88, y: 90, w: 24, h: 20 };

    fn gray_page(f: impl Fn(u32, u32) -> u8) -> Raster {
        let mut data = Vec::with_capacity((PAGE * PAGE) as usize);
        for y in 0..PAGE {
            for x in 0..PAGE {
                data.push(f(x, y));
            }
        }
        Raster {
            width: PAGE,
            height: PAGE,
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

    fn rgb_page(f: impl Fn(u32, u32) -> [u8; 3]) -> Raster {
        let mut data = Vec::with_capacity((PAGE * PAGE) as usize * 3);
        for y in 0..PAGE {
            for x in 0..PAGE {
                data.extend_from_slice(&f(x, y));
            }
        }
        Raster {
            width: PAGE,
            height: PAGE,
            mode: ColorMode::Rgb,
            depth: BitDepth::Eight,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None,
            color: Default::default(),
            data,
        }
    }

    /// A page of palette indices over a grey ramp palette. The samples are
    /// indices and the luma comes through the palette, which is exactly why an
    /// indexed source is scored rather than skipped.
    fn indexed_page(index_at: impl Fn(u32, u32) -> u8) -> Raster {
        let mut palette = Vec::with_capacity(256 * 3);
        for entry in 0..=255u8 {
            palette.extend_from_slice(&[entry, entry, entry]);
        }
        let mut data = Vec::with_capacity((PAGE * PAGE) as usize);
        for y in 0..PAGE {
            for x in 0..PAGE {
                data.push(index_at(x, y));
            }
        }
        Raster {
            width: PAGE,
            height: PAGE,
            mode: ColorMode::Indexed,
            depth: BitDepth::Eight,
            icc: None,
            palette: Some(palette),
            trns: None,
            srgb_intent: None,
            color: Default::default(),
            data,
        }
    }

    /// A patch over `mask.bounds`: the page, with `paint` applied through the
    /// mask. This is the shape an engine returns - everything outside the mask
    /// copied through - so a synthetic engine and a real one are scored alike.
    fn patch_over(page: &Raster, mask: &Mask, paint: impl Fn(u32, u32, usize) -> u16) -> Raster {
        let bounds = mask.bounds;
        let samples = page.mode.samples();
        let mut patch = Raster {
            width: bounds.w,
            height: bounds.h,
            mode: page.mode,
            depth: page.depth,
            icc: None,
            palette: page.palette.clone(),
            trns: page.trns.clone(),
            srgb_intent: None,
            color: Default::default(),
            data: vec![0; bounds.w as usize * bounds.h as usize * samples],
        };
        for y in 0..bounds.h {
            for x in 0..bounds.w {
                let (px, py) = (bounds.x + x as i64, bounds.y + y as i64);
                let inside = mask.contains(px, py);
                for channel in 0..samples {
                    let value = if inside {
                        paint(px as u32, py as u32, channel)
                    } else {
                        page.sample(px as u32, py as u32, channel)
                    };
                    patch.set_sample(x, y, channel, value);
                }
            }
        }
        patch
    }

    /// A patch painting one level through the mask, in every channel.
    fn flat_patch(page: &Raster, mask: &Mask, level: u16) -> Raster {
        patch_over(page, mask, move |_, _, _| level)
    }

    fn fitted_over(page: &Raster, rect: Rect) -> fit::Fitted {
        let seed = Mask::filled(rect);
        fit::fit(page, &seed, 1.0, 0.0, &EdgeMap::none(page.width, page.height), true)
    }

    fn scored(page: &Raster, mask: &Mask, patch: &Raster) -> Assessment {
        assess(page, mask, patch, fit::page_noise_sigma(page))
    }

    #[test]
    fn a_flat_fill_on_flat_paper_is_accepted() {
        let page = gray_page(|_, _| 246);
        let fitted = fitted_over(&page, REGION);
        let patch = fill::render(&page, &fitted);
        let verdict = scored(&page, &fitted.mask, &patch);
        assert_eq!(verdict.outcome, Outcome::Accepted, "{verdict:?}");
        assert_eq!(verdict.reason_key(), None);
    }

    #[test]
    fn a_flat_fill_on_a_gentle_gradient_is_accepted() {
        // The fill is the ring's median, which lies inside the surround range
        // by construction, and a flat interior has no edge energy. A test
        // failing here would be one no rung 0 output could pass.
        let page = gray_page(|x, _| (150 + x / 4) as u8);
        let fitted = fitted_over(&page, REGION);
        let patch = fill::render(&page, &fitted);
        let verdict = scored(&page, &fitted.mask, &patch);
        assert_eq!(verdict.outcome, Outcome::Accepted, "{verdict:?}");
    }

    #[test]
    fn screentone_continued_at_the_surrounding_pitch_is_accepted() {
        // The case the metric must not punish: an inpainter reconstructing the
        // tone it is surrounded by. The page keeps a flat region so its noise
        // floor comes from paper - which is what a real page's flattest decile
        // is - because otherwise the floor alone would carry the verdict and
        // this would prove nothing about the ratio.
        let tone = |x: u32, y: u32| if x % 8 < 3 && y % 8 < 3 { 40u8 } else { 250 };
        let page = gray_page(|x, y| if x < 40 { 246 } else { tone(x, y) });
        let mask = Mask::filled(REGION);
        let patch = patch_over(&page, &mask, move |x, y, _| tone(x, y) as u16);

        let verdict = scored(&page, &mask, &patch);
        assert_eq!(verdict.outcome, Outcome::Accepted, "{verdict:?}");
        // And the surround, not the noise floor, is what carried it.
        assert!(
            verdict.surround_edge_energy >= verdict.edge_energy_floor,
            "the floor decided this, so the ratio was never tested: {verdict:?}"
        );
        assert!(
            verdict.interior_edge_energy < EDGE_ENERGY_RATIO * verdict.surround_edge_energy,
            "{verdict:?}"
        );
    }

    #[test]
    fn an_added_stroke_field_is_declined_even_though_the_text_it_replaced_carried_more_ink() {
        // §5.3's correction, as a test. The pre-edit interior is denser than
        // anything the engine returned, so revision 2's comparison - post-edit
        // interior against pre-edit interior - passes this region. The surround
        // is flat paper, so §6's comparison declines it.
        let page = gray_page(|x, y| {
            if REGION.contains(x as i64, y as i64) && x % 4 < 2 { 0 } else { 246 }
        });
        let mask = Mask::filled(REGION);
        // Half the ink of the text it replaced, and still strokes on paper that
        // has none.
        let patch = patch_over(&page, &mask, |x, _, _| if x % 4 == 0 { 0 } else { 246 });

        let verdict = scored(&page, &mask, &patch);
        assert_eq!(verdict.outcome, Outcome::Declined(Cause::EdgeEnergy), "{verdict:?}");
        assert_eq!(verdict.reason_key(), Some("decline.reason.qualityMetric"));

        // The measurement that proves the prior has to be the surround: scored
        // the discarded way, this output is an improvement on what was there.
        let untouched = patch_over(&page, &mask, |x, y, c| page.sample(x, y, c));
        let before = scored(&page, &mask, &untouched).interior_edge_energy;
        assert!(
            before > verdict.interior_edge_energy,
            "the pre-edit interior was not the denser one: {before} against {}",
            verdict.interior_edge_energy
        );
    }

    #[test]
    fn a_smooth_grey_blob_is_declined_for_its_tone_although_it_adds_no_strokes() {
        let page = gray_page(|_, _| 246);
        let mask = Mask::filled(REGION);
        let patch = flat_patch(&page, &mask, 120);
        let verdict = scored(&page, &mask, &patch);
        assert_eq!(verdict.outcome, Outcome::Declined(Cause::Histogram), "{verdict:?}");
        // The edge test alone would have passed it: a flat blob has no strokes.
        assert!(
            verdict.interior_edge_energy <= EDGE_ENERGY_RATIO * verdict.edge_energy_floor,
            "{verdict:?}"
        );
    }

    #[test]
    fn a_tone_shift_inside_the_noise_floors_allowance_is_not_a_decline() {
        // The other side of the histogram test. Eight levels is what
        // `deviation_threshold` allows on a clean page, and an engine landing
        // inside it has matched the paper as closely as the mask fit itself is
        // asked to.
        let page = gray_page(|_, _| 246);
        let mask = Mask::filled(REGION);
        let verdict = scored(&page, &mask, &flat_patch(&page, &mask, 240));
        assert_eq!(verdict.outcome, Outcome::Accepted, "{verdict:?}");
    }

    #[test]
    fn the_edge_threshold_moves_with_the_pages_noise_floor() {
        // One and the same output, scored on two pages. The threshold is
        // relative; a fixed constant here would either decline every region
        // of a JPEG raw or never fire on a clean scan.
        let texture = |x: u32, y: u32| if (x + y).is_multiple_of(3) { 236u16 } else { 246 };

        let clean = gray_page(|_, _| 246);
        let mask = Mask::filled(REGION);
        let on_clean =
            scored(&clean, &mask, &patch_over(&clean, &mask, move |x, y, _| texture(x, y)));
        assert_eq!(on_clean.outcome, Outcome::Declined(Cause::EdgeEnergy), "{on_clean:?}");

        let grainy = gray_page(|x, y| {
            let n = ((x * 7919 + y * 104729) % 21) as i32 - 10;
            (241 + n).clamp(0, 255) as u8
        });
        let on_grainy =
            scored(&grainy, &mask, &patch_over(&grainy, &mask, move |x, y, _| texture(x, y)));
        assert_eq!(on_grainy.outcome, Outcome::Accepted, "{on_grainy:?}");
    }

    #[test]
    fn a_colour_page_is_scored_on_luma_and_a_fill_over_it_is_accepted() {
        let page = rgb_page(|x, _| [180, 170, (150 + x / 8) as u8]);
        let fitted = fitted_over(&page, REGION);
        let patch = fill::render(&page, &fitted);
        let verdict = scored(&page, &fitted.mask, &patch);
        assert_eq!(verdict.outcome, Outcome::Accepted, "{verdict:?}");
    }

    #[test]
    fn a_colour_page_declines_an_output_whose_luma_leaves_the_surround() {
        let page = rgb_page(|_, _| [180, 170, 160]);
        let mask = Mask::filled(REGION);
        let patch = flat_patch(&page, &mask, 60);
        let verdict = scored(&page, &mask, &patch);
        assert_eq!(verdict.outcome, Outcome::Declined(Cause::Histogram), "{verdict:?}");
    }

    #[test]
    fn a_chroma_only_error_is_invisible_to_a_luma_metric() {
        // Written down rather than worked around. §6 describes no chroma test,
        // and the one §5.2 step 6c describes belongs to the cloud rung and runs
        // after that rung's shared tone fit. A caller wanting one needs it
        // there, not here.
        let page = rgb_page(|_, _| [128, 128, 128]);
        let mask = Mask::filled(REGION);
        // 0.299·255 + 0.587·96 + 0.114·0 ≈ 133: five levels off in luma, and a
        // vivid orange in colour.
        let patch = patch_over(&page, &mask, |_, _, channel| match channel {
            0 => 255,
            1 => 96,
            _ => 0,
        });
        assert_eq!(scored(&page, &mask, &patch).outcome, Outcome::Accepted);
    }

    #[test]
    fn a_bitonal_source_is_reported_unmeasured_rather_than_passed() {
        // Both tests pass on a bitonal page whatever the output is, so a
        // verdict here would be one of the unfailable checks.
        let page = fixtures::by_name("bitonal").raster;
        assert!(!applies(&page));
        let mask = Mask::filled(Rect::new(10, 10, 12, 10));
        let patch = Raster {
            width: 12,
            height: 10,
            mode: ColorMode::Gray,
            depth: BitDepth::One,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None,
            color: Default::default(),
            data: vec![0; 2 * 10],
        };
        let verdict = assess(&page, &mask, &patch, 0.0);
        assert_eq!(verdict.outcome, Outcome::Unmeasured(Unmeasured::Bitonal));
        assert!(!verdict.declined());
        assert_eq!(verdict.reason_key(), None);
    }

    #[test]
    fn an_indexed_source_is_scored_because_its_luma_is_a_real_measurement() {
        let page = indexed_page(|_, _| 246);
        assert!(applies(&page));
        let fitted = fitted_over(&page, REGION);
        let patch = fill::render(&page, &fitted);
        assert_eq!(scored(&page, &fitted.mask, &patch).outcome, Outcome::Accepted);

        // And it can still fail: an index whose palette entry is nothing like
        // the paper around it.
        let mask = Mask::filled(REGION);
        let dark = flat_patch(&page, &mask, 40);
        assert_eq!(scored(&page, &mask, &dark).outcome, Outcome::Declined(Cause::Histogram));
    }

    #[test]
    fn a_mask_covering_the_whole_page_reports_no_surround_rather_than_a_number() {
        let page = gray_page(|_, _| 200);
        let mask = Mask::filled(Rect::new(0, 0, PAGE, PAGE));
        let patch = flat_patch(&page, &mask, 0);
        assert_eq!(
            assess(&page, &mask, &patch, 0.0).outcome,
            Outcome::Unmeasured(Unmeasured::NoSurround)
        );
    }

    #[test]
    fn an_empty_mask_is_not_a_verdict() {
        let page = gray_page(|_, _| 246);
        let mask = Mask::empty(REGION);
        let patch = flat_patch(&page, &mask, 0);
        assert_eq!(
            assess(&page, &mask, &patch, 0.0).outcome,
            Outcome::Unmeasured(Unmeasured::EmptyMask)
        );
    }

    #[test]
    fn the_surround_never_reads_the_text_the_mask_covers() {
        // Both halves of the exclusion rule at once: the bounding box is out of
        // the surround, and the 3×3 rule keeps the kernel off the boundary. Get
        // either wrong and the region's own ink joins the prior it is judged
        // against, which is an error in the direction of accepting.
        let page = gray_page(|x, y| {
            if REGION.contains(x as i64, y as i64) && x % 4 < 2 { 0 } else { 246 }
        });
        let mask = Mask::filled(REGION);
        let patch = flat_patch(&page, &mask, 246);
        let verdict = scored(&page, &mask, &patch);
        assert_eq!(verdict.outcome, Outcome::Accepted, "{verdict:?}");
        assert!(verdict.scored_surround_pixels > 0, "the rule left no surround samples");
        assert!(
            verdict.surround_edge_energy < 257.0,
            "the surround read the masked ink: {}",
            verdict.surround_edge_energy
        );
    }
}
