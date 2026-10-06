//! Page denoise on this computer: the cloud runtime's waifu2x tiler, its
//! linear-light Lanczos downscale and its page rules, ported from
//! `deploy/cloud/common/denoise.py` so a page denoised here and one denoised on
//! the cloud GPU agree. The parity test at the bottom holds a real page within
//! 2/255 of the cloud code's own output.
//!
//! One model runs locally for now: waifu2x swin_unet `art_scan`
//! `noise2_scale4x`, the preset `waifu2x-scan-4x-n2`. It is ONNX, so it runs
//! through `ort` like every other model here ([`accel::open_session`]). The
//! other presets are PyTorch files only the cloud worker loads.
//!
//! ## The same arithmetic, streamed
//!
//! The cloud holds the 4x page twice as float32 (pixels and blend weights),
//! about 900 MB for a 1284x1809 page. Here the tiles run one row of tiles at a
//! time. Once a tile row is blended, every canvas row above the next tile
//! row's overlap is final, so it goes straight into the downscale: its
//! horizontal pass shrinks the row as it arrives, and its vertical pass emits
//! an output row as soon as the input rows under its kernel are in. The order
//! and precision of every operation are the cloud's (float32 where numpy is
//! float32, float64 where Pillow's resampler is), so streaming changes the
//! memory and nothing else.
//!
//! ## Page rules
//!
//! The cloud's `decode_page` and `encode_page`: 1-bit, indexed and CMYK pages
//! are declined ([`Decline`]); grey is expanded to three channels for the model
//! and folded back by the channel mean, and an RGB page whose channels never
//! differ by 1.5/255 or more counts as grey; alpha is copied unchanged, and an
//! alpha channel that is fully opaque is dropped, as the cloud drops it; a
//! 16-bit page is written back at 16 bits.

use std::collections::VecDeque;
use std::path::Path;

use crate::accel::{self, Accelerator, ModelProfile, Preference, Selection, SessionError};
use crate::cloud_denoise_wire::{DenoiseRecipe, MAX_PAGE_PIXELS, MAX_UPSCALED_PIXELS};
use crate::engines::model::Decline;
use crate::image::{BitDepth, ColorMode, Raster};

/// The one preset this computer runs.
pub const LOCAL_PRESET: &str = "waifu2x-scan-4x-n2";
/// The files, named as `denoise.py` names them on the weights volume.
pub const MODEL_FILE: &str = "waifu2x--swin_unet--art_scan--noise2_scale4x.onnx";
pub const SEAM_FILTER_FILE: &str = "waifu2x--utils--create_seam_blending_filter.onnx";
/// The model id a local step must resolve to.
const LOCAL_MODEL_ID: &str = "waifu2x.swin_unet.art_scan.noise2_scale4x";

/// `BLEND_SIZE`: output pixels over which neighbouring tiles fade into each other.
pub const BLEND_SIZE: usize = 16;

/// Where the page denoise model runs. Measured on the M5 (2026-09-28), one
/// 1284x1809 grey page after a warm-up tile: WebGPU 31 to 33 s, CPU 48 s on
/// the 1.28.0 release runtime and 76 s on the downloaded one. WebGPU cannot
/// hold the whole swin graph (strict placement fails) and still wins with the
/// rest on the CPU. CoreML builds only partitioned and then fails its first
/// run, so a forced CoreML session is refused at build instead.
pub const PROFILE: ModelProfile = ModelProfile {
    name: "pageDenoise",
    label_key: "models.kind.pageDenoise",
    uses_dft: false,
    measured_better: &[Accelerator::WebGpu],
    unmeasured_candidates: &[],
    measured_peak_rss: &[],
    partitioned_on: &[Accelerator::WebGpu],
};

/// The seam helper graph: three integers in, one weight map out. Always the CPU.
pub const SEAM_PROFILE: ModelProfile = ModelProfile {
    name: "pageDenoiseSeams",
    label_key: "models.kind.pageDenoiseSeams",
    uses_dft: false,
    measured_better: &[],
    unmeasured_candidates: &[],
    measured_peak_rss: &[],
    partitioned_on: &[],
};

/// A waifu2x model's tiling contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Waifu2xModel {
    pub scale: usize,
    /// Output pixels the model consumes on each side of a tile.
    pub offset: usize,
    /// Input tile side.
    pub tile: usize,
}

impl Waifu2xModel {
    /// A swin_unet model at `scale`, with the tile `run_step` picks for it.
    pub fn swin(scale: usize) -> Waifu2xModel {
        let offset = match scale {
            1 => 8,
            2 => 16,
            _ => 32,
        };
        Waifu2xModel { scale, offset, tile: swin_tile(if scale == 4 { 256 } else { 400 }) }
    }

    /// The side of one tile's output, and of the seam weight map.
    pub fn output_side(&self) -> usize {
        self.tile * self.scale - 2 * self.offset
    }
}

/// `_swin_tile`: the next size swin_unet's windows divide.
pub fn swin_tile(mut tile: usize) -> usize {
    while tile < 16 || (tile - 16) % 12 != 0 || (tile - 16) % 16 != 0 {
        tile += 1;
    }
    tile
}

/// The step a local run takes for a recipe, or why this computer cannot run it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LocalStep {
    pub model: Waifu2xModel,
    pub file: &'static str,
    pub strength: f64,
}

