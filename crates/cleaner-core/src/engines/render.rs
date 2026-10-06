//! Pure diffusion crop preparation and host-side compositing contracts.
//!
//! This module decouples the raster preparation (tight crop extraction, context padding,
//! edge replication, and hint mask extraction) and host-side composition (isolation
//! ramp, alpha blending, dithering, channel reconstruction, and bit-exact preservation)
//! from any specific diffusion runner (in-process, sidecar, or remote cloud execution).
//!
//! ## Invariants
//!
//! 1. **Bit-Identical Outside Applied Mask:** Pixels outside the applied blend mask
//!    ([`applied_mask`]) are bit-identical to the source raster.
//! 2. **Alpha Channel Preservation:** Alpha channels (in `LA` and `RGBA` modes) are copied
//!    directly from the source page and never overwritten or synthesized by diffusion.
//! 3. **Versioned, Latent-Snapped Context:** The crop is centred on the region's bounding
//!    box with the context its [`Preprocessing`] version names (V1: half the long side clamped
//!    between [`CONTEXT_MIN`] and [`CONTEXT_MAX`]; V2: [`CONTEXT_WIDE`], or as much of it as
//!    keeps the crop inside [`MAX_CROP_SIDE`] and [`MAX_CROP_PIXELS`]), snapped up to
//!    multiples of [`LATENT_STRIDE`]. A render is prepared and composited under one version.
//! 4. **Edge Replication Without Reflection:** When a crop extends beyond the source page
//!    boundaries, missing pixels are edge-replicated. Missing padding is recorded via [`EdgePad`].
//! 5. **Explicit Checked Output Validation:** Model outputs are verified against expected
//!    geometry and buffer length using checked arithmetic before compositing.
//! 6. **Immutable Prepared State Binding:** Preparation captures the target patch, alpha ramp,
//!    and source mode/depth. Composition consumes this immutable snapshot alongside the
//!    [`GeneratedCrop`], preventing source desynchronization by construction.
//! 7. **Tone Alignment Inside the Support Only (V2):** The answer's drift from the page is
//!    measured on a ring outside the applied mask and corrected before the ramp blends it
//!    ([`ToneReport`]); the correction changes what is written inside the applied mask and
//!    nothing outside it.

use serde::{Deserialize, Serialize};

use crate::engines::model::{
    self, answer_mode, applied_mask, ceiling_for, dither_at, page_crop, AlphaRamp, Decline,
    Error, Rendered, MAX_BOX,
};
use crate::fit::Fitted;
use crate::image::{BitDepth, ColorMode, Raster};
use crate::mask::{Mask, Rect};
use crate::strip::window::EdgePad;

/// Supported cloud inference providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CloudProvider {
    Beam,
    Modal,
}

impl CloudProvider {
    pub fn as_str(&self) -> &'static str {
        match self {
            CloudProvider::Beam => "beam",
            CloudProvider::Modal => "modal",
        }
    }
}

/// Where a rendering operation should execute.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum ExecutionTarget {
    #[default]
    Local,
    #[serde(rename = "beam")]
    Beam {
        profile_id: String,
    },
    #[serde(rename = "modal")]
    Modal {
        profile_id: String,
    },
}

impl ExecutionTarget {
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }

    pub fn provider(&self) -> Option<CloudProvider> {
        match self {
            Self::Local => None,
            Self::Beam { .. } => Some(CloudProvider::Beam),
            Self::Modal { .. } => Some(CloudProvider::Modal),
        }
    }

    pub fn profile_id(&self) -> Option<&str> {
        match self {
            Self::Local => None,
            Self::Beam { profile_id } | Self::Modal { profile_id } => Some(profile_id),
        }
    }
}

/// User guidance supplements the worker's fixed artwork-preservation instructions.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QwenEdit {
    pub target: QwenTarget,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QwenTarget { Auto, Dialogue, SoundEffect, Other }

/// Model and recipe identity contract for diffusion rendering.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RenderRecipe {
    pub recipe_id: String,
    pub preprocessing_version: String,
    pub model_id: String,
    pub model_revision: String,
    pub native_mask_conditioning: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qwen_edit: Option<QwenEdit>,
}

impl RenderRecipe {
    pub fn sampling(&self) -> super::flux::WireSampling {
        let mut sampling = super::flux::wire_sampling();
        if self.recipe_id == "mc-qwen-image-edit-2511-v4" { sampling.steps = 8; }
        sampling
    }

    /// Deployment identity excludes request-local guidance. Grant equality
    /// still compares the complete recipe, including that guidance.
    pub fn same_deployment(&self, other: &Self) -> bool {
        let mut a = self.clone(); let mut b = other.clone();
        a.qwen_edit = None; b.qwen_edit = None; a == b
    }

    pub fn new(
        recipe_id: impl Into<String>,
        preprocessing_version: impl Into<String>,
        model_id: impl Into<String>,
        model_revision: impl Into<String>,
        native_mask_conditioning: bool,
    ) -> Self {
        Self {
            recipe_id: recipe_id.into(),
            preprocessing_version: preprocessing_version.into(),
            model_id: model_id.into(),
            model_revision: model_revision.into(),
            native_mask_conditioning,
            qwen_edit: None,
        }
    }
}

/// The crop's dimensions are snapped up to a multiple of this.
pub const LATENT_STRIDE: u32 = 16;

/// The least context a [`Preprocessing::V1`] crop carries, per side.
pub const CONTEXT_MIN: u32 = 24;

/// The most context a [`Preprocessing::V1`] crop carries, per side.
pub const CONTEXT_MAX: u32 = 80;

/// The context a [`Preprocessing::V2`] crop carries, per side, whatever the
/// region's size, and so the widest context any supported version adds.
///
/// Measured on the stopped demo chapter's own crops, re-rendered locally at
/// both widths: a glyph crop of 80 to 96 px carried 40 px of page at V1, was
/// upscaled about five times to the working size, and came back blank white or
/// as flattened mush over screentone. At 128 px the same crops kept the scene
/// and reconstructed the tone (high-frequency retention 0.34 to 0.64 on the
/// worst of them). A large region loses a little working resolution to the
/// wider frame, which the same renders did not show as a defect.
pub const CONTEXT_WIDE: u32 = 128;

/// The largest region accepted by model engines.
pub const MAX_REGION: u32 = MAX_BOX;

/// The widest and tallest crop the cloud render service accepts
/// (`ServiceLimits::max_width` and `max_height`, which
/// [`crate::cloud_wire::provisional_fixture_limits`] takes from here).
/// `deploy/cloud/tests/test_recipe_parity.py` holds the deployed gateway's
/// limits equal to these.
pub const MAX_CROP_SIDE: u32 = 2048;

/// The most pixels one crop the cloud render service accepts may hold
/// (`ServiceLimits::max_pixels`): a full square at [`MAX_CROP_SIDE`].
pub const MAX_CROP_PIXELS: u64 = 4_194_304;

const _: () = assert!(MAX_CROP_SIDE % LATENT_STRIDE == 0);
const _: () = assert!(MAX_CROP_PIXELS == MAX_CROP_SIDE as u64 * MAX_CROP_SIDE as u64);

/// The preprocessing version this build prepares new renders with. The cloud
/// gateway advertises the version its recipe pins, and
/// `deploy/cloud/tests/test_recipe_parity.py` holds the two equal.
pub const PREPROCESSING_VERSION: &str = "2.0.0";

/// How a region becomes the crop a model is shown, and how its answer is
/// written back. A render is prepared and composited under one version, which
/// its recipe names, so a result that was in flight when the version changed
/// is still composited exactly as it was prepared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Preprocessing {
    /// `1.0.0`. The hole is whatever mask the caller hands in, the context is
    /// half the region's long side clamped to [`CONTEXT_MIN`]..[`CONTEXT_MAX`],
    /// and the answer is blended as it came back.
    V1,
    /// `2.0.0`. The hole is a stored region's lettering ([`Preprocessing::hole`]),
    /// the context is [`CONTEXT_WIDE`], and the answer's tone is aligned to the
    /// page around the edit before it is blended ([`ToneReport`]).
    V2,
}

/// A preprocessing version this build cannot prepare or composite.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unsupported preprocessing version {0:?}")]
pub struct UnsupportedPreprocessing(pub String);

impl Preprocessing {
    /// The version new renders are prepared with.
    pub const CURRENT: Preprocessing = Preprocessing::V2;

    /// The version a recipe names. Anything else is refused rather than
    /// guessed at: a crop prepared one way and composited another is a patch
    /// that matches neither.
    pub fn of(version: &str) -> Result<Preprocessing, UnsupportedPreprocessing> {
        match version {
            "1.0.0" => Ok(Preprocessing::V1),
            PREPROCESSING_VERSION => Ok(Preprocessing::V2),
            other => Err(UnsupportedPreprocessing(other.to_owned())),
        }
    }

    pub fn version(self) -> &'static str {
        match self {
            Preprocessing::V1 => "1.0.0",
            Preprocessing::V2 => PREPROCESSING_VERSION,
        }
    }

    /// The set a render of a stored region removes, given the region's
    /// applied `mask` and its lettering `ink`.
    ///
    /// V1 handed the stored mask to the preparation as a fresh hole, so a
    /// detection's grown fill blob became the model's hole and a re-render of
    /// a patch grew by the isolation radius every time. V2 takes the lettering
    /// both times, the set a local clean of the same record writes through; a
    /// record that never stored lettering falls back to its mask.
    pub fn hole<'a>(self, mask: &'a Mask, ink: &'a Mask) -> &'a Mask {
        match self {
            Preprocessing::V2 if !ink.is_empty() => ink,
            _ => mask,
        }
    }

    /// How much real page the crop carries around the region, per side.
    ///
    /// V2 carries [`CONTEXT_WIDE`] where the snapped crop still fits the
    /// render service ([`MAX_CROP_SIDE`] a side, [`MAX_CROP_PIXELS`] in all),
    /// and otherwise the most that does: an 1800 px hole grown to 1810 by the
    /// isolation radius takes 119 px a side, a 2048 px crop, where the full
    /// width would ask for 2080 and be refused. A region too large to fit
    /// with none gets none, and the service's own limit refuses it. V1 is
    /// left exactly as results in flight were prepared under it.
    pub fn context_for(self, bounds: Rect) -> u32 {
        match self {
            Preprocessing::V1 => (bounds.w.max(bounds.h) / 2).clamp(CONTEXT_MIN, CONTEXT_MAX),
            Preprocessing::V2 => {
                let fits = |context: u32| match (crop_side(bounds.w, context), crop_side(bounds.h, context)) {
                    (Some(w), Some(h)) => fits_service(w, h),
                    _ => false,
                };
                (0..=CONTEXT_WIDE).rev().find(|&context| fits(context)).unwrap_or(0)
            }
        }
    }

    /// The crop the model is shown around the applied `bounds`, in page
    /// coordinates; `None` when its sides are past what a `u32` can hold,
    /// which no page this application reads comes near.
    pub fn crop_for(self, bounds: Rect) -> Option<Rect> {
        let context = self.context_for(bounds);
        let (w, h) = (crop_side(bounds.w, context)?, crop_side(bounds.h, context)?);
        Some(Rect::new(
            bounds.x + bounds.w as i64 / 2 - w as i64 / 2,
            bounds.y + bounds.h as i64 / 2 - h as i64 / 2,
            w,
            h,
        ))
    }

    /// The crop a render removing `hole` sends from a `page_w` by `page_h`
    /// page: the hole grown by the isolation radius and clipped to the page
    /// ([`model::applied_bounds`]), then [`Preprocessing::crop_for`]. The
    /// preparation cuts its crop here, and the cost estimate and the consent
    /// digest take theirs from here too, so a region at the page's right or
    /// bottom edge is estimated at the crop it is sent at.
    pub fn crop_of_hole(self, hole: Rect, page_w: u32, page_h: u32) -> Option<Rect> {
        self.crop_for(model::applied_bounds(hole, page_w, page_h))
    }
}

/// [`Preprocessing::context_for`] under [`Preprocessing::CURRENT`].
pub fn context_for(bounds: Rect) -> u32 {
    Preprocessing::CURRENT.context_for(bounds)
}

/// One side of a crop: `extent` with `context` on both sides, snapped up to
/// the next multiple of [`LATENT_STRIDE`]. Worked in `u64`, so no extent
/// wraps or panics on the way; `None` when the side does not fit a `u32`.
fn crop_side(extent: u32, context: u32) -> Option<u32> {
    let side = (u64::from(extent) + 2 * u64::from(context)).next_multiple_of(u64::from(LATENT_STRIDE));
    u32::try_from(side).ok()
}

/// Whether the cloud render service takes a `w` by `h` crop.
fn fits_service(w: u32, h: u32) -> bool {
    w <= MAX_CROP_SIDE && h <= MAX_CROP_SIDE && u64::from(w) * u64::from(h) <= MAX_CROP_PIXELS
}

/// Whether the cloud render service takes `crop` ([`MAX_CROP_SIDE`],
/// [`MAX_CROP_PIXELS`]). A region whose crop does not fit even with no
/// context is refused before it is planned, not when it is sent.
pub fn within_service_limits(crop: Rect) -> bool {
    fits_service(crop.w, crop.h)
}