pub fn local_step(recipe: &DenoiseRecipe) -> Result<LocalStep, String> {
    let steps = recipe.resolve()?;
    match (steps.as_slice(), recipe.steps.first()) {
        ([step], Some(raw)) if step.model_id == LOCAL_MODEL_ID => Ok(LocalStep {
            model: Waifu2xModel::swin(4),
            file: MODEL_FILE,
            strength: raw.strength.unwrap_or(1.0),
        }),
        _ => Err("this recipe does not run on this computer".into()),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Declined(#[from] Decline),
    #[error("{0}")]
    TooLarge(&'static str),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error("the model failed: {0}")]
    Run(String),
    #[error("the run was stopped")]
    Cancelled,
}

impl Error {
    /// The stable code a page fails with.
    pub fn code(&self) -> &'static str {
        match self {
            Error::Declined(_) => "denoise_unsupported_page",
            Error::TooLarge(_) => "denoise_page_too_large",
            Error::Session(_) => "denoise_session_failed",
            Error::Run(_) => "denoise_inference_failed",
            Error::Cancelled => "denoise_cancelled",
        }
    }
}

/* ------------------------------------------------------------------ */
/* Page in, page out                                                   */
/* ------------------------------------------------------------------ */

/// A page as the model reads it: three float planes in [0, 1], grey repeated.
pub struct Page {
    pub width: usize,
    pub height: usize,
    rgb: Vec<f32>,
    pub gray: bool,
    source: Raster,

}

/// `decode_page`, from a decoded raster.
pub fn decode_page(raster: &Raster) -> Result<Page, Error> {
    if !raster.mode.allows_model_engines() {
        return Err(Decline::Mode(raster.mode).into());
    }
    if raster.depth == BitDepth::One {
        return Err(Decline::Depth(raster.depth).into());
    }
    let pixels = u64::from(raster.width) * u64::from(raster.height);
    if pixels == 0 || pixels > MAX_PAGE_PIXELS {
        return Err(Error::TooLarge("page is too large"));
    }
    let (width, height) = (raster.width as usize, raster.height as usize);
    let plane = width * height;
    crate::image::color::validate_editing(raster).map_err(|e| Error::Run(e.to_string()))?;
    let managed = crate::image::color::ManagedColor::for_raster(raster).map_err(|e| Error::Run(e.to_string()))?;
    let colour = matches!(raster.mode, ColorMode::Rgb | ColorMode::Rgba);
    let mut rgb = vec![0f32; 3 * plane];
    for y in 0..raster.height {
        for x in 0..raster.width {
            let at = y as usize * width + x as usize;
            let shown = managed.pixel(raster, x, y);
            for c in 0..3 { rgb[c * plane + at] = shown[c]; }
        }
    }
    let tolerance = 1.5f32 / 255.0;
    let gray = !colour || (0..plane).all(|at| {
        let (r, g, b) = (rgb[at], rgb[plane + at], rgb[2 * plane + at]);
        (r - g).abs() < tolerance && (g - b).abs() < tolerance
    });
    Ok(Page {
        width,
        height,
        rgb,
        gray,
        source: raster.clone(),
    })
}

/// What a page came back as.
#[derive(Debug, Clone)]
pub struct Denoised {
    pub raster: Raster,
    pub gray: bool,
}

/// `encode_page`: `out` is one plane for a grey page, three otherwise, in [0, 1].
fn encode_page(page: &Page, out: &[f32], strength: f64) -> Result<Raster, Error> {
    let plane = page.width * page.height;
    let managed = crate::engines::model::editable_color(&page.source).map_err(|e| Error::Run(e.to_string()))?;
    let mut raster = page.source.clone();
    for y in 0..page.height {
        for x in 0..page.width {
            let at = y * page.width + x;
            let rgb = std::array::from_fn(|c| if page.gray { out[at] } else { out[c * plane + at] });
            crate::engines::model::commit_srgb(&managed, &mut raster, x as u32, y as u32, rgb, strength, 0.0)
                .map_err(|e| Error::Run(e.to_string()))?;
        }
    }
    raster.color = raster.color.after_edit();
    Ok(raster)
}

/* ------------------------------------------------------------------ */
/* The tiler                                                           */
/* ------------------------------------------------------------------ */

/// One tile through the model: `[1, 3, side, side]` in, `[1, 3, f, f]` out,
/// `f = side * scale - 2 * offset`.
pub trait TileModel {
    fn run(&mut self, tile: Vec<f32>, side: usize) -> Result<Vec<f32>, Error>;
}

/// `Waifu2x.run`'s geometry for one page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tiling {
    pub model: Waifu2xModel,
    pub in_offset: usize,
    pub in_step: usize,
    pub out_step: usize,
    pub h_blocks: usize,
    pub w_blocks: usize,
}

impl Tiling {
    pub fn new(model: Waifu2xModel, height: usize, width: usize) -> Tiling {
        let Waifu2xModel { scale, offset, tile } = model;
        let in_offset = offset.div_ceil(scale);
        let in_blend = BLEND_SIZE.div_ceil(scale);
        let in_step = tile - (in_offset * 2 + in_blend);
        let blocks = |size: usize| {
            let (mut blocks, mut covered) = (0, 0);
            while covered < size + in_offset * 2 {
                covered = blocks * in_step + tile;
                blocks += 1;
            }
            blocks
        };
        Tiling { model, in_offset, in_step, out_step: in_step * scale, h_blocks: blocks(height), w_blocks: blocks(width) }
    }

    pub fn tiles(&self) -> usize {
        self.h_blocks * self.w_blocks
    }
}

/// Run every tile of `page` in `Waifu2x.run`'s order and blend it as it does,
/// handing each finished canvas row (three planes of `width * scale`, clipped
/// to [0, 1], cropped to the page) to `emit`, top to bottom.
///
/// Only one tile row's band is held: canvas rows are final once the next tile
/// row can no longer reach them, and the band then slides down by `out_step`.
pub fn upscale(
    page: &Page,
    model: &mut dyn TileModel,
    tiling: &Tiling,
    filter: &[f32],
    mut emit: impl FnMut(&[f32]),
) -> Result<(), Error> {
    let Waifu2xModel { scale, tile, .. } = tiling.model;
    let side = tiling.model.output_side();
    if filter.len() != side * side || tiling.out_step > side {
        return Err(Error::Run("the seam weights do not fit the tile".into()));
    }
    let (height, width) = (page.height * scale, page.width * scale);
    let plane = page.width * page.height;
    let mut pixels = vec![0f32; 3 * side * width];
    let mut weights = vec![0f32; side * width];
    let mut row = vec![0f32; 3 * width];
    let clamp = |value: isize, size: usize| value.clamp(0, size as isize - 1) as usize;
    for h_i in 0..tiling.h_blocks {
        for w_i in 0..tiling.w_blocks {
            let (i, j) = (h_i * tiling.in_step, w_i * tiling.in_step);
            // `np.pad(mode="edge")` read in place: a padded coordinate is the
            // nearest page pixel.
            let mut input = vec![0f32; 3 * tile * tile];
            for c in 0..3 {
                for ty in 0..tile {
                    let sy = clamp((i + ty) as isize - tiling.in_offset as isize, page.height);
                    let src = &page.rgb[c * plane + sy * page.width..][..page.width];
                    let dst = &mut input[(c * tile + ty) * tile..][..tile];
                    for (tx, value) in dst.iter_mut().enumerate() {
                        *value = src[clamp((j + tx) as isize - tiling.in_offset as isize, page.width)];
                    }
                }
            }
            let output = model.run(input, tile)?;
            if output.len() != 3 * side * side {
                return Err(Error::Run(format!("a tile came back with {} values", output.len())));
            }
            let jj = w_i * tiling.out_step;
            if jj >= width {
                continue;
            }
            let span = side.min(width - jj);
            for ry in 0..side {
                if h_i * tiling.out_step + ry >= height {
                    break;
                }
                for rx in 0..span {
                    let at = ry * width + jj + rx;
                    let old = weights[at];
                    let new = old + filter[ry * side + rx];
                    let kept = old / new;
                    for c in 0..3 {
                        let slot = &mut pixels[c * side * width + at];
                        *slot = *slot * kept + output[(c * side + ry) * side + rx] * (1.0 - kept);
                    }
                    weights[at] = new;
                }
            }
        }
        let top = h_i * tiling.out_step;
        let done = if h_i + 1 == tiling.h_blocks { side } else { tiling.out_step };
        for ry in 0..done.min(height.saturating_sub(top)) {
            for c in 0..3 {
                let src = &pixels[c * side * width + ry * width..][..width];
                for (dst, value) in row[c * width..][..width].iter_mut().zip(src) {
                    *dst = value.clamp(0.0, 1.0);
                }
            }
            emit(&row);
        }
        // Slide the band: the overlap the next tile row blends into moves up.
        let carried = side - tiling.out_step;
        for c in 0..3 {
            let band = &mut pixels[c * side * width..][..side * width];
            band.copy_within(tiling.out_step * width.., 0);
            band[carried * width..].fill(0.0);
        }
        weights.copy_within(tiling.out_step * width.., 0);
        weights[carried * width..].fill(0.0);
    }
    Ok(())
}

/* ------------------------------------------------------------------ */
/* The downscale                                                       */
/* ------------------------------------------------------------------ */

fn srgb_to_linear(x: f32) -> f32 {
    if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
}

fn linear_to_srgb(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x <= 0.003_130_8 { x * 12.92 } else { 1.055 * x.powf((1.0f64 / 2.4) as f32) - 0.055 }
}

fn sinc(x: f64) -> f64 {
    if x == 0.0 {
        return 1.0;
    }
    let x = x * std::f64::consts::PI;
    x.sin() / x
}

fn lanczos(x: f64) -> f64 {
    if (-3.0..3.0).contains(&x) { sinc(x) * sinc(x / 3.0) } else { 0.0 }
}

/// Pillow's `precompute_coeffs` for `LANCZOS` along one axis: for each output
/// sample, the first input sample and the normalised float64 weights.
struct Axis {
    starts: Vec<usize>,
    weights: Vec<Vec<f64>>,
}

impl Axis {
    fn new(in_size: usize, out_size: usize) -> Axis {
        let scale = in_size as f64 / out_size as f64;
        let filterscale = scale.max(1.0);
        let support = 3.0 * filterscale;
        let ss = 1.0 / filterscale;
        let (mut starts, mut weights) = (Vec::with_capacity(out_size), Vec::with_capacity(out_size));
        for xx in 0..out_size {
            let center = (xx as f64 + 0.5) * scale;
            // C's `(int)` truncates toward zero, as `as` does.
            let xmin = ((center - support + 0.5) as i64).max(0) as usize;
            let xmax = ((center + support + 0.5) as i64).min(in_size as i64) as usize;
            let mut kernel: Vec<f64> =
                (xmin..xmax.max(xmin)).map(|x| lanczos((x as f64 - center + 0.5) * ss)).collect();
            let total: f64 = kernel.iter().sum();
            if total != 0.0 {
                kernel.iter_mut().for_each(|weight| *weight /= total);
            }
            starts.push(xmin);
            weights.push(kernel);
        }
        Axis { starts, weights }
    }

    /// The input samples output `index` reads, `start..end`.
    fn span(&self, index: usize) -> (usize, usize) {
        (self.starts[index], self.starts[index] + self.weights[index].len())
    }
}

/// `downscale_linear_lanczos`, streamed: canvas rows in (already in linear
/// light), output rows out. Pillow's order: the horizontal pass first, kept
/// as float32, then the vertical pass over it.
struct Downscale {
    channels: usize,
    in_w: usize,
    out_w: usize,
    horizontal: Axis,
    vertical: Axis,
    /// Horizontal-pass rows `first..first + held.len()`, float32 as Pillow keeps them.
    held: VecDeque<Vec<f32>>,
    first: usize,
    received: usize,
    next: usize,
    /// sRGB output, `channels` planes of `out_w * out_h`.
    out: Vec<f32>,
    out_h: usize,
}

impl Downscale {
    fn new(channels: usize, in_w: usize, in_h: usize, out_w: usize, out_h: usize) -> Downscale {
        Downscale {
            channels,
            in_w,
            out_w,
            horizontal: Axis::new(in_w, out_w),
            vertical: Axis::new(in_h, out_h),
            held: VecDeque::new(),
            first: 0,
            received: 0,
            next: 0,
            out: vec![0.0; channels * out_w * out_h],
            out_h,
        }
    }

    /// One canvas row, `channels` planes of `in_w`, in linear light.
    fn push(&mut self, linear: &[f32]) {
        let mut shrunk = vec![0f32; self.channels * self.out_w];
        for c in 0..self.channels {
            let src = &linear[c * self.in_w..][..self.in_w];
            for (xx, slot) in shrunk[c * self.out_w..][..self.out_w].iter_mut().enumerate() {
                let (start, _) = self.horizontal.span(xx);
                let mut sum = 0f64;
                for (weight, value) in self.horizontal.weights[xx].iter().zip(&src[start..]) {
                    sum += f64::from(*value) * weight;
                }
                *slot = sum as f32;
            }
        }
        self.held.push_back(shrunk);
        self.received += 1;
        while self.next < self.out_h && self.vertical.span(self.next).1 <= self.received {
            self.emit_row();
        }
    }

    fn emit_row(&mut self) {
        let yy = self.next;
        let (start, _) = self.vertical.span(yy);
        let plane = self.out_w * self.out_h;
        for c in 0..self.channels {
            for xx in 0..self.out_w {
                let mut sum = 0f64;
                for (k, weight) in self.vertical.weights[yy].iter().enumerate() {
                    sum += f64::from(self.held[start + k - self.first][c * self.out_w + xx]) * weight;
                }
                self.out[c * plane + yy * self.out_w + xx] = linear_to_srgb(sum as f32);
            }
        }
        self.next += 1;
        // Rows no later output row reads are dropped: the kernels move down.
        let keep_from = if self.next < self.out_h { self.vertical.span(self.next).0 } else { self.received };
        while self.first < keep_from && !self.held.is_empty() {
            self.held.pop_front();
            self.first += 1;
        }
    }

    fn finish(self) -> Result<Vec<f32>, Error> {
        if self.next != self.out_h {
            return Err(Error::Run("the downscale was left short".into()));
        }
        Ok(self.out)
    }
}

/// `run_step` for one waifu2x step: tile, blend, fold grey, downscale in
/// linear light, blend with the input by `strength`, and encode.
pub fn denoise(
    raster: &Raster,
    model: &mut dyn TileModel,
    spec: Waifu2xModel,
    filter: &[f32],
    strength: f64,
) -> Result<Denoised, Error> {
    let page = decode_page(raster)?;
    let pixels = (page.width * page.height) as u64;
    if pixels * (spec.scale * spec.scale) as u64 > MAX_UPSCALED_PIXELS {
        return Err(Error::TooLarge("page is too large for this sharpen scale"));
    }
    let tiling = Tiling::new(spec, page.height, page.width);
    let channels = if page.gray { 1 } else { 3 };
    let (in_w, in_h) = (page.width * spec.scale, page.height * spec.scale);
    let mut downscale = Downscale::new(channels, in_w, in_h, page.width, page.height);
    let mut linear = vec![0f32; channels * in_w];
    upscale(&page, model, &tiling, filter, |row| {
        if page.gray {
            // `out.mean(axis=0)`, float32, in numpy's order.
            for (x, slot) in linear.iter_mut().enumerate() {
                *slot = srgb_to_linear((row[x] + row[in_w + x] + row[2 * in_w + x]) / 3.0);
            }
        } else {
            for (slot, value) in linear.iter_mut().zip(row) {
                *slot = srgb_to_linear(*value);
            }
        }
        downscale.push(&linear);
    })?;
    let out = downscale.finish()?;
    Ok(Denoised { raster: encode_page(&page, &out, strength)?, gray: page.gray })
}

/* ------------------------------------------------------------------ */
/* The session                                                         */
/* ------------------------------------------------------------------ */

struct OrtTile<'a>(&'a mut ort::session::Session);

impl TileModel for OrtTile<'_> {
    fn run(&mut self, tile: Vec<f32>, side: usize) -> Result<Vec<f32>, Error> {
        let input = ort::value::Tensor::from_array(([1usize, 3, side, side], tile))
            .map_err(|e| Error::Run(e.to_string()))?;
        let outputs = self.0.run(ort::inputs![input]).map_err(|e| Error::Run(e.to_string()))?;
        let (_, data) = outputs[0].try_extract_tensor::<f32>().map_err(|e| Error::Run(e.to_string()))?;
        Ok(data.to_vec())
    }
}

/// A tile model that asks `tick(done, total)` before each tile and stops the
/// page with [`Error::Cancelled`] when it answers false. `total` is the page's
/// tile count, so `done / total` is how far through the page the run is.
pub struct Watched<'a> {
    pub inner: &'a mut dyn TileModel,
    pub done: usize,
    pub total: usize,
    pub tick: &'a mut dyn FnMut(usize, usize) -> bool,
}

impl TileModel for Watched<'_> {
    fn run(&mut self, tile: Vec<f32>, side: usize) -> Result<Vec<f32>, Error> {
        if !(self.tick)(self.done, self.total) {
            return Err(Error::Cancelled);
        }
        let output = self.inner.run(tile, side)?;
        self.done += 1;
        Ok(output)
    }
}