/// Snap an extent up to the next multiple of [`LATENT_STRIDE`].
#[cfg(test)]
pub(crate) fn snap(extent: u32) -> u32 {
    crop_side(extent, 0).expect("a test extent fits a u32")
}

/// [`Preprocessing::crop_for`] under [`Preprocessing::CURRENT`].
pub fn crop_for(bounds: Rect) -> Option<Rect> {
    Preprocessing::CURRENT.crop_for(bounds)
}

/// Whether model diffusion engines can run on this raster.
pub fn applies(page: &Raster) -> bool {
    model::applies(page) && page.depth != BitDepth::Sixteen
}

/// Every reason a region would be declined before crop preparation or execution.
pub fn declines(page: &Raster, fitted: &Fitted) -> Option<Decline> {
    if let Some(decline) = model::declines(page, fitted) {
        return Some(decline);
    }
    (page.depth == BitDepth::Sixteen).then_some(Decline::DepthBeyondEngine(page.depth))
}

/// The crop as RGB8 samples, edge-replicated where it runs off the page.
pub(crate) fn crop_samples(page: &Raster, crop: Rect, managed: &crate::image::color::ManagedColor) -> (EdgePad, Vec<u8>) {
    let mut pad = EdgePad::None;
    let total_samples = (crop.w as usize)
        .checked_mul(crop.h as usize)
        .and_then(|px| px.checked_mul(3))
        .expect("crop dimensions overflow byte length");
    let mut out = vec![0u8; total_samples];
    for y in 0..crop.h as i64 {
        for x in 0..crop.w as i64 {
            let (px, py) = (crop.x + x, crop.y + y);
            let cx = px.clamp(0, page.width as i64 - 1);
            let cy = py.clamp(0, page.height as i64 - 1);
            if (cx, cy) != (px, py) {
                pad = EdgePad::Replicate;
            }
            let at = (y * crop.w as i64 + x) as usize * 3;
            let rgba = managed.pixel(page, cx as u32, cy as u32);
            for c in 0..3 { out[at + c] = (rgba[c] * 255.0).round().clamp(0.0,255.0) as u8; }
        }
    }
    (pad, out)
}

/// Per crop pixel, whether the page is fully opaque there, edge-replicated as
/// [`crop_samples`] is: full alpha where the mode has an alpha channel, and
/// anything but the `tRNS` colour key ([`Raster::colour_key`]) where a Gray
/// or RGB page has one, since the decoder keeps that chunk as it came rather
/// than turning it into alpha. `None` for a page with neither.
pub(crate) fn crop_opacity(page: &Raster, crop: Rect, ceiling: f64) -> Option<Vec<bool>> {
    let alpha = page.mode.alpha_channel();
    let key = page.colour_key();
    let opaque_at = |x: u32, y: u32| match (alpha, &key) {
        (Some(alpha), _) => page.sample(x, y, alpha) as f64 >= ceiling,
        (None, Some(key)) => key.iter().enumerate().any(|(channel, &keyed)| page.sample(x, y, channel) != keyed),
        (None, None) => true,
    };
    if alpha.is_none() && key.is_none() {
        return None;
    }
    let mut out = Vec::with_capacity(crop.w as usize * crop.h as usize);
    for y in 0..crop.h as i64 {
        for x in 0..crop.w as i64 {
            let cx = (crop.x + x).clamp(0, page.width as i64 - 1) as u32;
            let cy = (crop.y + y).clamp(0, page.height as i64 - 1) as u32;
            out.push(opaque_at(cx, cy));
        }
    }
    Some(out)
}

/// The hint mask samples: 255 where ink is present, 0 elsewhere.
pub(crate) fn hint_samples(fitted: &Fitted, crop: Rect) -> Vec<u8> {
    let total_samples = (crop.w as usize)
        .checked_mul(crop.h as usize)
        .expect("crop dimensions overflow byte length");
    let mut out = vec![0u8; total_samples];
    for y in 0..crop.h as i64 {
        for x in 0..crop.w as i64 {
            if fitted.ink.contains(crop.x + x, crop.y + y) {
                out[(y * crop.w as i64 + x) as usize] = 255;
            }
        }
    }
    out
}

/// One returned sample reduced back to the page's color mode.
pub(crate) fn returned_sample(edited: &[u8], mode: ColorMode, channel: usize, at: usize) -> f64 {
    match mode {
        ColorMode::Gray | ColorMode::GrayAlpha => {
            (edited[at] as f64 + edited[at + 1] as f64 + edited[at + 2] as f64) / 3.0 / 255.0
        }
        _ => edited[at + channel.min(2)] as f64 / 255.0,
    }
}

/// Prepared diffusion inputs containing isolated raster crops, hint masks, bounds,
/// and the captured source state needed for immutable host composition.
#[derive(Debug, Clone)]
pub struct PreparedRender {
    preprocessing: Preprocessing,
    /// The page the crop was cut from, in its own coordinates: what the crop
    /// holds outside it is edge replication, not page.
    extent: Rect,
    applied: Mask,
    /// The hole: the lettering the model is asked to remove. What lies
    /// between it and the applied mask's edge is the page the lettering sits
    /// on, which is what the tone alignment takes as background.
    ink: Mask,
    /// Per crop pixel, whether the page is fully opaque there
    /// ([`crop_opacity`]: alpha, or a `tRNS` colour key); `None` for a page
    /// with neither. A colour under alpha 0 is not what anyone sees.
    opaque: Option<Vec<bool>>,
    bounds: Rect,
    crop: Rect,
    pad: EdgePad,
    image_rgb8: Vec<u8>,
    hint_gray8: Vec<u8>,
    ramp: AlphaRamp,
    patch: Raster,
    /// The mode the model's answer is read in ([`answer_mode`]): grey on a
    /// grey page stored as RGB, so the patch stays grey.
    reading: ColorMode,
    depth: BitDepth,
    ceiling: f64,
}

/// A generated diffusion crop returned by an inpainter or inference engine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeneratedCrop<'a> {
    width: u32,
    height: u32,
    rgb8: &'a [u8],
}

impl<'a> GeneratedCrop<'a> {
    pub fn new(width: u32, height: u32, rgb8: &'a [u8]) -> Self {
        Self {
            width,
            height,
            rgb8,
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn rgb8(&self) -> &'a [u8] {
        self.rgb8
    }
}

impl PreparedRender {
    /// Prepare the render inputs for a page region under
    /// [`Preprocessing::CURRENT`].
    pub fn prepare(page: &Raster, fitted: &Fitted) -> Result<Self, Error> {
        Self::prepare_as(page, fitted, Preprocessing::CURRENT)
    }

    /// Prepare the render inputs for a page region under `preprocessing`.
    ///
    /// Validates source raster geometry, enforces model engine eligibility,
    /// extracts the snapped crop and hint samples, and captures the original
    /// unedited patch for subsequent composition.
    pub fn prepare_as(
        page: &Raster,
        fitted: &Fitted,
        preprocessing: Preprocessing,
    ) -> Result<Self, Error> {
        if page.width == 0 || page.height == 0 {
            return Err(Error::Run(
                "cannot prepare render for zero-dimension page".to_owned(),
            ));
        }
        if let Some(decline) = declines(page, fitted) {
            return Err(decline.into());
        }
        let applied = applied_mask(fitted, page.width, page.height);
        let bounds = applied.bounds;
        if bounds.w == 0 || bounds.h == 0 {
            return Err(Error::Run(
                "cannot prepare render for empty region bounds".to_owned(),
            ));
        }
        let crop = preprocessing
            .crop_of_hole(fitted.ink.bounds, page.width, page.height)
            .ok_or_else(|| Error::Run("cannot prepare render for an unrepresentable crop".to_owned()))?;
        debug_assert_eq!(model::applied_bounds(fitted.ink.bounds, page.width, page.height), bounds);
        if crop.w == 0 || crop.h == 0 {
            return Err(Error::Run(
                "cannot prepare render for empty crop bounds".to_owned(),
            ));
        }
        let ceiling = ceiling_for(page.depth);
        let managed = model::editable_color(page)?;
        let (pad, image_rgb8) = crop_samples(page, crop, &managed);
        let hint_gray8 = hint_samples(fitted, crop);
        let opaque = crop_opacity(page, crop, ceiling);
        let ramp = AlphaRamp::new(fitted, page.width, page.height);
        let patch = page_crop(page, bounds);

        Ok(PreparedRender {
            preprocessing,
            extent: Rect::new(0, 0, page.width, page.height),
            applied,
            ink: fitted.ink.clone(),
            opaque,
            bounds,
            crop,
            pad,
            image_rgb8,
            hint_gray8,
            ramp,
            patch,
            reading: answer_mode(page),
            depth: page.depth,
            ceiling,
        })
    }

    pub fn preprocessing(&self) -> Preprocessing {
        self.preprocessing
    }

    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    pub fn crop(&self) -> Rect {
        self.crop
    }

    pub fn pad(&self) -> EdgePad {
        self.pad
    }

    pub fn image_rgb8(&self) -> &[u8] {
        &self.image_rgb8
    }

    pub fn hint_gray8(&self) -> &[u8] {
        &self.hint_gray8
    }

    pub fn applied(&self) -> &Mask {
        &self.applied
    }

    /// The weight the model's answer is written at, at page position
    /// `(x, y)` of the prepared raster: 1 in the core, the ramp's steps in the
    /// feather, 0 outside the applied mask.
    pub fn alpha(&self, x: i64, y: i64) -> f64 {
        if self.applied.contains(x, y) { self.ramp.at(x, y) } else { 0.0 }
    }

    /// Validate the dimensions and buffer length of the generated crop.
    pub fn validate_generated(&self, generated: &GeneratedCrop<'_>) -> Result<(), Error> {
        if generated.width != self.crop.w || generated.height != self.crop.h {
            return Err(Error::Run(format!(
                "asked for {}×{} and got {}×{}",
                self.crop.w, self.crop.h, generated.width, generated.height
            )));
        }
        let expected_len = (self.crop.w as usize)
            .checked_mul(self.crop.h as usize)
            .and_then(|px| px.checked_mul(3))
            .ok_or_else(|| {
                Error::Run("crop dimensions overflow byte length calculation".to_owned())
            })?;
        if generated.rgb8.len() != expected_len {
            return Err(Error::Run(format!(
                "the model output byte length ({}) does not match expected length ({})",
                generated.rgb8.len(),
                expected_len
            )));
        }
        Ok(())
    }

    /// Composite the generated crop back onto the captured source patch, the
    /// way this preparation's version does: as it came back under V1, tone
    /// aligned under V2.
    ///
    /// Clones the captured bounded patch and evaluates the captured AlphaRamp without requiring
    /// the source page to be passed again, preventing caller desynchronization by construction.
    /// A bounded preview of the actual tone-aligned blend, with the original
    /// crop beside it. Context outside the write mask stays exactly as captured.
    pub fn preview_rgb8(&self, generated: &GeneratedCrop<'_>) -> Result<(Vec<u8>, Vec<u8>), Error> {
        let rendered = self.composite(generated)?;
        let before = self.image_rgb8.clone();
        let mut after = before.clone();
        let pixels = &rendered.pixels;
        for y in 0..pixels.height {
            for x in 0..pixels.width {
                let cx = rendered.mask.bounds.x + i64::from(x) - self.crop.x;
                let cy = rendered.mask.bounds.y + i64::from(y) - self.crop.y;
                if cx < 0 || cy < 0 || cx >= i64::from(self.crop.w) || cy >= i64::from(self.crop.h) { continue; }
                let at = (cy as usize * self.crop.w as usize + cx as usize) * 3;
                for channel in 0..3 {
                    let source_channel = if matches!(pixels.mode, ColorMode::Gray | ColorMode::GrayAlpha) { 0 } else { channel };
                    after[at + channel] = (f64::from(pixels.sample(x, y, source_channel)) / self.ceiling * 255.0).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        Ok((before, after))
    }

    pub fn composite(&self, generated: &GeneratedCrop<'_>) -> Result<Rendered, Error> {
        let tone = match self.preprocessing {
            Preprocessing::V1 => None,
            Preprocessing::V2 => Some(self.tone(generated)?),
        };
        self.blend(generated, tone)
    }

    /// How far the model's answer drifted from the page around the edit,
    /// measured where both show the same page: the ring
    /// [`TONE_RING_INNER`]..=[`TONE_RING_OUTER`] px outside the applied mask,
    /// inside the page.
    ///
    /// Each ring sample compares a [`TONE_BOX`]-wide box mean of the crop the
    /// model was shown with the same box of its answer, per page channel. Box
    /// means rather than pixels, because a screentone the model redrew half a
    /// period out of phase differs by the full dot contrast pixel by pixel and
    /// by nothing on average.
    ///
    /// **Only background counts.** The ring says how the answer drifted from
    /// the page the lettering sits on only where the ring *is* that page. The
    /// page between the lettering and the applied mask's edge is the one
    /// sure sample of it, so a ring sample is evidence only where the page
    /// there has a colour at least [`BACKGROUND_SUPPORT`] of those samples
    /// have, within [`BACKGROUND_TOLERANCE`]. Artwork beside the lettering
    /// is not: a grey balloon's lettering ringed by purple art the model
    /// returned grey would otherwise take a purple tint the full
    /// [`MAX_TONE_OFFSET`] deep. Page under less than full alpha is not
    /// evidence either: its colour is whatever the file stored there. Fewer
    /// than [`MIN_TONE_SAMPLES`] samples of background and nothing is
    /// corrected, and a correction that would part the channels further than
    /// the raw answer does is not drift either unless the lettering's own
    /// surroundings show that parting: too little of them near the colour
    /// *and* parted as it is ([`MAX_ADDED_SEPARATION`], [`BACKGROUND_SHARE`])
    /// and the alignment abstains.
    ///
    /// The samples are banded by the level the model returned, and each band
    /// with enough of them contributes the median difference at its median
    /// level: a correction curve rather than one offset, because the drift is
    /// not the same at every level. Measured over the demo chapter's crops the
    /// bands of one crop disagreed by a median 8 levels, and where a channel
    /// clips - a sky at full blue, shown and returned both at 255 - a single
    /// offset learned on the darker art beside it turned the sky green. A band's
    /// median also outvotes the parts of the ring the model did change: lettering
    /// of another region it removed returns paper, and lands in the paper band
    /// the true paper already holds.
    pub fn tone(&self, generated: &GeneratedCrop<'_>) -> Result<ToneReport, Error> {
        self.validate_generated(generated)?;
        let channels = tone_channels(self.reading);
        let (w, h) = (self.crop.w as i64, self.crop.h as i64);
        let distance = self.distance_to_applied();
        let half = (TONE_BOX / 2) as i64;
        let opaque = |x: i64, y: i64| self.opaque.as_ref().is_none_or(|opaque| opaque[(y * w + x) as usize]);
        // The lettering and the pixel around it: an anti-aliased edge the
        // hole stops short of is lettering too, not the page it sits on.
        let lettering = self.ink.dilated(1, u32::MAX, u32::MAX);
        // The box mean of `rgb8` around crop pixel (cx, cy), per channel, over
        // the opaque pixels that are not lettering: `None` when fewer than
        // half the box's pixels are.
        let box_mean = |rgb8: &[u8], cx: i64, cy: i64| -> Option<[f64; 3]> {
            let (mut sum, mut used, mut seen) = ([0.0; 3], 0usize, 0usize);
            for y in (cy - half).max(0)..=(cy + half).min(h - 1) {
                for x in (cx - half).max(0)..=(cx + half).min(w - 1) {
                    seen += 1;
                    if !opaque(x, y) || lettering.contains(self.crop.x + x, self.crop.y + y) {
                        continue;
                    }
                    used += 1;
                    for (channel, sum) in sum.iter_mut().enumerate().take(channels) {
                        *sum += returned_sample(rgb8, self.reading, channel, ((y * w + x) * 3) as usize);
                    }
                }
            }
            (used * 2 >= seen).then(|| sum.map(|sum| sum / used as f64 * 255.0))
        };
        let usable = |cx: i64, cy: i64| self.extent.contains(self.crop.x + cx, self.crop.y + cy) && opaque(cx, cy);

        // The page the lettering sits on, as the model was shown it.
        let mut background = Vec::new();
        for cy in (0..h).step_by(TONE_STRIDE) {
            for cx in (0..w).step_by(TONE_STRIDE) {
                let (px, py) = (self.crop.x + cx, self.crop.y + cy);
                if !self.applied.contains(px, py) || self.ink.contains(px, py) || !usable(cx, cy) {
                    continue;
                }
                background.extend(box_mean(&self.image_rgb8, cx, cy));
            }
        }
        let background_colours = ColourIndex::new(&background, channels);

        // Ring samples on that page: (shown, returned), and the texture of each.
        let mut ring = Vec::new();
        let (mut ring_samples, mut texture_shown, mut texture_returned) = (0, 0.0, 0.0);
        for cy in (0..h).step_by(TONE_STRIDE) {
            for cx in (0..w).step_by(TONE_STRIDE) {
                let d = distance[(cy * w + cx) as usize];
                if !(TONE_RING_INNER..=TONE_RING_OUTER).contains(&d) || !self.extent.contains(self.crop.x + cx, self.crop.y + cy) {
                    continue;
                }
                ring_samples += 1;
                if !opaque(cx, cy) {
                    continue;
                }
                let (Some(shown), Some(returned)) = (box_mean(&self.image_rgb8, cx, cy), box_mean(generated.rgb8, cx, cy))
                else {
                    continue;
                };
                if background_colours.support(&shown) < BACKGROUND_SUPPORT {
                    continue;
                }
                let at = ((cy * w + cx) * 3) as usize;
                for channel in 0..channels {
                    texture_shown += (returned_sample(&self.image_rgb8, self.reading, channel, at) * 255.0 - shown[channel]).abs();
                    texture_returned +=
                        (returned_sample(generated.rgb8, self.reading, channel, at) * 255.0 - returned[channel]).abs();
                }
                ring.push((shown, returned));
            }
        }
        let samples = ring.len();

        let median = |mut values: Vec<f64>| {
            let middle = values.len() / 2;
            *values.select_nth_unstable_by(middle, f64::total_cmp).1
        };
        let colour_median = |colours: &mut dyn Iterator<Item = [f64; 3]>| {
            let colours: Vec<[f64; 3]> = colours.collect();
            let mut out = [0.0; 3];
            for (channel, out) in out.iter_mut().enumerate().take(channels) {
                *out = median(colours.iter().map(|colour| colour[channel]).collect());
            }
            out
        };
        let abstained = if samples < MIN_TONE_SAMPLES {
            Some(if background.is_empty() { Abstention::NoBackground } else { Abstention::TooFewSamples })
        } else if channels == 3 {
            // The colour the correction brings the ring to, against what the
            // model returned there. Parting two channels further than the
            // answer did is a colour the correction adds, which only a page
            // the lettering clearly sits on justifies: a share of at least
            // [`BACKGROUND_SHARE`] of its surroundings near that colour *and*
            // parted as it is, within [`MAX_ADDED_SEPARATION`], pair by pair.
            // Nearness alone is not enough: art 11 levels off a neutral grey
            // on every channel passes the colour tolerance while parted by 22.
            let shown = colour_median(&mut ring.iter().map(|sample| sample.0));
            let returned = colour_median(&mut ring.iter().map(|sample| sample.1));
            let parting = |colour: &[f64; 3], (i, j): (usize, usize)| colour[i] - colour[j];
            let added: Vec<(usize, usize)> = [(0, 1), (0, 2), (1, 2)]
                .into_iter()
                .filter(|&pair| parting(&shown, pair).abs() > parting(&returned, pair).abs() + MAX_ADDED_SEPARATION)
                .collect();
            let supports = |colour: &[f64; 3]| {
                (0..3).all(|c| (colour[c] - shown[c]).abs() <= BACKGROUND_TOLERANCE)
                    && added.iter().all(|&pair| (parting(colour, pair) - parting(&shown, pair)).abs() <= MAX_ADDED_SEPARATION)
            };
            let share = background.iter().filter(|colour| supports(colour)).count() as f64 / background.len() as f64;
            (!added.is_empty() && share < BACKGROUND_SHARE).then_some(Abstention::Separation)
        } else {
            None
        };

        let curves = (0..channels)
            .map(|channel| {
                if abstained.is_some() {
                    return Vec::new();
                }
                // Per sample: (returned, shown - returned).
                let pairs: Vec<(f64, f64)> =
                    ring.iter().map(|(shown, returned)| (returned[channel], shown[channel] - returned[channel])).collect();
                // A sample further from the channel's typical drift than any
                // drift is not drift: it is lettering of another region the
                // model removed, or art it redrew. Left in, a band those
                // samples happen to fill takes their difference for its own.
                let typical = median(pairs.iter().map(|pair| pair.1).collect());
                let mut bands = vec![Vec::new(); TONE_BANDS];
                for (returned, difference) in pairs {
                    if (difference - typical).abs() <= MAX_TONE_OFFSET {
                        bands[tone_band(returned)].push((returned, difference));
                    }
                }
                bands
                    .into_iter()
                    .filter(|band| band.len() >= MIN_BAND_SAMPLES)
                    .map(|band| {
                        let level = median(band.iter().map(|sample| sample.0).collect());
                        let correction = median(band.iter().map(|sample| sample.1).collect());
                        [level, correction.clamp(-MAX_TONE_OFFSET, MAX_TONE_OFFSET)]
                    })
                    .collect()
            })
            .collect();
        let samples_total = (samples * channels) as f64;
        let source_texture = (samples > 0).then(|| texture_shown / samples_total);
        // Below one level of mean texture the ring is flat paper, and a ratio
        // of two noise floors says nothing about the model.
        let texture = source_texture.filter(|&source| source >= 1.0)
            .map(|_| texture_returned / texture_shown);
        Ok(ToneReport {
            curves,
            samples,
            ring: ring_samples,
            background: background.len(),
            abstained,
            preprocessing: self.preprocessing,
            source_texture,
            texture,
        })
    }

    /// Composite with an explicit `tone`: `None` blends the answer as it came
    /// back, whatever this preparation's version. [`PreparedRender::composite`]
    /// is the call a render makes; this one exists so a V1 capture can be
    /// measured against the V2 alignment.
    pub fn blend(
        &self,
        generated: &GeneratedCrop<'_>,
        tone: Option<ToneReport>,
    ) -> Result<Rendered, Error> {
        self.validate_generated(generated)?;
        let mut patch = self.patch.clone();
        let managed = model::editable_color(&patch)?;
        // Where the curve is read: at the pixel's own level where the ring
        // measured that level, and otherwise at the level its box averages to,
        // which is all the ring ever showed of a texture - a screentone's dots
        // and gaps then take the correction of the tone they make up, while a
        // clipped sky beside darker art keeps its own, right up to the edge.
        let means: Vec<Vec<f64>> = match &tone {
            Some(_) => (0..tone_channels(self.reading))
                .map(|channel| self.box_means(generated.rgb8, channel, self.bounds))
                .collect(),
            None => Vec::new(),
        };

        for y in self.bounds.y..self.bounds.bottom() {
            for x in self.bounds.x..self.bounds.right() {
                if !self.applied.contains(x, y) {
                    continue;
                }
                let alpha = self.ramp.at(x, y);
                if alpha == 0.0 {
                    continue;
                }
                let at = ((y - self.crop.y) * self.crop.w as i64 + (x - self.crop.x)) as usize * 3;
                let (lx, ly) = ((x - self.bounds.x) as u32, (y - self.bounds.y) as u32);
                let dither = dither_at(self.depth, alpha, x, y, self.ceiling);
                let mut rgb = [0.0f32; 3];
                for (channel, value) in rgb.iter_mut().enumerate() {
                    let model = returned_sample(generated.rgb8, self.reading, channel, at);
                    let model = match (&tone, means.get(channel.min(means.len().saturating_sub(1)))) {
                        (Some(tone), Some(means)) => {
                            let own = model * 255.0;
                            let level = if tone.measured(channel, own) {
                                own
                            } else {
                                means[(ly * self.bounds.w + lx) as usize]
                            };
                            (model + tone.correction(channel, level) / 255.0).clamp(0.0, 1.0)
                        }
                        _ => model,
                    };
                    *value = model as f32;
                }
                model::commit_srgb(&managed, &mut patch, lx, ly, rgb, alpha, dither)?;
            }
        }

        Ok(Rendered {
            mask: self.applied.clone(),
            pixels: patch,
            pad: self.pad,
            tiles: 1,
            tone,
        })
    }

    /// The [`TONE_BOX`]-wide box mean of `rgb8` (a crop-sized RGB8 buffer),
    /// read as page channel `channel`, at every pixel of `rect` (page
    /// coordinates, inside the crop), in 8-bit levels. Boxes are cut at the
    /// crop's edge, as the ring's are.
    fn box_means(&self, rgb8: &[u8], channel: usize, rect: Rect) -> Vec<f64> {
        let half = (TONE_BOX / 2) as i64;
        let (cw, ch) = (self.crop.w as i64, self.crop.h as i64);
        let (x0, y0) = ((rect.x - self.crop.x - half).max(0), (rect.y - self.crop.y - half).max(0));
        let x1 = (rect.right() - self.crop.x + half).min(cw);
        let y1 = (rect.bottom() - self.crop.y + half).min(ch);
        let (iw, ih) = ((x1 - x0 + 1) as usize, (y1 - y0 + 1) as usize);
        // Summed-area table over the grown rect, one row and column of zeros
        // ahead of it.
        let mut table = vec![0.0f64; iw * ih];
        for y in 1..ih {
            let mut row = 0.0;
            for x in 1..iw {
                let at = (((y0 + y as i64 - 1) * cw + x0 + x as i64 - 1) * 3) as usize;
                row += returned_sample(rgb8, self.reading, channel, at);
                table[y * iw + x] = table[(y - 1) * iw + x] + row;
            }
        }
        let mut out = Vec::with_capacity(rect.w as usize * rect.h as usize);
        for py in rect.y..rect.bottom() {
            for px in rect.x..rect.right() {
                let (cx, cy) = (px - self.crop.x, py - self.crop.y);
                let (ax, ay) = (((cx - half).max(0) - x0) as usize, ((cy - half).max(0) - y0) as usize);
                let (bx, by) = (((cx + half + 1).min(cw) - x0) as usize, ((cy + half + 1).min(ch) - y0) as usize);
                let sum = table[by * iw + bx] - table[ay * iw + bx] - table[by * iw + ax] + table[ay * iw + ax];
                out.push(sum / ((bx - ax) * (by - ay)) as f64 * 255.0);
            }
        }
        out
    }

    /// Each crop pixel's Chebyshev distance to the applied mask, saturating
    /// one past [`TONE_RING_OUTER`]: a two-pass chamfer, which is exact for
    /// this metric.
    fn distance_to_applied(&self) -> Vec<u16> {
        let (w, h) = (self.crop.w as usize, self.crop.h as usize);
        let far = TONE_RING_OUTER + 1;
        let mut distance = vec![far; w * h];
        for y in 0..h {
            for x in 0..w {
                if self.applied.contains(self.crop.x + x as i64, self.crop.y + y as i64) {
                    distance[y * w + x] = 0;
                }
            }
        }
        let relax = |distance: &mut Vec<u16>, x: usize, y: usize, nx: isize, ny: isize| {
            if nx < 0 || ny < 0 || nx as usize >= w || ny as usize >= h {
                return;
            }
            let through = distance[ny as usize * w + nx as usize].saturating_add(1).min(far);
            if through < distance[y * w + x] {
                distance[y * w + x] = through;
            }
        };
        for y in 0..h {
            for x in 0..w {
                let (sx, sy) = (x as isize, y as isize);
                for (dx, dy) in [(-1, -1), (0, -1), (1, -1), (-1, 0)] {
                    relax(&mut distance, x, y, sx + dx, sy + dy);
                }
            }
        }
        for y in (0..h).rev() {
            for x in (0..w).rev() {
                let (sx, sy) = (x as isize, y as isize);
                for (dx, dy) in [(1, 1), (0, 1), (-1, 1), (1, 0)] {
                    relax(&mut distance, x, y, sx + dx, sy + dy);
                }
            }
        }
        distance
    }
}

/// The nearest ring pixel a tone sample is taken at, in px outside the applied
/// mask. With [`TONE_BOX`] 7 the box of a sample here reaches no closer to the
/// mask than 3 px.
pub const TONE_RING_INNER: u16 = 6;

/// The farthest ring pixel a tone sample is taken at: near enough that the
/// page it sees is the page around this edit.
pub const TONE_RING_OUTER: u16 = 18;

/// The side of the box a tone sample averages over, in px: wider than the
/// period of any screentone in the demo chapter.
pub const TONE_BOX: u32 = 7;

/// Ring pixels are sampled on this grid; neighbouring boxes overlap almost
/// entirely and say the same thing.
const TONE_STRIDE: usize = 2;

/// Fewer ring samples than this and the drift is not trusted: nothing is
/// added.
pub const MIN_TONE_SAMPLES: usize = 64;

/// How many bands of returned level the drift is measured in: 32 levels each.
const TONE_BANDS: usize = 8;

/// The band of returned level an 8-bit `level` falls in.
fn tone_band(level: f64) -> usize {
    ((level / 256.0 * TONE_BANDS as f64) as usize).min(TONE_BANDS - 1)
}

/// Fewer samples than this in a band and the band says nothing.
pub const MIN_BAND_SAMPLES: usize = 16;

/// The largest correction added, in 8-bit levels. The drift measured on the
/// demo chapter was 11 to 18 levels either way; a median beyond this is not
/// drift but a ring the model rewrote (dense lettering of another region it
/// removed too, or a crop it redrew), and adding all of it would paint the
/// page's tone over whatever the model did.
pub const MAX_TONE_OFFSET: f64 = 24.0;

/// How near, in 8-bit levels on every channel, a ring sample's colour must be
/// to a colour of the lettering's surroundings to count as background. Wider
/// than a 7 px box's swing over the demo chapter's screentones (about 10
/// levels either way of their mean) and than a gradient changes across the
/// ring; narrower than the half-cap tint an artwork ring would add.
pub const BACKGROUND_TOLERANCE: f64 = 12.0;

/// How many samples of the lettering's surroundings must share a ring
/// sample's colour: one stray pixel of lettering the fit left outside the
/// hole does not make a colour background.
pub const BACKGROUND_SUPPORT: usize = 3;

/// The most, in levels, the correction may part two channels beyond what the
/// raw answer parts them without the lettering's surroundings being that
/// colour ([`BACKGROUND_SHARE`]), and how closely a surrounding sample's own
/// parting of those channels must match the ring's to count as that colour.
/// More is a tint, not drift: the ring has a colour the page under the
/// lettering does not.
pub const MAX_ADDED_SEPARATION: f64 = 4.0;

/// The share of the lettering's surroundings that must be the ring's colour,
/// channel partings included, before a correction may add colour the answer
/// did not have. A third: a
/// line of lettering across two backgrounds has half of its surroundings on
/// each.
pub const BACKGROUND_SHARE: f64 = 1.0 / 3.0;

/// Why an alignment corrected nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Abstention {
    /// No opaque page between the lettering and the applied mask's edge.
    NoBackground,
    /// Fewer than [`MIN_TONE_SAMPLES`] ring samples were that page.
    TooFewSamples,
    /// The correction would tint: see [`MAX_ADDED_SEPARATION`] and
    /// [`BACKGROUND_SHARE`].
    Separation,
}

/// The colours of the lettering's surroundings, bucketed by
/// [`BACKGROUND_TOLERANCE`] so a ring sample finds the ones near it without
/// comparing against every one.
struct ColourIndex {
    channels: usize,
    cells: std::collections::HashMap<[i32; 3], Vec<[f64; 3]>>,
}

impl ColourIndex {
    fn cell(colour: &[f64; 3], channels: usize) -> [i32; 3] {
        let mut cell = [0; 3];
        for channel in 0..channels {
            cell[channel] = (colour[channel] / BACKGROUND_TOLERANCE).floor() as i32;
        }
        cell
    }

    fn new(colours: &[[f64; 3]], channels: usize) -> Self {
        let mut cells: std::collections::HashMap<[i32; 3], Vec<[f64; 3]>> = std::collections::HashMap::new();
        for colour in colours {
            cells.entry(Self::cell(colour, channels)).or_default().push(*colour);
        }
        ColourIndex { channels, cells }
    }

    /// How many of the colours are within [`BACKGROUND_TOLERANCE`] of
    /// `colour` on every channel, counted up to [`BACKGROUND_SUPPORT`].
    fn support(&self, colour: &[f64; 3]) -> usize {
        let centre = Self::cell(colour, self.channels);
        let span = |channel: usize| if channel < self.channels { -1..=1 } else { 0..=0 };
        let mut found = 0;
        for dr in span(0) {
            for dg in span(1) {
                for db in span(2) {
                    let key = [centre[0] + dr, centre[1] + dg, centre[2] + db];
                    for other in self.cells.get(&key).into_iter().flatten() {
                        if (0..self.channels).all(|c| (other[c] - colour[c]).abs() <= BACKGROUND_TOLERANCE) {
                            found += 1;
                            if found >= BACKGROUND_SUPPORT {
                                return found;
                            }
                        }
                    }
                }
            }
        }
        found
    }
}

/// How many channels a tone offset is measured for: one on a grey page, whose
/// answer is read as the mean of the model's three.
fn tone_channels(mode: ColorMode) -> usize {
    match mode {
        ColorMode::Gray | ColorMode::GrayAlpha => 1,
        _ => 3,
    }
}

/// What V2's tone alignment measured and did, for the patch's provenance.
///
/// The evidence it answers: over the stopped demo chapter's 227 cloud crops,
/// the model's answer on flat paper came back a median 10.9 levels (p90 14.1)
/// brighter than the page it was shown, which composites as a pale
/// glyph-shaped patch however well the mask covers the lettering. The
/// correction is added to the answer before the ramp blends it, so the core
/// and the feather both land on the page's own tone and pixels outside the
/// applied mask are still never touched.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToneReport {
    /// Per page channel (one on a grey page), the correction curve:
    /// `[returned level, levels added]` at each band of returned levels the
    /// ring populated, ascending. Empty when the alignment abstained. See
    /// [`ToneReport::correction`].
    pub curves: Vec<Vec<[f64; 2]>>,
    /// Ring samples the curves were measured over: those that showed the
    /// page the lettering sits on.
    pub samples: usize,
    /// Ring samples inside the page, background or not.
    pub ring: usize,
    /// Samples of the page between the lettering and the applied mask's
    /// edge, the background a ring sample is matched against.
    pub background: usize,
    /// Why nothing was corrected, when nothing was.
    pub abstained: Option<Abstention>,
    /// The preprocessing the answer was prepared and aligned under.
    #[serde(skip)]
    pub preprocessing: Preprocessing,
    /// The answer's high-frequency texture over the ring as a fraction of the
    /// page's, where the page has any: well below 1 is a model that washed the
    /// crop out, which no alignment of its levels restores.
    pub texture: Option<f64>,
    /// The source's mean absolute center-minus-box residual over the accepted
    /// ring samples, in 8-bit levels per channel. None when there are no samples.
    pub source_texture: Option<f64>,
}

impl ToneReport {
    /// The record a patch's provenance keeps of this alignment, under
    /// `params_snapshot["tone"]`: the same shape for a cloud and a local
    /// render, with the preprocessing version it was measured under.
    pub fn provenance(&self) -> serde_json::Value {
        serde_json::json!({
            "preprocessing_version": self.preprocessing.version(),
            "curves": self.curves,
            "samples": self.samples,
            "ring": self.ring,
            "background": self.background,
            "abstained": self.abstained,
            "texture": self.texture,
            "source_texture": self.source_texture,
        })
    }