/// The seam weight map, from nunif's helper graph, one channel of it: the
/// three it returns are the same map.
pub fn seam_filter(helper: &Path, model: Waifu2xModel) -> Result<Vec<f32>, Error> {
    let (mut session, _) = accel::open_session(helper, &SEAM_PROFILE, Preference::CpuOnly, Some(1))?;
    let scalar = |value: usize| {
        ort::value::Tensor::from_array(((), vec![value as i64])).map_err(|e| Error::Run(e.to_string()))
    };
    let outputs = session
        .run(ort::inputs![
            "scale" => scalar(model.scale)?,
            "offset" => scalar(model.offset)?,
            "tile_size" => scalar(model.tile)?,
        ])
        .map_err(|e| Error::Run(e.to_string()))?;
    let (_, data) = outputs[0].try_extract_tensor::<f32>().map_err(|e| Error::Run(e.to_string()))?;
    let side = model.output_side();
    if data.len() != 3 * side * side {
        return Err(Error::Run("the seam helper answered another size".into()));
    }
    Ok(data[..side * side].to_vec())
}

/// An open page denoise model: its session, where it landed, and its seam map.
pub struct Waifu2x {
    session: ort::session::Session,
    selection: Selection,
    model: Waifu2xModel,
    filter: Vec<f32>,
}