    fn curve(&self, channel: usize) -> Option<&Vec<[f64; 2]>> {
        self.curves.get(channel.min(self.curves.len().saturating_sub(1)))
    }

    /// Whether the ring measured returned `level` of page channel `channel`:
    /// one of the curve's points is in the same band.
    pub fn measured(&self, channel: usize, level: f64) -> bool {
        self.curve(channel)
            .is_some_and(|curve| curve.iter().any(|point| tone_band(point[0]) == tone_band(level)))
    }

    /// The levels added to a returned `level` of page channel `channel`:
    /// interpolated between the curve's points, and tapering to nothing
    /// towards 0 and 255 past its outermost ones, where a clipped answer says
    /// nothing about drift and the ring said nothing about that level.
    pub fn correction(&self, channel: usize, level: f64) -> f64 {
        let Some(curve) = self.curve(channel) else {
            return 0.0;
        };
        let (Some(first), Some(last)) = (curve.first(), curve.last()) else {
            return 0.0;
        };
        if level <= first[0] {
            return if first[0] <= 0.0 { first[1] } else { first[1] * level.max(0.0) / first[0] };
        }
        if level >= last[0] {
            return if last[0] >= 255.0 {
                last[1]
            } else {
                last[1] * (255.0 - level.min(255.0)) / (255.0 - last[0])
            };
        }
        curve
            .windows(2)
            .find(|pair| level <= pair[1][0])
            .map_or(0.0, |pair| {
                let ([x0, y0], [x1, y1]) = (pair[0], pair[1]);
                if x1 <= x0 { y1 } else { y0 + (y1 - y0) * (level - x0) / (x1 - x0) }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::model::write_bound;
    use crate::fit;

    fn make_page(
        w: u32,
        h: u32,
        mode: ColorMode,
        depth: BitDepth,
        fill_fn: impl Fn(u32, u32, usize) -> u16,
    ) -> Raster {
        let samples = mode.samples();
        let bits = depth.bits() as usize;
        let stride = (w as usize * samples * bits).div_ceil(8);
        let mut raster = Raster {
            width: w,
            height: h,
            mode,
            depth,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None, color: Default::default(),
            data: vec![0u8; stride * h as usize],
        };
        for y in 0..h {
            for x in 0..w {
                for c in 0..samples {
                    raster.set_sample(x, y, c, fill_fn(x, y, c));
                }
            }
        }
        raster
    }

    fn fitted_over(page: &Raster, rect: Rect) -> Fitted {
        let seed = Mask::filled(rect);
        fit::fit(
            page,
            &seed,
            1.0,
            0.0,
            &fit::EdgeMap::none(page.width, page.height),
            true,
        )
    }

    #[test]
    fn managed_crop_and_model_commit_use_srgb_with_native_storage() {
        let mut page = make_page(80,80,ColorMode::Rgb,BitDepth::Eight, |_,_,_|64);
        page.color.gamma = Some(100000);
        let managed = model::editable_color(&page).unwrap();
        let (_,rgb) = crop_samples(&page,Rect::new(0,0,2,2),&managed);
        // IEC sRGB encode of 64/255 linear is 137.21/255.
        assert_eq!(rgb,vec![137;12]);
        let fitted = fitted_over(&page,Rect::new(30,30,20,20));
        let prepared = PreparedRender::prepare_as(&page,&fitted,Preprocessing::V1).unwrap();
        let samples = vec![128;prepared.crop.w as usize*prepared.crop.h as usize*3];
        let made = prepared.composite(&GeneratedCrop::new(prepared.crop.w,prepared.crop.h,&samples)).unwrap();
        let (x,y)=((40-made.mask.bounds.x) as u32,(40-made.mask.bounds.y) as u32);
        for c in 0..3 { assert_eq!(made.pixels.sample(x,y,c),55); }
        assert_eq!(made.pixels.color,page.color);
        for y in 0..made.pixels.height { for x in 0..made.pixels.width {
            if !made.mask.contains(made.mask.bounds.x+x as i64,made.mask.bounds.y+y as i64) {
                for c in 0..3 { assert_eq!(made.pixels.sample(x,y,c),64); }
            }
        }}
    }

    #[test]
    fn pure_crop_prep_and_dimensions_geometry() {
        let page = make_page(500, 600, ColorMode::Rgb, BitDepth::Eight, |x, y, c| {
            ((x * 7 + y * 13 + c as u32 * 31) % 256) as u16
        });
        // Far enough from every page edge for V2's context to fit.
        let rect = Rect::new(200, 250, 50, 70);
        let fitted = fitted_over(&page, rect);

        let prepared = PreparedRender::prepare(&page, &fitted).expect("prepare should succeed");
        assert_eq!(prepared.crop().w % LATENT_STRIDE, 0);
        assert_eq!(prepared.crop().h % LATENT_STRIDE, 0);
        assert_eq!(
            prepared.image_rgb8().len(),
            (prepared.crop().w * prepared.crop().h * 3) as usize
        );
        assert_eq!(
            prepared.hint_gray8().len(),
            (prepared.crop().w * prepared.crop().h) as usize
        );
        assert_eq!(prepared.pad(), EdgePad::None);
    }

    #[test]
    fn outside_applied_mask_samples_bit_identical_and_inside_blended() {
        let page = make_page(400, 400, ColorMode::Rgb, BitDepth::Eight, |x, y, c| {
            ((x * 11 + y * 17 + c as u32 * 43) % 256) as u16
        });
        let rect = Rect::new(120, 120, 40, 40);
        let fitted = fitted_over(&page, rect);
        let prepared = PreparedRender::prepare(&page, &fitted).unwrap();

        // Synthetic generated crop that is black everywhere (0)
        let generated_pixels = vec![0u8; (prepared.crop().w * prepared.crop().h * 3) as usize];
        let generated = GeneratedCrop::new(prepared.crop().w, prepared.crop().h, &generated_pixels);

        let rendered = prepared.composite(&generated).unwrap();
        let bounds = rendered.mask.bounds;
        let mut inside_changed = 0usize;

        for y in 0..rendered.pixels.height {
            for x in 0..rendered.pixels.width {
                let (px, py) = (bounds.x + x as i64, bounds.y + y as i64);
                let inside = rendered.mask.contains(px, py);
                for c in 0..3 {
                    let actual = rendered.pixels.sample(x, y, c);
                    let orig = page.sample(px as u32, py as u32, c);
                    if inside {
                        if actual != orig {
                            inside_changed += 1;
                        }
                    } else {
                        assert_eq!(
                            actual, orig,
                            "Sample outside mask changed at {px},{py} channel {c}"
                        );
                    }
                }
            }
        }
        assert!(inside_changed > 0, "Expected changes inside the mask");
    }

    /// A grey scan stored as RGBA keeps a grey patch though the model answers
    /// in colour; a colour page keeps the colour it was given.
    #[test]
    fn a_tinted_answer_on_a_grey_rgba_page_is_written_grey() {
        let grey = make_page(300, 300, ColorMode::Rgba, BitDepth::Eight, |x, y, c| {
            if c == 3 { 255 } else { ((x * 3 + y * 5) % 180 + 40) as u16 }
        });
        let tinted = make_page(300, 300, ColorMode::Rgba, BitDepth::Eight, |x, y, c| match c {
            3 => 255,
            0 => ((x * 3 + y * 5) % 180 + 50) as u16,
            _ => ((x * 3 + y * 5) % 180 + 40) as u16,
        });
        for (page, stays_grey) in [(grey, true), (tinted, false)] {
            let fitted = fitted_over(&page, Rect::new(120, 120, 40, 40));
            let prepared = PreparedRender::prepare(&page, &fitted).unwrap();
            let answer = [210u8, 190, 200].repeat((prepared.crop().w * prepared.crop().h) as usize);
            let rendered =
                prepared.composite(&GeneratedCrop::new(prepared.crop().w, prepared.crop().h, &answer)).unwrap();
            let bounds = rendered.mask.bounds;
            let parted = (0..rendered.pixels.height)
                .flat_map(|y| (0..rendered.pixels.width).map(move |x| (x, y)))
                .filter(|&(x, y)| rendered.mask.contains(bounds.x + x as i64, bounds.y + y as i64))
                .filter(|&(x, y)| {
                    let [r, g, b] = [0, 1, 2].map(|c| rendered.pixels.sample(x, y, c));
                    r != g || g != b
                })
                .count();
            assert_eq!(parted == 0, stays_grey, "grey page {stays_grey}: {parted} pixels parted");
        }
    }

    #[test]
    fn alpha_channel_preserved_on_rgba_and_gray_alpha() {
        for mode in [ColorMode::Rgba, ColorMode::GrayAlpha] {
            let page = make_page(300, 300, mode, BitDepth::Eight, |x, y, c| {
                if mode.alpha_channel() == Some(c) {
                    ((x * 3 + y * 5) % 200 + 55) as u16 // Specific varied alpha
                } else {
                    ((x + y * 2 + c as u32) % 256) as u16
                }
            });
            let rect = Rect::new(80, 80, 50, 50);
            let fitted = fitted_over(&page, rect);
            let prepared = PreparedRender::prepare(&page, &fitted).unwrap();

            // Generated crop is pure white (255)
            let generated_pixels =
                vec![255u8; (prepared.crop().w * prepared.crop().h * 3) as usize];
            let generated =
                GeneratedCrop::new(prepared.crop().w, prepared.crop().h, &generated_pixels);

            let rendered = prepared.composite(&generated).unwrap();
            let bounds = rendered.mask.bounds;
            let alpha_ch = mode.alpha_channel().unwrap();

            for y in 0..rendered.pixels.height {
                for x in 0..rendered.pixels.width {
                    let (px, py) = (bounds.x + x as i64, bounds.y + y as i64);
                    let orig_alpha = page.sample(px as u32, py as u32, alpha_ch);
                    let rendered_alpha = rendered.pixels.sample(x, y, alpha_ch);
                    assert_eq!(
                        rendered_alpha, orig_alpha,
                        "Alpha channel modified at {px},{py} in {mode:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn edge_crop_mapping_and_replicated_padding_recorded() {
        let page = make_page(200, 200, ColorMode::Gray, BitDepth::Eight, |x, y, _| {
            if x == 0 || y == 0 {
                42
            } else {
                180
            }
        });
        // Region tightly abutting (2, 2), causing the context crop to extend into negative coords
        let rect = Rect::new(2, 2, 20, 20);
        let fitted = fitted_over(&page, rect);
        let prepared = PreparedRender::prepare(&page, &fitted).unwrap();

        assert_eq!(
            prepared.pad(),
            EdgePad::Replicate,
            "Should detect edge replication"
        );
        assert!(
            prepared.crop().x < 0 || prepared.crop().y < 0,
            "Crop should extend past page origin"
        );

        // The edge-replicated samples in the negative coordinate portion should match (0, 0)
        let sample_at_0_0 = prepared.image_rgb8()[0];
        assert_eq!(
            sample_at_0_0, 42,
            "Out-of-bounds sample should be replicated from (0,0)"
        );
    }

    #[test]
    fn mismatched_generated_dimensions_and_lengths_rejected() {
        let page = make_page(300, 300, ColorMode::Rgb, BitDepth::Eight, |_, _, _| 128);
        let rect = Rect::new(50, 50, 40, 40);
        let fitted = fitted_over(&page, rect);
        let prepared = PreparedRender::prepare(&page, &fitted).unwrap();

        // Wrong width
        let wrong_w_bytes = vec![0u8; ((prepared.crop().w + 16) * prepared.crop().h * 3) as usize];
        let gen_wrong_w =
            GeneratedCrop::new(prepared.crop().w + 16, prepared.crop().h, &wrong_w_bytes);
        match prepared.composite(&gen_wrong_w) {
            Err(Error::Run(err)) => assert!(err.contains("asked for"), "{err}"),
            other => panic!("Expected Error::Run for wrong width, got {other:?}"),
        }

        // Wrong height
        let wrong_h_bytes = vec![0u8; (prepared.crop().w * (prepared.crop().h + 16) * 3) as usize];
        let gen_wrong_h =
            GeneratedCrop::new(prepared.crop().w, prepared.crop().h + 16, &wrong_h_bytes);
        match prepared.composite(&gen_wrong_h) {
            Err(Error::Run(err)) => assert!(err.contains("asked for"), "{err}"),
            other => panic!("Expected Error::Run for wrong height, got {other:?}"),
        }

        // Correct dimensions but short byte length
        let short_bytes = vec![0u8; (prepared.crop().w * prepared.crop().h * 3 - 5) as usize];
        let gen_short_bytes =
            GeneratedCrop::new(prepared.crop().w, prepared.crop().h, &short_bytes);
        match prepared.composite(&gen_short_bytes) {
            Err(Error::Run(err)) => assert!(err.contains("byte length"), "{err}"),
            other => panic!("Expected Error::Run for short buffer, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_source_modes_and_depths_refused() {
        // 16-bit depth
        let page_16bit = make_page(200, 200, ColorMode::Gray, BitDepth::Sixteen, |_, _, _| {
            30000
        });
        let fitted = fitted_over(&page_16bit, Rect::new(20, 20, 30, 30));
        assert!(!applies(&page_16bit));
        assert_eq!(
            declines(&page_16bit, &fitted),
            Some(Decline::DepthBeyondEngine(BitDepth::Sixteen))
        );
        assert!(matches!(
            PreparedRender::prepare(&page_16bit, &fitted),
            Err(Error::Declined(Decline::DepthBeyondEngine(
                BitDepth::Sixteen
            )))
        ));

        // 1-bit depth
        let page_1bit = make_page(200, 200, ColorMode::Gray, BitDepth::One, |_, _, _| 1);
        let fitted = fitted_over(&page_1bit, Rect::new(20, 20, 30, 30));
        assert!(!applies(&page_1bit));
        assert_eq!(
            declines(&page_1bit, &fitted),
            Some(Decline::Depth(BitDepth::One))
        );

        // Indexed mode
        let page_indexed = make_page(200, 200, ColorMode::Indexed, BitDepth::Eight, |_, _, _| 0);
        let fitted = fitted_over(&page_indexed, Rect::new(20, 20, 30, 30));
        assert!(!applies(&page_indexed));
        assert_eq!(
            declines(&page_indexed, &fitted),
            Some(Decline::Mode(ColorMode::Indexed))
        );

        // CMYK mode
        let page_cmyk = make_page(200, 200, ColorMode::Cmyk, BitDepth::Eight, |_, _, _| 0);
        let fitted = fitted_over(&page_cmyk, Rect::new(20, 20, 30, 30));
        assert!(!applies(&page_cmyk));
        assert_eq!(
            declines(&page_cmyk, &fitted),
            Some(Decline::Mode(ColorMode::Cmyk))
        );

        // Oversized region
        let page_large = make_page(3000, 3000, ColorMode::Gray, BitDepth::Eight, |_, _, _| 128);
        let fitted_large = fitted_over(&page_large, Rect::new(0, 0, MAX_REGION + 1, 10));
        assert!(matches!(
            declines(&page_large, &fitted_large),
            Some(Decline::TooLarge { .. })
        ));
    }

    #[test]
    fn edit_stays_strictly_inside_write_bound() {
        let page = make_page(500, 500, ColorMode::Gray, BitDepth::Eight, |_, _, _| 200);
        let fitted = fitted_over(&page, Rect::new(150, 150, 60, 40));
        let prepared = PreparedRender::prepare(&page, &fitted).unwrap();

        let gen_pixels = vec![0u8; (prepared.crop().w * prepared.crop().h * 3) as usize];
        let generated = GeneratedCrop::new(prepared.crop().w, prepared.crop().h, &gen_pixels);
        let rendered = prepared.composite(&generated).unwrap();

        let permitted = write_bound(&fitted, page.width, page.height);
        let bounds = rendered.mask.bounds;
        for y in 0..rendered.pixels.height {
            for x in 0..rendered.pixels.width {
                if rendered.pixels.sample(x, y, 0) == 200 {
                    continue;
                }
                let (px, py) = (bounds.x + x as i64, bounds.y + y as i64);
                assert!(
                    permitted.contains(px, py),
                    "Wrote outside write bound at {px},{py}"
                );
            }
        }
    }

    #[test]
    fn prepared_snapshot_is_stable_even_if_original_page_mutates() {
        let mut page = make_page(400, 400, ColorMode::Rgb, BitDepth::Eight, |_, _, _| 200);
        let fitted = fitted_over(&page, Rect::new(100, 100, 40, 40));
        let prepared = PreparedRender::prepare(&page, &fitted).unwrap();

        // Mutate original page samples drastically after prepare
        for b in page.data.iter_mut() {
            *b = 0;
        }

        let gen_pixels = vec![50u8; (prepared.crop().w * prepared.crop().h * 3) as usize];
        let generated = GeneratedCrop::new(prepared.crop().w, prepared.crop().h, &gen_pixels);
        let rendered = prepared.composite(&generated).unwrap();

        // Pixels outside the mask must match the captured page state (200), not the mutated page (0)
        let bounds = rendered.mask.bounds;
        for y in 0..rendered.pixels.height {
            for x in 0..rendered.pixels.width {
                let (px, py) = (bounds.x + x as i64, bounds.y + y as i64);
                if !rendered.mask.contains(px, py) {
                    assert_eq!(
                        rendered.pixels.sample(x, y, 0),
                        200,
                        "Outside-mask sample leaked mutated page state at {px},{py}"
                    );
                }
            }
        }
    }

    #[test]
    fn zero_dimension_page_and_empty_bounds_refused() {
        let zero_w_page = Raster {
            width: 0,
            height: 100,
            mode: ColorMode::Gray,
            depth: BitDepth::Eight,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None, color: Default::default(),
            data: Vec::new(),
        };
        let fitted = fitted_over(
            &make_page(100, 100, ColorMode::Gray, BitDepth::Eight, |_, _, _| 0),
            Rect::new(10, 10, 20, 20),
        );
        match PreparedRender::prepare(&zero_w_page, &fitted) {
            Err(Error::Run(err)) => assert!(err.contains("zero-dimension"), "{err}"),
            other => panic!("Expected Error::Run for zero width, got {other:?}"),
        }

        let zero_h_page = Raster {
            width: 100,
            height: 0,
            mode: ColorMode::Gray,
            depth: BitDepth::Eight,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None, color: Default::default(),
            data: Vec::new(),
        };
        match PreparedRender::prepare(&zero_h_page, &fitted) {
            Err(Error::Run(err)) => assert!(err.contains("zero-dimension"), "{err}"),
            other => panic!("Expected Error::Run for zero height, got {other:?}"),
        }
    }

    #[test]
    fn execution_target_and_cloud_provider_serde() {
        let local = ExecutionTarget::Local;
        let local_json = serde_json::to_string(&local).unwrap();
        assert_eq!(local_json, r#"{"type":"local"}"#);
        let local_de: ExecutionTarget = serde_json::from_str(&local_json).unwrap();
        assert_eq!(local_de, ExecutionTarget::Local);
        assert!(local_de.is_local());
        assert_eq!(local_de.provider(), None);
        assert_eq!(local_de.profile_id(), None);

        let beam = ExecutionTarget::Beam {
            profile_id: "profile-1".to_string(),
        };
        let beam_json = serde_json::to_string(&beam).unwrap();
        assert_eq!(beam_json, r#"{"type":"beam","profile_id":"profile-1"}"#);
        let beam_de: ExecutionTarget = serde_json::from_str(&beam_json).unwrap();
        assert_eq!(beam_de, beam);
        assert!(!beam_de.is_local());
        assert_eq!(beam_de.provider(), Some(CloudProvider::Beam));
        assert_eq!(beam_de.profile_id(), Some("profile-1"));

        let modal = ExecutionTarget::Modal {
            profile_id: "profile-2".to_string(),
        };
        let modal_json = serde_json::to_string(&modal).unwrap();
        assert_eq!(modal_json, r#"{"type":"modal","profile_id":"profile-2"}"#);
        let modal_de: ExecutionTarget = serde_json::from_str(&modal_json).unwrap();
        assert_eq!(modal_de, modal);
        assert_eq!(modal_de.provider(), Some(CloudProvider::Modal));
        assert_eq!(modal_de.profile_id(), Some("profile-2"));

        assert_eq!(CloudProvider::Beam.as_str(), "beam");
        assert_eq!(CloudProvider::Modal.as_str(), "modal");
        assert_eq!(
            serde_json::to_string(&CloudProvider::Beam).unwrap(),
            r#""beam""#
        );
        assert_eq!(
            serde_json::to_string(&CloudProvider::Modal).unwrap(),
            r#""modal""#
        );
    }

    #[test]
    fn render_recipe_identity_contract() {
        let recipe = RenderRecipe::new(
            "flux-sdnq-klein-v1",
            "cleaner-crop-v1",
            "FLUX.2-klein-4b",
            "pinned-rev-1",
            false,
        );
        assert_eq!(recipe.recipe_id, "flux-sdnq-klein-v1");
        assert_eq!(recipe.preprocessing_version, "cleaner-crop-v1");
        assert_eq!(recipe.model_id, "FLUX.2-klein-4b");
        assert_eq!(recipe.model_revision, "pinned-rev-1");
        assert!(!recipe.native_mask_conditioning);

        let json = serde_json::to_string(&recipe).unwrap();
        let decoded: RenderRecipe = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, recipe);
    }

    /* -- V2 tone alignment, over the backgrounds a fringe was reviewed on -- */

    /// One synthetic lettering scene: the page with its lettering, the page
    /// as it was under the lettering, and the lettering's segmentation mask.
    struct Scene {
        name: &'static str,
        page: Raster,
        clean: Raster,
        /// The glyph core only, as a segmentation mask ends: the one-pixel
        /// anti-aliasing ring drawn around it is outside, which is the
        /// coverage a real SAM mask has at scale 1.56.
        seed: Mask,
        /// Every pixel the lettering touched, ring included.
        lettering: Mask,
    }

    const SIDE: u32 = 360;

    /// Draw `glyph` (its core, and a one-pixel anti-aliased ring at the mean
    /// of ink and ground) over `ground` on a grey page, or on an RGB page
    /// when `mode` says so.
    fn scene(
        name: &'static str,
        mode: ColorMode,
        ground: impl Fn(u32, u32, usize) -> u16,
        ink: [u16; 3],
        glyph: &Mask,
    ) -> Scene {
        let clean = make_page(SIDE, SIDE, mode, BitDepth::Eight, &ground);
        let mut page = clean.clone();
        let ring = glyph.dilated(1, SIDE, SIDE);
        for y in ring.bounds.y..ring.bounds.bottom() {
            for x in ring.bounds.x..ring.bounds.right() {
                if !ring.contains(x, y) {
                    continue;
                }
                let core = glyph.contains(x, y);
                for c in 0..mode.samples() {
                    if mode.alpha_channel() == Some(c) {
                        continue;
                    }
                    let under = clean.sample(x as u32, y as u32, c);
                    let ink = ink[c.min(2)];
                    page.set_sample(x as u32, y as u32, c, if core { ink } else { (ink + under) / 2 });
                }
            }
        }
        Scene { name, page, clean, seed: glyph.clone(), lettering: ring }
    }

    /// A line of lettering: three thin strokes and a stroke with a bar, the
    /// thinnest two pixels wide.
    fn lettering_at(x: i64, y: i64) -> Mask {
        let mut glyph = Mask::empty(Rect::new(x, y, 70, 30));
        for (gx, gy, w, h) in [(0, 0, 2, 30), (12, 0, 3, 30), (12, 13, 14, 3), (40, 4, 2, 22), (52, 0, 18, 2)] {
            for yy in 0..h {
                for xx in 0..w {
                    glyph.set(x + gx + xx, y + gy + yy, true);
                }
            }
        }
        glyph
    }

    /// Punctuation: a lone three-pixel dot.
    fn dot_at(x: i64, y: i64) -> Mask {
        Mask::filled(Rect::new(x, y, 3, 3))
    }

    /// `scene` with another region's lettering drawn beside it: dark ink over
    /// `rect`, outside this region's applied mask but across its ring, which
    /// the model removes along with this region's and the page as it was
    /// does not have either.
    fn beside_other_lettering(mut scene: Scene, rect: Rect) -> Scene {
        for y in rect.y..rect.bottom() {
            for x in rect.x..rect.right() {
                scene.page.set_sample(x as u32, y as u32, 0, 20);
            }
        }
        scene
    }

    fn scenes() -> Vec<Scene> {
        let text = lettering_at(140, 160);
        let gradient = |x: u32, _: u32, _: usize| (60 + x * 150 / SIDE) as u16;
        vec![
            scene("flat grey", ColorMode::Gray, |_, _, _| 170, [20; 3], &text),
            scene("black", ColorMode::Gray, |_, _, _| 12, [245; 3], &text),
            scene(
                "screentone",
                ColorMode::Gray,
                |x, y, _| if x % 6 < 3 && y % 6 < 3 { 90 } else { 220 },
                [15; 3],
                &text,
            ),
            scene("gradient", ColorMode::Gray, |x, _, _| (60 + x * 150 / SIDE) as u16, [10; 3], &text),
            scene(
                "line art",
                ColorMode::Gray,
                |x, y, _| if (x + y) % 23 < 2 || x % 31 < 2 { 30 } else { 235 },
                [5; 3],
                &text,
            ),
            scene("punctuation", ColorMode::Gray, |_, _, _| 200, [10; 3], &dot_at(180, 180)),
            scene("page edge", ColorMode::Gray, |_, _, _| 190, [10; 3], &lettering_at(2, 3)),
            scene(
                "colour on colour",
                ColorMode::Rgb,
                |_, _, c| [60, 110, 200][c],
                [240, 220, 40],
                &text,
            ),
            // Another region's dense lettering fills the ring's whole right
            // side, and on a gradient the ring's samples there are the only
            // ones at that level: without the drift test they would set the
            // curve for it.
            beside_other_lettering(
                scene("gradient beside other lettering", ColorMode::Gray, gradient, [10; 3], &text),
                Rect::new(222, 120, 20, 122),
            ),
            // A sky at full blue beside darker art, the lettering across both:
            // the blue channel clips in the sky, shown and returned alike.
            scene(
                "clipped sky beside art",
                ColorMode::Rgb,
                |x, _, c| if x < 170 { [86, 211, 255][c] } else { [60, 90, 140][c] },
                [240, 220, 40],
                &text,
            ),
        ]
    }

    /// What the model sends back for a scene: the page as it was under the
    /// lettering, drifted by `drift` levels per channel - the answer the
    /// demo chapter's crops came back as, brighter than the page they were
    /// cut from.
    fn drifted(prepared: &PreparedRender, clean: &Raster, drift: [i32; 3]) -> Vec<u8> {
        let crop = prepared.crop();
        let mut out = vec![0u8; (crop.w * crop.h * 3) as usize];
        for y in 0..crop.h as i64 {
            for x in 0..crop.w as i64 {
                let px = (crop.x + x).clamp(0, clean.width as i64 - 1) as u32;
                let py = (crop.y + y).clamp(0, clean.height as i64 - 1) as u32;
                for (c, drift) in drift.iter().enumerate() {
                    let under = clean.sample(px, py, model::source_channel(clean.mode, c)) as i32;
                    out[((y * crop.w as i64 + x) * 3) as usize + c] = (under + drift).clamp(0, 255) as u8;
                }
            }
        }
        out
    }

    /// The largest difference from the page as it was, over the applied
    /// mask, in levels.
    fn residual(scene: &Scene, rendered: &Rendered) -> u16 {
        let bounds = rendered.mask.bounds;
        let mut worst = 0;
        for y in bounds.y..bounds.bottom() {
            for x in bounds.x..bounds.right() {
                if !rendered.mask.contains(x, y) {
                    continue;
                }
                for c in 0..scene.page.mode.samples() {
                    let got = rendered.pixels.sample((x - bounds.x) as u32, (y - bounds.y) as u32, c);
                    worst = worst.max(got.abs_diff(scene.clean.sample(x as u32, y as u32, c)));
                }
            }
        }
        worst
    }

    fn manual_fit(scene: &Scene) -> Fitted {
        fit::fit(&scene.page, &scene.seed, 1.0, 0.0, &fit::EdgeMap::none(SIDE, SIDE), true)
    }

    #[test]
    fn qwen_preview_shows_the_actual_blend_and_keeps_every_pixel_outside_the_mask() {
        for scene in scenes() {
            let fitted = manual_fit(&scene);
            let prepared = PreparedRender::prepare(&scene.page, &fitted).unwrap();
            let answer = drifted(&prepared, &scene.clean, [14; 3]);
            let generated = GeneratedCrop::new(prepared.crop.w, prepared.crop.h, &answer);
            let (before, after) = prepared.preview_rgb8(&generated).unwrap();
            assert_eq!(before, prepared.image_rgb8);
            let rendered = prepared.composite(&generated).unwrap();
            for y in 0..prepared.crop.h {
                for x in 0..prepared.crop.w {
                    let px = prepared.crop.x + i64::from(x);
                    let py = prepared.crop.y + i64::from(y);
                    let at = (y as usize * prepared.crop.w as usize + x as usize) * 3;
                    if !rendered.mask.contains(px, py) {
                        assert_eq!(&before[at..at + 3], &after[at..at + 3], "{} outside the mask", scene.name);
                    } else {
                        for channel in 0..3 {
                            let c = if rendered.pixels.mode == ColorMode::Gray { 0 } else { channel };
                            let expected = rendered.pixels.sample((px - rendered.mask.bounds.x) as u32, (py - rendered.mask.bounds.y) as u32, c) as u8;
                            assert_eq!(after[at + channel], expected, "{} actual blend", scene.name);
                        }
                    }
                }
            }
        }
    }

    /// **The pale glyph-shaped patch, and its fix.** On every background a
    /// fringe was reviewed on, an answer drifted the way the demo chapter's
    /// were composites under V1 as a patch 14 levels off the page around it,
    /// and under V2 lands within two levels of the page as it was under the
    /// lettering - core and feather both, so there is no contour either.
    #[test]
    fn a_drifted_answer_composites_onto_the_page_tone_on_every_background() {
        for scene in scenes() {
            let fitted = manual_fit(&scene);
            let drift = match scene.name {
                "clipped sky beside art" => [8, 5, 17],
                _ if scene.page.mode == ColorMode::Rgb => [18, 14, 10],
                _ => [14; 3],
            };
            let v1 = PreparedRender::prepare_as(&scene.page, &fitted, Preprocessing::V1).unwrap();
            let answer = drifted(&v1, &scene.clean, drift);
            let straight = v1.composite(&GeneratedCrop::new(v1.crop().w, v1.crop().h, &answer)).unwrap();
            assert!(straight.tone.is_none());
            assert!(residual(&scene, &straight) >= 10, "{}: the fixture has no drift to fix", scene.name);

            let v2 = PreparedRender::prepare(&scene.page, &fitted).unwrap();
            assert_eq!(v2.preprocessing(), Preprocessing::V2);
            let answer = drifted(&v2, &scene.clean, drift);
            let aligned = v2.composite(&GeneratedCrop::new(v2.crop().w, v2.crop().h, &answer)).unwrap();
            let tone = aligned.tone.clone().expect("V2 reports its tone");
            assert!(tone.samples >= MIN_TONE_SAMPLES, "{}: {} samples", scene.name, tone.samples);
            for (curve, drift) in tone.curves.iter().zip(drift) {
                assert!(!curve.is_empty(), "{}: no curve", scene.name);
                for &[level, correction] in curve {
                    let drift = -f64::from(drift);
                    if level >= 254.5 {
                        // A clipped band returned what it was shown.
                        assert!(correction.abs() <= 1.0, "{}: {correction} at {level}", scene.name);
                    } else if scene.name == "clipped sky beside art" {
                        // Boxes across the sky's edge hold both, and read between.
                        assert!((drift - 1.0..=1.0).contains(&correction), "{}: {correction} at {level}", scene.name);
                    } else {
                        assert!((correction - drift).abs() <= 1.0, "{}: {correction} at {level}, drift {drift}", scene.name);
                    }
                }
            }
            assert!(residual(&scene, &aligned) <= 2, "{}: residual {}", scene.name, residual(&scene, &aligned));
        }
    }

    /// All of the lettering, anti-aliasing ring included, is inside the core
    /// the answer is written at full weight, and the feather lies wholly on
    /// clean page.
    #[test]
    fn the_lettering_is_inside_the_fully_replaced_core_and_the_feather_is_clean_page() {
        for scene in scenes() {
            let prepared = PreparedRender::prepare(&scene.page, &manual_fit(&scene)).unwrap();
            let applied = prepared.applied();
            let mut feathered = 0;
            for y in applied.bounds.y..applied.bounds.bottom() {
                for x in applied.bounds.x..applied.bounds.right() {
                    let alpha = prepared.alpha(x, y);
                    if scene.lettering.contains(x, y) {
                        assert_eq!(alpha, 1.0, "{} ({x}, {y}): lettering outside the core", scene.name);
                    } else if alpha > 0.0 && alpha < 1.0 {
                        feathered += 1;
                        assert!(
                            !scene.lettering.dilated(1, SIDE, SIDE).contains(x, y),
                            "{} ({x}, {y}): the feather touches the lettering",
                            scene.name
                        );
                    }
                }
            }
            // The feather is kept, not removed: a hard edge is its own contour.
            assert!(feathered > 0, "{}: no feather", scene.name);
        }
    }

    /// Outside the declared support nothing moves, and alpha is copied, under
    /// V2's alignment as under V1.
    #[test]
    fn alignment_writes_nothing_outside_the_applied_mask_and_never_alpha() {
        for mode in [ColorMode::Gray, ColorMode::GrayAlpha, ColorMode::Rgb, ColorMode::Rgba] {
            let page = make_page(SIDE, SIDE, mode, BitDepth::Eight, |x, y, c| {
                if mode.alpha_channel() == Some(c) {
                    ((x * 3 + y * 5) % 200 + 55) as u16
                } else if x % 6 < 3 && y % 6 < 3 {
                    90
                } else {
                    220
                }
            });
            let fitted = fitted_over(&page, Rect::new(150, 150, 40, 30));
            let prepared = PreparedRender::prepare(&page, &fitted).unwrap();
            let answer = vec![255u8; (prepared.crop().w * prepared.crop().h * 3) as usize];
            let rendered = prepared
                .composite(&GeneratedCrop::new(prepared.crop().w, prepared.crop().h, &answer))
                .unwrap();
            assert_eq!(rendered.mask, *prepared.applied());
            let bounds = rendered.mask.bounds;
            for y in 0..rendered.pixels.height {
                for x in 0..rendered.pixels.width {
                    let (px, py) = (bounds.x + x as i64, bounds.y + y as i64);
                    for c in 0..mode.samples() {
                        let (got, was) = (rendered.pixels.sample(x, y, c), page.sample(px as u32, py as u32, c));
                        if !rendered.mask.contains(px, py) || mode.alpha_channel() == Some(c) {
                            assert_eq!(got, was, "{mode:?} ({px}, {py}) channel {c}");
                        }
                    }
                }
            }
        }
    }

    /// A model that washed the texture out is reported, not hidden: the mean
    /// is aligned, so no pale step, and the ring's texture says how much of
    /// the tone came back.
    #[test]
    fn a_washed_out_answer_is_aligned_and_reported() {
        let scene = &scenes()[2];
        assert_eq!(scene.name, "screentone");
        let prepared = PreparedRender::prepare(&scene.page, &manual_fit(scene)).unwrap();
        // The tone's mean, (9 * 90 + 27 * 220) / 36 = 187.5, drifted up by
        // 13.5 and flattened. The median box a 7 px box sees of a 6 px tone
        // holds 12 dark pixels of 49, a mean of 188.2.
        let answer = vec![201u8; (prepared.crop().w * prepared.crop().h * 3) as usize];
        let rendered = prepared
            .composite(&GeneratedCrop::new(prepared.crop().w, prepared.crop().h, &answer))
            .unwrap();
        let tone = rendered.tone.unwrap();
        assert!((tone.correction(0, 201.0) - (188.2 - 201.0)).abs() <= 0.5, "{:?}", tone.curves);
        assert!(tone.source_texture.unwrap() >= 1.0, "{:?}", tone.source_texture);
        assert!(tone.texture.unwrap() < 0.05, "{:?}", tone.texture);

        let straight = prepared.blend(&GeneratedCrop::new(prepared.crop().w, prepared.crop().h, &answer), None).unwrap();
        assert!(straight.tone.is_none());
    }

    /// A ring the model rewrote is not drift: the offset stops at
    /// [`MAX_TONE_OFFSET`], and too few ring samples add nothing.
    #[test]
    fn the_offset_is_bounded_and_needs_enough_ring() {
        let page = make_page(SIDE, SIDE, ColorMode::Gray, BitDepth::Eight, |_, _, _| 200);
        let fitted = fitted_over(&page, Rect::new(150, 150, 40, 30));
        let prepared = PreparedRender::prepare(&page, &fitted).unwrap();
        let answer = vec![20u8; (prepared.crop().w * prepared.crop().h * 3) as usize];
        let tone = prepared.tone(&GeneratedCrop::new(prepared.crop().w, prepared.crop().h, &answer)).unwrap();
        assert_eq!(tone.curves.len(), 1);
        assert_eq!(tone.curves[0].len(), 1);
        assert!((tone.curves[0][0][0] - 20.0).abs() < 1e-9);
        assert_eq!(tone.curves[0][0][1], MAX_TONE_OFFSET);
        assert!(tone.source_texture.is_some());
        assert_eq!(tone.texture, None, "flat paper has no texture to keep");

        // A region filling a small page leaves almost no ring inside it.
        let small = make_page(24, 24, ColorMode::Gray, BitDepth::Eight, |_, _, _| 200);
        let fitted = fitted_over(&small, Rect::new(4, 4, 16, 16));
        let prepared = PreparedRender::prepare(&small, &fitted).unwrap();
        let answer = vec![20u8; (prepared.crop().w * prepared.crop().h * 3) as usize];
        let tone = prepared.tone(&GeneratedCrop::new(prepared.crop().w, prepared.crop().h, &answer)).unwrap();
        assert!(tone.samples < MIN_TONE_SAMPLES);
        assert_eq!(tone.curves, vec![Vec::<[f64; 2]>::new()]);
        assert_eq!(tone.correction(0, 20.0), 0.0);
    }

    /// Between the curve's points the correction is interpolated; past them it
    /// tapers to nothing at 0 and 255, and a point at a clipped level holds
    /// to the end.
    #[test]
    fn the_correction_interpolates_and_tapers_to_the_ends() {
        let tone = ToneReport { curves: vec![vec![[100.0, -10.0], [200.0, -20.0]]], samples: 99, ring: 99, background: 99, abstained: None, preprocessing: Preprocessing::V2, source_texture: None, texture: None };
        for (level, wanted) in [(0.0, 0.0), (50.0, -5.0), (100.0, -10.0), (150.0, -15.0), (227.5, -10.0), (255.0, 0.0)] {
            assert!((tone.correction(0, level) - wanted).abs() < 1e-9, "{level}");
        }
        let clipped = ToneReport { curves: vec![vec![[0.0, 4.0], [255.0, -6.0]]], samples: 99, ring: 99, background: 99, abstained: None, preprocessing: Preprocessing::V2, source_texture: None, texture: None };
        assert_eq!(clipped.correction(0, 0.0), 4.0);
        assert_eq!(clipped.correction(0, 255.0), -6.0);
    }

    /// V1 is still exactly V1: the crop it cut and the patch it blended do not
    /// move, so a result that was in flight when the version changed attaches
    /// as it was prepared.
    #[test]
    fn a_v1_preparation_is_the_legacy_crop_and_blend() {
        let page = make_page(400, 400, ColorMode::Rgb, BitDepth::Eight, |x, y, c| {
            ((x * 11 + y * 17 + c as u32 * 43) % 256) as u16
        });
        let fitted = fitted_over(&page, Rect::new(120, 120, 40, 40));
        let v1 = PreparedRender::prepare_as(&page, &fitted, Preprocessing::V1).unwrap();
        let applied = applied_mask(&fitted, 400, 400).bounds;
        let context = (applied.w.max(applied.h) / 2).clamp(CONTEXT_MIN, CONTEXT_MAX);
        assert_eq!(v1.crop().w, snap(applied.w + 2 * context));
        let answer: Vec<u8> = (0..v1.crop().w * v1.crop().h * 3).map(|i| (i * 7 % 256) as u8).collect();
        let generated = GeneratedCrop::new(v1.crop().w, v1.crop().h, &answer);
        let rendered = v1.composite(&generated).unwrap();
        assert_eq!(rendered.pixels.data, v1.blend(&generated, None).unwrap().pixels.data);
        assert!(rendered.tone.is_none());
        assert_eq!(PreparedRender::prepare(&page, &fitted).unwrap().crop().w, snap(applied.w + 2 * CONTEXT_WIDE));
    }

    /// A page whose lettering sits on `near` out to `reach` px from the
    /// glyph, and on `far` beyond: the ring is `far` wherever it is more than
    /// a box's half-width past `reach`.
    fn lettering_on_a_halo(
        name: &'static str,
        mode: ColorMode,
        near: [u16; 4],
        far: [u16; 4],
        reach: u32,
    ) -> Scene {
        let text = lettering_at(140, 160);
        let halo = text.dilated(reach, SIDE, SIDE);
        let ground = move |x: u32, y: u32, c: usize| {
            if halo.contains(x as i64, y as i64) { near[c] } else { far[c] }
        };
        scene(name, mode, ground, [20; 3], &text)
    }

    /// The largest change the aligned composite makes to the raw return
    /// blended as it came back, over the applied mask, in levels.
    fn departure_from_raw(prepared: &PreparedRender, answer: &[u8]) -> u16 {
        let generated = GeneratedCrop::new(prepared.crop().w, prepared.crop().h, answer);
        let aligned = prepared.composite(&generated).unwrap();
        let raw = prepared.blend(&generated, None).unwrap();
        aligned.pixels.data.iter().zip(&raw.pixels.data).map(|(a, b)| a.abs_diff(*b) as u16).max().unwrap_or(0)
    }

    /// **The artwork ring.** The lettering sits on grey, the ring around the
    /// edit is purple artwork, and the model returned the grey it should
    /// have everywhere. The ring is not the lettering's background, so the
    /// alignment has no evidence of drift and must not paint the ring's
    /// purple (48 levels of channel separation) over a correct grey core.
    #[test]
    fn an_artwork_ring_does_not_tint_a_correct_core() {
        let scene = lettering_on_a_halo(
            "grey halo in purple art", ColorMode::Rgb, [128, 128, 128, 255], [152, 104, 152, 255], 8,
        );
        let prepared = PreparedRender::prepare(&scene.page, &manual_fit(&scene)).unwrap();
        let answer = vec![128u8; (prepared.crop().w * prepared.crop().h * 3) as usize];
        let tone = prepared.tone(&GeneratedCrop::new(prepared.crop().w, prepared.crop().h, &answer)).unwrap();
        for channel in 0..3 {
            assert!(tone.correction(channel, 128.0).abs() <= 1.0, "{:?}", tone);
        }
        assert!(departure_from_raw(&prepared, &answer) <= 1, "the core was tinted: {tone:?}");
    }

    /// **An artwork ring within the colour tolerance.** The ring is purple
    /// art only 11 levels off the neutral grey the lettering sits on, near
    /// enough to pass for that grey channel by channel, but parted 22 levels
    /// where the grey is parted by none. The page around the lettering shows
    /// no such separation, so the correction may not add it: the correct grey
    /// core is not tinted [+11, -11, +11].
    #[test]
    fn an_artwork_ring_near_the_background_does_not_part_a_neutral_core() {
        let scene = lettering_on_a_halo(
            "grey halo in near-grey purple art", ColorMode::Rgb, [180, 180, 180, 255], [191, 169, 191, 255], 8,
        );
        let prepared = PreparedRender::prepare(&scene.page, &manual_fit(&scene)).unwrap();
        let answer = vec![180u8; (prepared.crop().w * prepared.crop().h * 3) as usize];
        let tone = prepared.tone(&GeneratedCrop::new(prepared.crop().w, prepared.crop().h, &answer)).unwrap();
        assert_eq!(tone.abstained, Some(Abstention::Separation), "{tone:?}");
        assert!(departure_from_raw(&prepared, &answer) <= 1, "the core was tinted: {tone:?}");
    }

    /// The same, with the last stroke's end on the purple too: the ring's
    /// colour is now among the lettering's surroundings, but only at one
    /// corner of them, which does not make the grey core purple.
    #[test]
    fn a_colour_only_a_corner_of_the_lettering_sits_on_does_not_tint_it() {
        let text = lettering_at(140, 160);
        let halo = text.dilated(8, SIDE, SIDE);
        let ground = move |x: u32, y: u32, c: usize| {
            if halo.contains(x as i64, y as i64) && x < 202 { 128 } else { [152, 104, 152][c] }
        };
        let scene = scene("grey halo cut by purple art", ColorMode::Rgb, ground, [20; 3], &text);
        let prepared = PreparedRender::prepare(&scene.page, &manual_fit(&scene)).unwrap();
        let answer = vec![128u8; (prepared.crop().w * prepared.crop().h * 3) as usize];
        let tone = prepared.tone(&GeneratedCrop::new(prepared.crop().w, prepared.crop().h, &answer)).unwrap();
        assert_eq!(tone.abstained, Some(Abstention::Separation), "{tone:?}");
        assert_eq!(departure_from_raw(&prepared, &answer), 0);
    }

    /// Transparent page is not background: its colour samples are whatever
    /// the file stored under alpha 0, not anything the model was shown.
    #[test]
    fn transparent_ring_samples_are_not_evidence() {
        for mode in [ColorMode::Rgba, ColorMode::GrayAlpha] {
            let (near, far) = match mode {
                ColorMode::Rgba => ([128, 128, 128, 255], [0, 0, 0, 0]),
                _ => ([128, 255, 0, 0], [0, 0, 0, 0]),
            };
            let scene = lettering_on_a_halo("grey halo in transparency", mode, near, far, 8);
            let prepared = PreparedRender::prepare(&scene.page, &manual_fit(&scene)).unwrap();
            let answer = vec![140u8; (prepared.crop().w * prepared.crop().h * 3) as usize];
            let tone = prepared.tone(&GeneratedCrop::new(prepared.crop().w, prepared.crop().h, &answer)).unwrap();
            assert!(tone.curves.iter().all(Vec::is_empty), "{mode:?}: {tone:?}");
            assert_eq!(departure_from_raw(&prepared, &answer), 0, "{mode:?}");
        }
    }

    /// A Gray or RGB PNG's `tRNS` colour key is transparency too: every pixel
    /// of exactly that colour is see-through, alpha channel or not. Here the
    /// keyed colour is 2 levels off the lettering's grey, near enough to pass
    /// as its background, and the model returned 10 levels lighter: counted
    /// as page, the see-through ring would pull the whole answer 10 darker.
    #[test]
    fn colour_keyed_ring_samples_are_not_evidence() {
        for (mode, key) in [(ColorMode::Rgb, vec![0, 130, 0, 130, 0, 130]), (ColorMode::Gray, vec![0, 130])] {
            let mut scene = lettering_on_a_halo("grey halo in keyed transparency", mode, [128; 4], [130; 4], 8);
            scene.page.trns = Some(key);
            // Through a PNG and back: the decoder keeps the key as the file
            // stores it, and that is the page a render is prepared from.
            let png = crate::image::encode(&scene.page, crate::image::Format::Png).unwrap();
            scene.page = crate::image::decode(&png).unwrap();
            assert_eq!(scene.page.colour_key(), Some(vec![130; mode.samples()]));
            let prepared = PreparedRender::prepare(&scene.page, &manual_fit(&scene)).unwrap();
            let answer = vec![140u8; (prepared.crop().w * prepared.crop().h * 3) as usize];
            let tone = prepared.tone(&GeneratedCrop::new(prepared.crop().w, prepared.crop().h, &answer)).unwrap();
            assert!(tone.curves.iter().all(Vec::is_empty), "{mode:?}: {tone:?}");
            assert_eq!(departure_from_raw(&prepared, &answer), 0, "{mode:?}");
        }
    }

    /// Where the lettering's own surroundings are the ring's colour, the
    /// correction toward it is evidence, not a tint: a model that washed a
    /// coloured page out to grey is brought back to the page.
    #[test]
    fn a_ring_the_lettering_sits_on_still_corrects_its_colour() {
        let scene = lettering_on_a_halo(
            "purple page", ColorMode::Rgb, [140, 110, 140, 255], [140, 110, 140, 255], 8,
        );
        let prepared = PreparedRender::prepare(&scene.page, &manual_fit(&scene)).unwrap();
        // Drifted towards grey by 8 levels a channel, which a straight blend
        // leaves as a step at the edit.
        let answer = drifted(&prepared, &scene.clean, [-8, 8, -8]);
        let generated = GeneratedCrop::new(prepared.crop().w, prepared.crop().h, &answer);
        let straight = prepared.blend(&generated, None).unwrap();
        assert!(residual(&scene, &straight) >= 7);
        let aligned = prepared.composite(&generated).unwrap();
        assert!(residual(&scene, &aligned) <= 2, "{:?}", aligned.tone);
    }

    /// **The crop fits the render service.** Under 2.0.0 an interior 1800 px
    /// hole asked for a 2080 px crop, past the deployed 2048 px limit, where
    /// 1.0.0 cut 1984; the context now shrinks to what fits, to the pixel at
    /// the boundary, and a region too large to fit with none gets none.
    #[test]
    fn a_v2_crop_shrinks_its_context_to_fit_the_render_service() {
        let limits = crate::cloud_wire::provisional_fixture_limits();
        let fits = |crop: Rect| {
            crop.w <= limits.max_width
                && crop.h <= limits.max_height
                && u64::from(crop.w) * u64::from(crop.h) <= limits.max_pixels
        };
        let v2 = Preprocessing::V2;
        let applied = Rect::new(300, 300, 1800, 1800).grown(crate::constants::ISOLATION_RADIUS, u32::MAX, u32::MAX);
        let legacy = Preprocessing::V1.crop_for(applied).unwrap();
        assert_eq!((legacy.w, legacy.h), (1984, 1984), "1.0.0 is unchanged");
        let crop = v2.crop_for(applied).unwrap();
        assert!(fits(crop), "{crop:?}");
        assert_eq!((crop.w, crop.h), (2048, 2048));
        assert_eq!(v2.context_for(applied), 119);
        assert_eq!(crop.x + crop.w as i64 / 2, applied.x + applied.w as i64 / 2, "still centred");

        // The widest region that takes the whole context, and the next one.
        let edge = MAX_CROP_SIDE - 2 * CONTEXT_WIDE;
        assert_eq!(v2.context_for(Rect::new(0, 0, edge, 40)), CONTEXT_WIDE);
        assert_eq!(v2.context_for(Rect::new(0, 0, edge + 1, 40)), CONTEXT_WIDE - 1);
        assert_eq!(v2.context_for(Rect::new(0, 0, 40, edge + 2)), CONTEXT_WIDE - 1);
        for extent in edge - 2..=MAX_CROP_SIDE {
            for bounds in [Rect::new(0, 0, extent, 40), Rect::new(0, 0, extent, extent)] {
                let crop = v2.crop_for(bounds).unwrap();
                assert!(fits(crop), "{bounds:?} -> {crop:?}");
                assert_eq!(within_service_limits(crop), fits(crop));
                assert_eq!((crop.w % LATENT_STRIDE, crop.h % LATENT_STRIDE), (0, 0));
                assert!(crop.w >= bounds.w && crop.h >= bounds.h);
            }
        }
        assert_eq!(v2.context_for(Rect::new(0, 0, 900, 1000)), CONTEXT_WIDE, "small regions keep it all");

        let over = Rect::new(0, 0, MAX_CROP_SIDE + 1, 40);
        assert_eq!(v2.context_for(over), 0);
        let crop = v2.crop_for(over).unwrap();
        assert!(!fits(crop) && !within_service_limits(crop), "a plan holds it out: {crop:?}");
    }

    /// **No extent wraps.** A side near `u32::MAX` used to saturate into the
    /// stride snap and overflow it; the side is now worked wide and a crop no
    /// `u32` can hold is refused, where one that just fits is still cut.
    #[test]
    fn a_crop_past_what_a_u32_holds_is_refused_not_wrapped() {
        let top = u32::MAX - (LATENT_STRIDE - 1);
        for version in [Preprocessing::V1, Preprocessing::V2] {
            assert_eq!(version.crop_for(Rect::new(0, 0, u32::MAX, 40)), None, "{version:?}");
            assert_eq!(version.crop_for(Rect::new(0, 0, 40, u32::MAX)), None, "{version:?}");
            assert_eq!(version.crop_of_hole(Rect::new(0, 0, u32::MAX, 40), u32::MAX, u32::MAX), None);
        }
        assert_eq!(Preprocessing::V2.context_for(Rect::new(0, 0, u32::MAX, u32::MAX)), 0);
        let widest = Preprocessing::V2.crop_for(Rect::new(0, 0, top, 40)).unwrap();
        assert_eq!(widest.w, top, "the largest multiple of the stride is still a crop");
        assert!(!within_service_limits(widest));
        assert_eq!(Preprocessing::V2.crop_for(Rect::new(0, 0, top + 1, 40)), None);
    }

    /// **The page edge clips the estimate as it clips the render.** A hole at
    /// the right or bottom edge grows by the isolation radius only as far as
    /// the page, so its crop is the one the preparation cuts, not the wider
    /// one an unclipped estimate named (304 px for a 26 px hole, sent at 288).
    #[test]
    fn a_hole_at_the_page_edge_is_cut_where_the_preparation_cuts_it() {
        let page = make_page(400, 300, ColorMode::Gray, BitDepth::Eight, |_, _, _| 180);
        for hole in [Rect::new(374, 100, 26, 20), Rect::new(100, 274, 20, 26), Rect::new(0, 0, 26, 26)] {
            let fitted = fitted_over(&page, hole);
            for version in [Preprocessing::V1, Preprocessing::V2] {
                let prepared = PreparedRender::prepare_as(&page, &fitted, version).unwrap();
                assert_eq!(version.crop_of_hole(hole, 400, 300), Some(prepared.crop()), "{version:?} {hole:?}");
            }
        }
        let right = Preprocessing::V2.crop_of_hole(Rect::new(374, 100, 26, 20), 400, 300).unwrap();
        assert_eq!(right.w, 288);
        let unclipped = Preprocessing::V2.crop_for(Rect::new(369, 95, 36, 30)).unwrap();
        assert_eq!(unclipped.w, 304, "what the estimate used to name");
    }

    #[test]
    fn preprocessing_versions_parse_and_unknown_ones_are_refused() {
        assert_eq!(Preprocessing::of("1.0.0"), Ok(Preprocessing::V1));
        assert_eq!(Preprocessing::of(PREPROCESSING_VERSION), Ok(Preprocessing::CURRENT));
        assert_eq!(Preprocessing::CURRENT.version(), PREPROCESSING_VERSION);
        for unknown in ["", "3.0.0", "cleaner-crop-v1", "2.0"] {
            assert_eq!(Preprocessing::of(unknown), Err(UnsupportedPreprocessing(unknown.to_owned())));
        }
        let (mask, ink) = (Mask::filled(Rect::new(0, 0, 9, 9)), Mask::filled(Rect::new(2, 2, 3, 3)));
        assert_eq!(Preprocessing::V1.hole(&mask, &ink), &mask);
        assert_eq!(Preprocessing::V2.hole(&mask, &ink), &ink);
        let unrecorded = Mask::empty(Rect::new(0, 0, 0, 0));
        assert_eq!(Preprocessing::V2.hole(&mask, &unrecorded), &mask);
    }
}