impl Waifu2x {
    /// Open the model on `preference` and read the seam map once. Costs seconds.
    pub fn open(model_path: &Path, helper: &Path, model: Waifu2xModel, preference: Preference) -> Result<Waifu2x, Error> {
        let (session, selection) = accel::open_session(model_path, &PROFILE, preference, None)?;
        let filter = seam_filter(helper, model)?;
        Ok(Waifu2x { session, selection, model, filter })
    }

    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    /// One mid-grey tile, so a timing does not include the provider's first-run setup.
    pub fn warm_up(&mut self) -> Result<(), Error> {
        let side = self.model.tile;
        OrtTile(&mut self.session).run(vec![0.5; 3 * side * side], side).map(drop)
    }

    pub fn denoise(&mut self, page: &Raster, strength: f64) -> Result<Denoised, Error> {
        self.denoise_watched(page, strength, &mut |_, _| true)
    }

    /// [`Waifu2x::denoise`], asking `tick` before each tile ([`Watched`]).
    pub fn denoise_watched(
        &mut self,
        page: &Raster,
        strength: f64,
        tick: &mut dyn FnMut(usize, usize) -> bool,
    ) -> Result<Denoised, Error> {
        let total = Tiling::new(self.model, page.height as usize, page.width as usize).tiles();
        let mut session = OrtTile(&mut self.session);
        let mut watched = Watched { inner: &mut session, done: 0, total, tick };
        denoise(page, &mut watched, self.model, &self.filter, strength)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// The waifu2x contract with no model: nearest upscale, `offset` cropped
    /// from each side, as `FakeModelSession` in `test_denoise.py`.
    struct Nearest {
        model: Waifu2xModel,
        calls: usize,
    }

    impl TileModel for Nearest {
        fn run(&mut self, tile: Vec<f32>, side: usize) -> Result<Vec<f32>, Error> {
            self.calls += 1;
            let Waifu2xModel { scale, offset, .. } = self.model;
            let out = side * scale - 2 * offset;
            let mut y = vec![0f32; 3 * out * out];
            for c in 0..3 {
                for oy in 0..out {
                    for ox in 0..out {
                        y[(c * out + oy) * out + ox] =
                            tile[(c * side + (oy + offset) / scale) * side + (ox + offset) / scale];
                    }
                }
            }
            Ok(y)
        }
    }

    /// nunif's `create_seam_blending_filter`: `BLEND_SIZE` constant-value rings
    /// padded around a block of ones, so a weight is its ring's by the
    /// Chebyshev distance to the edge. The parity test checks the helper graph
    /// answers exactly this.
    fn ring_filter(model: Waifu2xModel) -> Vec<f32> {
        let side = model.output_side();
        let ramp: Vec<f32> = (0..side)
            .map(|i| {
                let d = i.min(side - 1 - i);
                if d < BLEND_SIZE { 1.0 - (BLEND_SIZE - d) as f32 / (BLEND_SIZE + 1) as f32 } else { 1.0 }
            })
            .collect();
        (0..side * side).map(|at| ramp[at / side].min(ramp[at % side])).collect()
    }

    /// A seeded xorshift, so the "random" pages are the same on every run.
    fn noise(seed: u64, count: usize) -> Vec<f32> {
        let mut state = seed;
        (0..count)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 40) as f32 / (1u64 << 24) as f32
            })
            .collect()
    }

    fn raster(width: u32, height: u32, mode: ColorMode, depth: BitDepth, samples: impl Fn(u32, u32, usize) -> u16) -> Raster {
        let mut raster = Raster {
            width, height, mode, depth, icc: None, palette: None, trns: None, srgb_intent: None, color: Default::default(), data: Vec::new(),
        };
        raster.data = vec![0; raster.stride() * height as usize];
        for y in 0..height {
            for x in 0..width {
                for c in 0..mode.samples() {
                    raster.set_sample(x, y, c, samples(x, y, c));
                }
            }
        }
        raster
    }

    #[test]
    fn managed_page_boundary_preserves_native_metadata_precision_and_transparency() {
        let mut source = raster(4, 2, ColorMode::Rgba, BitDepth::Sixteen, |x, _, c| if c == 3 { if x == 0 { 0 } else { 12345 } } else { 16384 });
        source.color.gamma = Some(100000);
        let page = decode_page(&source).unwrap();
        // IEC sRGB encoded value of linear 0.25, independent of the implementation.
        assert!((page.rgb[1] - 0.5370987).abs() < 0.0001);
        let out = encode_page(&page, &[0.5; 8], 1.0).unwrap();
        assert_eq!(out.mode, source.mode);
        assert_eq!(out.depth, source.depth);
        assert_eq!(out.color, source.color);
        assert_eq!(&out.data[..8], &source.data[..8], "transparent native pixel remains exact");
        for c in 0..3 { assert!((out.sample(1,0,c) as i32 - 14027).abs() <= 3); }
        assert_eq!(out.sample(1,0,3),12345);
        assert_eq!(encode_page(&page, &[1.0;8],0.0).unwrap().data,source.data);
    }

    #[test]
    fn swin_tiles_are_the_sizes_nunif_allows() {
        assert_eq!(swin_tile(256), 256);
        assert_eq!(swin_tile(400), 400);
        assert_eq!(swin_tile(64), 64);
        assert_eq!(swin_tile(100), 112);
        assert_eq!(Waifu2xModel::swin(4), Waifu2xModel { scale: 4, offset: 32, tile: 256 });
        assert_eq!(Waifu2xModel::swin(2), Waifu2xModel { scale: 2, offset: 16, tile: 400 });
        // A 1284x1809 page at 4x: 236-pixel steps, 8 tile rows of 6, 960-pixel outputs.
        let tiling = Tiling::new(Waifu2xModel::swin(4), 1809, 1284);
        assert_eq!((tiling.in_offset, tiling.in_step, tiling.out_step), (8, 236, 944));
        assert_eq!((tiling.h_blocks, tiling.w_blocks, tiling.tiles()), (8, 6, 48));
        assert_eq!(tiling.model.output_side(), 960);
    }

    /// `test_waifu2x_tiling_matches_a_plain_upscale`: with a model that only
    /// upscales, tiling, padding and seam blending must give back the plain
    /// nearest upscale of the page, every canvas row exactly once, in order.
    #[test]
    fn tiling_matches_a_plain_upscale() {
        let (width, height) = (517usize, 203usize);
        let values = noise(7, 3 * width * height);
        let page = Page {
            width, height, rgb: values.clone(), gray: false,
            source: Raster { width: width as u32, height: height as u32, mode: ColorMode::Rgb, depth: BitDepth::Eight,
                icc: None, palette: None, trns: None, srgb_intent: None, color: Default::default(), data: vec![0;3*width*height] },
        };
        for (scale, offset) in [(4, 32), (2, 16), (1, 8)] {
            let model = Waifu2xModel { scale, offset, tile: swin_tile(64) };
            let tiling = Tiling::new(model, height, width);
            let mut fake = Nearest { model, calls: 0 };
            let (out_w, mut rows) = (width * scale, 0usize);
            let mut worst = 0f32;
            upscale(&page, &mut fake, &tiling, &ring_filter(model), |row| {
                assert_eq!(row.len(), 3 * out_w);
                for c in 0..3 {
                    for x in 0..out_w {
                        let expected = values[c * width * height + (rows / scale) * width + x / scale];
                        worst = worst.max((row[c * out_w + x] - expected).abs());
                    }
                }
                rows += 1;
            }).unwrap();
            assert_eq!(rows, height * scale, "{scale}x: every canvas row once");
            assert_eq!(fake.calls, tiling.tiles(), "{scale}x");
            assert!(fake.calls > 4, "{scale}x: the page spans several tiles in both directions");
            assert!(worst <= 1e-5, "{scale}x: {worst}");
        }
    }

    /// Pillow's `Image.resize(LANCZOS)` on a mode "F" image, from Pillow 11.3:
    /// a 40x24 ramp pattern `((7x + 13y) % 17) / 16` down to 10x6.
    #[test]
    fn lanczos_matches_pillow() {
        const PILLOW: [f64; 60] = [
            0.50233393907547, 0.48766764998435974, 0.48981088399887085, 0.5181058645248413, 0.5055941343307495,
            0.49073919653892517, 0.4849039614200592, 0.5142780542373657, 0.5087320804595947, 0.5015091300010681,
            0.4898247718811035, 0.5040501356124878, 0.5027869343757629, 0.49661386013031006, 0.4980524480342865,
            0.5011545419692993, 0.5040391087532043, 0.4980245530605316, 0.4963039755821228, 0.504713237285614,
            0.5047152042388916, 0.4995288848876953, 0.499765545129776, 0.49964016675949097, 0.5001006722450256,
            0.5006799101829529, 0.49963200092315674, 0.4997003972530365, 0.49906232953071594, 0.5040384531021118,
            0.5098321437835693, 0.497936874628067, 0.4996635615825653, 0.5001245141029358, 0.5003231763839722,
            0.5003679990768433, 0.4993200898170471, 0.499478816986084, 0.5029963254928589, 0.48847508430480957,
            0.49416035413742065, 0.4976183772087097, 0.49958017468452454, 0.5028570890426636, 0.5021250247955322,
            0.49596095085144043, 0.4988454580307007, 0.5020126700401306, 0.5031692385673523, 0.49724021553993225,
            0.49564704298973083, 0.5156679749488831, 0.5058272480964661, 0.4893047511577606, 0.48624688386917114,
            0.5150960683822632, 0.5092608332633972, 0.4948825240135193, 0.4796004295349121, 0.523206889629364,
        ];
        let (w, h) = (40usize, 24usize);
        let mut downscale = Downscale::new(1, w, h, 10, 6);
        for y in 0..h {
            let row: Vec<f32> = (0..w).map(|x| ((x * 7 + y * 13) % 17) as f32 / 16.0).collect();
            downscale.push(&row);
            assert!(downscale.held.len() <= 25, "only the rows under one kernel are held");
        }
        // `push` converts to sRGB on the way out; undo it to compare Pillow's own pass.
        let out = downscale.finish().unwrap();
        for (at, (got, want)) in out.iter().zip(PILLOW).enumerate() {
            let linear = f64::from(srgb_to_linear(*got));
            assert!((linear - want).abs() < 2e-6, "{at}: {linear} vs {want}");
        }
    }

    fn run(raster: &Raster) -> Denoised {
        let model = Waifu2xModel { scale: 4, offset: 32, tile: 64 };
        denoise(raster, &mut Nearest { model, calls: 0 }, model, &ring_filter(model), 1.0).unwrap()
    }

    #[test]
    fn a_watched_page_counts_its_tiles_and_stops_when_told() {
        let model = Waifu2xModel { scale: 4, offset: 32, tile: 64 };
        let page = raster(90, 70, ColorMode::Gray, BitDepth::Eight, |x, y, _| (x + y) as u16);
        let total = Tiling::new(model, 70, 90).tiles();
        assert!(total > 2);

        let mut seen = Vec::new();
        let mut inner = Nearest { model, calls: 0 };
        let mut tick = |done: usize, of: usize| { seen.push((done, of)); true };
        let mut watched = Watched { inner: &mut inner, done: 0, total, tick: &mut tick };
        denoise(&page, &mut watched, model, &ring_filter(model), 1.0).unwrap();
        assert_eq!(seen, (0..total).map(|done| (done, total)).collect::<Vec<_>>());

        let mut inner = Nearest { model, calls: 0 };
        let mut tick = |done: usize, _: usize| done < 2;
        let mut watched = Watched { inner: &mut inner, done: 0, total, tick: &mut tick };
        let stopped = denoise(&page, &mut watched, model, &ring_filter(model), 1.0).unwrap_err();
        assert!(matches!(stopped, Error::Cancelled));
        assert_eq!(stopped.code(), "denoise_cancelled");
        assert_eq!(inner.calls, 2, "no tile runs after the stop");
    }

    #[test]
    fn a_flat_page_stays_flat_and_the_same_size() {
        for (mode, depth, level) in [
            (ColorMode::Gray, BitDepth::Eight, 77u16),
            (ColorMode::Rgb, BitDepth::Eight, 200),
            (ColorMode::Gray, BitDepth::Sixteen, 40_000),
        ] {
            let page = raster(37, 23, mode, depth, |_, _, c| if mode == ColorMode::Rgb { level - c as u16 * 60 } else { level });
            let out = run(&page).raster;
            assert_eq!((out.width, out.height, out.mode, out.depth), (37, 23, mode, depth));
            for y in 0..23 {
                for x in 0..37 {
                    for c in 0..mode.samples() {
                        let (got, want) = (out.sample(x, y, c) as i32, page.sample(x, y, c) as i32);
                        let slack = if depth == BitDepth::Sixteen { 3 } else { 0 };
                        assert!((got - want).abs() <= slack, "{mode:?} ({x}, {y}, {c}): {got} vs {want}");
                    }
                }
            }
        }
    }

    #[test]
    fn grey_stays_grey_alpha_is_copied_and_depth_is_kept() {
        // An RGB page whose channels differ by one level is grey, and comes back grey.
        let near = raster(20, 12, ColorMode::Rgb, BitDepth::Eight, |x, y, c| (x * 9 + y * 3) as u16 + (c == 1) as u16);
        let denoised = run(&near);
        assert!(denoised.gray);
        assert_eq!(denoised.raster.mode, ColorMode::Rgb);
        // Two levels apart is colour.
        let colour = raster(20, 12, ColorMode::Rgb, BitDepth::Eight, |x, _, c| x as u16 * 10 + 2 * c as u16);
        let denoised = run(&colour);
        assert!(!denoised.gray);
        assert_eq!(denoised.raster.mode, ColorMode::Rgb);

        let rgba = raster(19, 11, ColorMode::Rgba, BitDepth::Eight, |x, y, c| if c == 3 { (x * 11 + y) as u16 } else { 200 });
        let out = run(&rgba).raster;
        assert_eq!(out.mode, ColorMode::Rgba, "grey RGBA preserves its native storage");
        for y in 0..11 {
            for x in 0..19 {
                assert_eq!(out.sample(x, y, 3), rgba.sample(x, y, 3), "alpha at ({x}, {y})");
            }
        }
        // Fully opaque native alpha is retained.
        let opaque = raster(9, 9, ColorMode::GrayAlpha, BitDepth::Eight, |_, _, c| if c == 1 { 255 } else { 90 });
        assert_eq!(run(&opaque).raster.mode, ColorMode::GrayAlpha);

        let sixteen = raster(17, 13, ColorMode::GrayAlpha, BitDepth::Sixteen, |x, _, c| if c == 1 { 1000 + x as u16 } else { 30_000 });
        let out = run(&sixteen).raster;
        assert_eq!((out.mode, out.depth), (ColorMode::GrayAlpha, BitDepth::Sixteen));
        assert_eq!(out.sample(5, 3, 1), 1005);
    }

    #[test]
    fn bitonal_indexed_and_cmyk_pages_are_declined() {
        let mut indexed = raster(4, 4, ColorMode::Indexed, BitDepth::Eight, |_, _, _| 0);
        indexed.palette = Some(vec![0; 3]);
        let cmyk = raster(4, 4, ColorMode::Cmyk, BitDepth::Eight, |_, _, _| 0);
        let bitonal = raster(8, 4, ColorMode::Gray, BitDepth::One, |x, _, _| (x % 2) as u16);
        let model = Waifu2xModel::swin(4);
        for page in [indexed, cmyk, bitonal] {
            let mut fake = Nearest { model, calls: 0 };
            let error = denoise(&page, &mut fake, model, &ring_filter(model), 1.0).unwrap_err();
            assert_eq!(error.code(), "denoise_unsupported_page", "{:?}", page.mode);
            assert_eq!(fake.calls, 0, "declined before any tile runs");
        }
    }

    #[test]
    fn strength_blends_with_the_input() {
        let page = raster(16, 16, ColorMode::Gray, BitDepth::Eight, |x, _, _| if x < 8 { 0 } else { 255 });
        let model = Waifu2xModel { scale: 4, offset: 32, tile: 64 };
        let none = denoise(&page, &mut Nearest { model, calls: 0 }, model, &ring_filter(model), 0.0).unwrap();
        assert_eq!(none.raster.data, page.data, "strength 0 is the input");
    }

    #[test]
    fn only_the_waifu2x_scan_preset_runs_locally() {
        use crate::cloud_denoise_wire::PRESETS;
        for preset in PRESETS {
            assert_eq!(local_step(&preset.recipe()).is_ok(), preset.runs_locally(), "{}", preset.id);
        }
        let step = local_step(&crate::cloud_denoise_wire::preset(LOCAL_PRESET).unwrap().recipe()).unwrap();
        assert_eq!((step.model, step.file, step.strength), (Waifu2xModel::swin(4), MODEL_FILE, 1.0));
    }

    /* -------------------------------------------------------------- */
    /* The real model, against the cloud code's own output             */
    /* -------------------------------------------------------------- */

    fn env_path(name: &str) -> Option<PathBuf> {
        std::env::var_os(name).map(PathBuf::from)
    }

    /// `cpu`, `auto`, or a provider's label without `accel.` (`coreml`, `webgpu`).
    fn preference_named(name: &str) -> Preference {
        match name {
            "cpu" => Preference::CpuOnly,
            "auto" => Preference::Automatic,
            other => Preference::Force(
                crate::accel::KNOWN.into_iter().find(|a| a.label_key() == format!("accel.{other}")).expect("a provider"),
            ),
        }
    }

    fn open_real(preference: Preference) -> Waifu2x {
        let runtime = crate::runtime::find(None).expect("set ORT_DYLIB_PATH to an ONNX Runtime");
        crate::runtime::load(&runtime).expect("the ONNX Runtime loads");
        let models = env_path("MC_DENOISE_MODELS").expect("MC_DENOISE_MODELS: the folder with both model files");
        Waifu2x::open(&models.join(MODEL_FILE), &models.join(SEAM_FILTER_FILE), Waifu2xModel::swin(4), preference)
            .expect("the model opens")
    }

    /// Runs the engine on every `<name>-input.png` in `MC_DENOISE_PARITY` and
    /// compares it with `<name>-golden.png`, which `denoise.py` wrote on CPU
    /// for the same input. Also checks the helper graph answers the ring map
    /// the unit tests use. Then, with `MC_DENOISE_PAGE`, times a whole page.
    ///
    /// `MC_DENOISE_MODELS=<dir> MC_DENOISE_PARITY=<dir> ORT_DYLIB_PATH=<dylib>
    /// cargo test -p cleaner-core --release page_denoise::tests::matches_the_cloud -- --ignored --nocapture`
    #[test]
    #[ignore = "needs the model files, the ONNX Runtime and golden pages; see the doc comment"]
    fn matches_the_cloud_runtime_on_a_real_page() {
        let provider = std::env::var("MC_DENOISE_PROVIDER").unwrap_or_else(|_| "cpu".into());
        let mut engine = open_real(preference_named(&provider));
        println!("{provider}: landed on {:?}", engine.selection().accelerator);
        let model = Waifu2xModel::swin(4);
        let worst = engine.filter.iter().zip(ring_filter(model)).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        assert!(worst < 1e-6, "the seam helper is the ring map ({worst})");

        let dir = env_path("MC_DENOISE_PARITY").expect("MC_DENOISE_PARITY: the folder with the golden pages");
        let mut compared = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix("-input.png")) else {
                continue;
            };
            let input = crate::image::decode(&std::fs::read(&path).unwrap()).unwrap();
            let golden = crate::image::decode(&std::fs::read(dir.join(format!("{name}-golden.png"))).unwrap()).unwrap();
            let started = std::time::Instant::now();
            let out = engine.denoise(&input, 1.0).unwrap().raster;
            let seconds = started.elapsed().as_secs_f64();
            assert_eq!((out.width, out.height, out.mode, out.depth), (golden.width, golden.height, golden.mode, golden.depth));
            let (mut max, mut sum, mut count) = (0i32, 0f64, 0usize);
            for y in 0..out.height {
                for x in 0..out.width {
                    for c in 0..out.mode.samples() {
                        let diff = (out.sample(x, y, c) as i32 - golden.sample(x, y, c) as i32).abs();
                        max = max.max(diff);
                        sum += f64::from(diff);
                        count += 1;
                    }
                }
            }
            println!("{name}: {}x{} {:?}, max diff {max}/255, mean {:.5}/255, {seconds:.2} s",
                out.width, out.height, out.mode, sum / count as f64);
            assert!(max <= 2, "{name}: max diff {max}/255");
            compared += 1;
        }
        assert!(compared > 0, "no <name>-input.png in {}", dir.display());
    }

    /// Seconds per page for `MC_DENOISE_PAGE` on each provider this machine
    /// has, after one warm-up tile, and where the session actually landed.
    #[test]
    #[ignore = "needs the model files, the ONNX Runtime and a page; see matches_the_cloud_runtime_on_a_real_page"]
    fn times_a_real_page_per_provider() {
        let page = env_path("MC_DENOISE_PAGE").expect("MC_DENOISE_PAGE: a page to time");
        let page = crate::image::decode(&std::fs::read(page).unwrap()).unwrap();
        let wanted = std::env::var("MC_DENOISE_PROVIDERS").unwrap_or_else(|_| "cpu".into());
        for name in wanted.split(',') {
            let preference = preference_named(name);
            let runtime = crate::runtime::find(None).expect("ORT_DYLIB_PATH");
            crate::runtime::load(&runtime).expect("the ONNX Runtime loads");
            let models = env_path("MC_DENOISE_MODELS").expect("MC_DENOISE_MODELS");
            if let Preference::Force(_) = preference {
                // Strict placement: does the provider hold the whole graph?
                let strict = ModelProfile { partitioned_on: &[], ..PROFILE };
                let whole = accel::open_session(&models.join(MODEL_FILE), &strict, preference, None);
                println!("{name}: whole graph on the provider: {}", whole.is_ok());
            }
            let opened = std::time::Instant::now();
            let mut engine = match Waifu2x::open(&models.join(MODEL_FILE), &models.join(SEAM_FILTER_FILE), Waifu2xModel::swin(4), preference) {
                Ok(engine) => engine,
                Err(error) => {
                    println!("{name}: no session: {error}");
                    continue;
                }
            };
            let build = opened.elapsed().as_secs_f64();
            let warm = std::time::Instant::now();
            if let Err(error) = engine.warm_up() {
                println!("{name}: warm-up failed: {error}");
                continue;
            }
            let warm = warm.elapsed().as_secs_f64();
            let started = std::time::Instant::now();
            let out = engine.denoise(&page, 1.0);
            println!("{name}: landed on {:?} (declined {:?}), build {build:.1} s, warm-up tile {warm:.2} s, page {:.1} s ({})",
                engine.selection().accelerator, engine.selection().declined.map(|d| d.reason_key),
                started.elapsed().as_secs_f64(), out.map(|d| format!("gray {}", d.gray)).unwrap_or_else(|e| e.to_string()));
        }
    }
}
