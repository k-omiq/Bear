//! The geometry a model rung is run inside, held apart from the model.
//!
//! This was extracted when the ladder carried **two** in-process model rungs  - 
//! the manga-finetuned LaMa, with a fixed 512² float input and `DFT` in the
//! middle of it, and MI-GAN, a 28 MB GAN pipeline with dynamic uint8 tensors
//! and an inverted mask. They differed in almost everything a reader would
//! notice, and yet the geometry they were run inside was the same geometry,
//! because it is the pipeline's and not the model's.
//!
//! MI-GAN has since been removed, and this module stays, because the argument
//! for it never rested on there being two of them. The sizing rule is a rule
//! about the **window**: the decode window contains an
//! `engine_context(512² for LaMa)`, and "boxes larger than the engine input are
//! tiled with 128 px overlap". One number, named once, and rung 3a
//! ([`super::flux`]) reads it from here too.
//!
//! So this module holds the parts a second model rung would otherwise copy: the
//! tiling, the applied mask and the write bound, the decline vocabulary, the
//! isolation ramp, and the small conversions between a [`Raster`]'s samples and
//! a tensor's. It was extracted from [`crate::engines::lama`] when rung 3
//! arrived rather than written ahead of it, and rule 4 is what
//! makes its constants rung-independent.
//!
//! What is deliberately *not* here is the run itself. The two rungs disagree
//! about the tensor's dtype, about which polarity means "hole", about whether
//! the graph masks its own input, and about how wide a margin the model's own
//! internal blend needs - and each of those is a measurement against one file
//! rather than a property of model rungs in general. A trait that unified them
//! would have to be parameterised on all four, which is a longer way of writing
//! the two loops out.

use crate::constants::{EDIT_MARGIN, ISOLATION_RADIUS};
use crate::fit::Fitted;
use crate::image::{BitDepth, ColorMode, Raster};
use crate::mask::{Mask, Rect};
use crate::strip::window::{ENGINE_INPUT, EdgePad};

/// The spatial input model rungs are run at. Shared with
/// [`crate::strip::window::ENGINE_INPUT`] rather than restated, because rule
/// 4's decode window is sized against this same number and two copies of it
/// would eventually disagree.
///
/// For rung 2 this is the export's own fixed shape and there is no other
/// choice.
pub const MODEL_INPUT: u32 = ENGINE_INPUT;

/// §3's tile overlap, in native pixels.
pub const TILE_OVERLAP: u32 = 128;

/// How far one tile advances. The overlap is what the next tile gets as
/// context, so the stride is what is left.
pub const TILE_STRIDE: u32 = MODEL_INPUT - TILE_OVERLAP;

/// How much of the overlap a tile leaves for its successor to fill. Half, so
/// both sides of every join have the same amount of settled context.
pub const TILE_HANDOFF: u32 = TILE_OVERLAP / 2;

/// §3's decline threshold: "Box > 4× the model input in either dimension →
/// `decline`, and say so in review."
pub const MAX_BOX: u32 = 4 * MODEL_INPUT;

/// The width of the hard feather inside the isolation margin
/// ("composite through the local
/// mask + 5 px isolation, hard radius-2 feather inside that margin").
///
/// A hard radius-truncated ramp and never a Gaussian: a σ=1 Gaussian
/// spreads non-zero alpha 2–3 px past its radius and would put the rung outside
/// `edit_margin`.
pub const ISOLATION_FEATHER: u32 = 2;

/// Why a region got no patch from a model rung.
///
/// A decline is not a failure. It is a defined outcome - the region is
/// left exactly as it was and listed in review with its reason - so each
/// variant carries an i18n key rather than a sentence. No English crosses the
/// seam.
///
/// The enum is shared by both rungs and neither emits all of it. That is the
/// intended shape: a reviewer comparing two rungs' review rows is comparing one
/// vocabulary, and a rung that grows a reason its neighbour already has should
/// not name it twice ([`Decline::reason_key`] is the string the interface sees,
/// and a second spelling of it is a second translation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Decline {
    /// The source's colour mode cannot carry a model rung's output at all:
    /// indexed, because continuous tone is unrepresentable in a palette, and
    /// CMYK, because the round trip through RGB is not invertible.
    #[error("{0:?} cannot carry a model rung's output")]
    Mode(ColorMode),
    /// One bit per sample. See [`crate::engines::lama`]'s module note - this one
    /// is not in the documents.
    #[error("{0:?} bits per sample is not a level a continuous-tone model can write into")]
    Depth(BitDepth),
    /// The *source* has more precision than the engine does. Rung 3's tensors
    /// were uint8 in and uint8 out, so a 16-bit page routed through it would come
    /// back quantised to 257ths of its own range inside the mask: uint8 in and
    /// uint8 out forecloses a 16-bit path through this rung. Rung 2 never emits
    /// this; its tensors are float32 and 16 bits survive them.
    #[error("{0:?} bits per sample is finer than this engine's uint8 tensors")]
    DepthBeyondEngine(BitDepth),
    /// Past 4× the model input in either dimension.
    #[error("a {w}×{h} box is larger than {MAX_BOX} px in one dimension")]
    TooLarge { w: u32, h: u32 },

    /* -- rung 3a, the out-of-process one --------------------------------- */
    //
    // These six are [`crate::sidecar`]'s and no in-process rung emits one. They
    // are here rather than in an enum of their own because this module's note
    // above says why: the vocabulary is shared so that a reviewer comparing two
    // rungs' review rows is comparing one list, and a rung that grows a reason
    // its neighbour already has must not name it twice. Rung 3a's six are
    // genuinely new - none of them is a fact about the region at all, which is
    // the thing that makes them worth telling apart from the four above.
    /// The machine cannot carry the sidecar: quantized weights plus working set
    /// against the budget. The two figures
    /// travel on the variant, for the reason
    /// [`crate::accel::Declined::needed_bytes`] gives - a byte count is not a
    /// translatable sentence.
    #[error("the sidecar needs {needed} bytes and this machine has room for {room}")]
    SidecarMachine { needed: u64, room: u64 },
    /// The machine could not be classified, so it is declined rather than
    /// admitted. See [`crate::sidecar::hardware`] for why the safe half is the
    /// safe half here and is not obviously right.
    ///
    /// **This is no longer "not macOS".** [`crate::memory::room`] answers on
    /// Windows and Linux now, so what is left here is a probe that was made and
    /// failed - a sysctl, a `/proc` read, a `GlobalMemoryStatusEx`. A platform
    /// with no backend at all is [`Decline::SidecarPlatform`], which is a
    /// different sentence with a different remedy.
    #[error("this machine's memory could not be established, so the sidecar is not offered")]
    SidecarUnknownMachine,
    /// The **chosen** sidecar backend has no render path in this build on this
    /// platform, whatever the machine's memory turns out to be.
    ///
    /// **No longer a whole-platform refusal.** It was one while `mflux` was the
    /// only rendering backend - MLX, so Apple Silicon, so every Windows and
    /// Linux machine refused here. `sdnq` is torch and builds everywhere
    /// ([`crate::sidecar::backend`]), so what reaches this variant now is a
    /// *choice*: a user who selected `mflux` in Settings on a machine with no
    /// MLX, or a build asked for the declared-only `sdcpp` stub. The remedy is to pick
    /// another backend, which is why it stays a different sentence from
    /// [`Decline::SidecarBackendMissing`] below.
    #[error("this build has no sidecar backend for this platform")]
    SidecarPlatform,
    /// The sidecar is installed, the chosen backend renders on this platform,
    /// and **this virtual environment does not have its Python packages**.
    ///
    /// A venv may hold `mflux`'s dependencies, `sdnq`'s, or both, and the parent
    /// cannot see inside one - it found an interpreter beside a `pyvenv.cfg` and
    /// nothing more. The sidecar reports what it can import
    /// ([`crate::sidecar::wire::Health::backends`]) and this is the refusal when
    /// the answer does not include what was asked for.
    ///
    /// Its own sentence because its own remedy is the user's and is one command:
    /// `pip install -r sidecar/requirements-sdnq.txt`. Telling them their
    /// platform has no backend, or that their machine is too small, would send
    /// them somewhere there is nothing to do.
    #[error("this sidecar install does not have the chosen backend's packages")]
    SidecarBackendMissing,
    /// The sidecar's allocation cap fired -
    /// rule 9's third guard
    /// working as specified. The region routes to rung 2 and the run is a region
    /// worse rather than a machine worse.
    #[error("the sidecar ran out of the memory it was allowed")]
    SidecarMemory,
    /// The chosen backend has no bound this build knows how to impose or check.
    /// Rule 9: *"Adding a backend means adding its bound, or refusing to offer
    /// it."*
    #[error("this sidecar backend's memory cannot be bounded, so it is refused")]
    SidecarUnbounded,
    /// The sidecar is installed and has no weights on disk. Almost absence, and
    /// deliberately not the same sentence: absence is silent because the user
    /// never asked for the rung, and this is a user who did and is one download
    /// away.
    #[error("the sidecar has no model weights")]
    SidecarWeightsMissing,
}

impl Decline {
    /// The key the review row carries. Sits under `decline.reason`, beside
    /// §6's own `qualityMetric`, because both are the same kind of thing: a
    /// terminal outcome the pipeline reports for one region.
    pub fn reason_key(self) -> &'static str {
        match self {
            Decline::Mode(_) => "decline.reason.unrepresentableMode",
            Decline::Depth(_) => "decline.reason.unrepresentableDepth",
            Decline::DepthBeyondEngine(_) => "decline.reason.depthBeyondEngine",
            Decline::TooLarge { .. } => "decline.reason.tooLarge",
            Decline::SidecarMachine { .. } => "decline.reason.sidecarMachine",
            Decline::SidecarUnknownMachine => "decline.reason.sidecarUnknownMachine",
            Decline::SidecarPlatform => "decline.reason.sidecarPlatform",
            Decline::SidecarBackendMissing => "decline.reason.sidecarBackend",
            Decline::SidecarMemory => "decline.reason.sidecarMemory",
            Decline::SidecarUnbounded => "decline.reason.sidecarUnbounded",
            Decline::SidecarWeightsMissing => "decline.reason.sidecarWeights",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The region was refused before the model ran. Recoverable and expected;
    /// the router records it and moves on.
    #[error(transparent)]
    Declined(#[from] Decline),
    /// The model was asked and would not answer. Distinct from a decline
    /// because a decline is a decision and this is a fault, and the two are
    /// kept apart in the review row.
    #[error("the inpainter would not run: {0}")]
    Run(String),
}

/// Whether *any* model rung can run on this page. Rung-specific refusals sit on
/// top of this rather than replacing it.
pub fn applies(page: &Raster) -> bool {
    page.mode.allows_model_engines() && page.depth != BitDepth::One
}

/// Every reason a region would be declined by any model rung, before a session
/// is opened. A rung with its own additional refusal calls this first.
pub fn declines(page: &Raster, fitted: &Fitted) -> Option<Decline> {
    if !page.mode.allows_model_engines() {
        return Some(Decline::Mode(page.mode));
    }
    if page.depth == BitDepth::One {
        return Some(Decline::Depth(page.depth));
    }
    // The box a model rung would actually be run over, which is the one
    // [`applied_mask`] answers and therefore [`Fitted::ink`]'s and not the
    // fitted mask's. Asking the wider one would decline a region for the size of
    // a growth this rung no longer writes through.
    let box_ = fitted.ink.bounds;
    if box_.w > MAX_BOX || box_.h > MAX_BOX {
        return Some(Decline::TooLarge { w: box_.w, h: box_.h });
    }
    None
}

/// The mask a model rung writes through: [`Fitted::ink`] grown by the isolation
/// radius, in **one** dilation.
///
/// One dilation and not a composition:
/// `disc(3) ⊕ disc(2)` reaches further at the diagonals than `disc(5)` does,
/// and a margin is only a bound if the construction matches the arithmetic.
///
/// **`ink` and not `mask`**, which is the one thing here that is not
/// arithmetic. `Fitted::ink` carries the reason in full; the short form is that
/// `mask` is sized by what rung 0 may safely repaint with the region's own
/// paper, a model rung repaints with a tone it invented, and the two answers
/// stopped being interchangeable the moment the second one existed. Every
/// geometry downstream of this call - the tiles, the patch's bounds, the ramp,
/// the tensor's hole - narrows with it, so a model rung now reads *more* page as
/// context and writes less of it.
pub fn applied_mask(fitted: &Fitted, page_w: u32, page_h: u32) -> Mask {
    fitted.ink.dilated(ISOLATION_RADIUS, page_w, page_h)
}

/// The bounds of [`applied_mask`] for lettering bounded by `ink`, without the
/// mask: grown by [`ISOLATION_RADIUS`] and clipped to the page, as
/// [`Mask::dilated`] bounds it. What an estimate reads when it has the stored
/// hole and the page's size but not the page.
pub fn applied_bounds(ink: Rect, page_w: u32, page_h: u32) -> Rect {
    ink.grown(ISOLATION_RADIUS, page_w, page_h)
}

/// How far from [`Fitted::ink`] a model rung may write, which is what
/// `EDIT_MARGIN` is derived to cover. The margin is the isolation radius, so
/// this is the applied mask's own extent: no rung writes wider.
pub fn write_bound(fitted: &Fitted, page_w: u32, page_h: u32) -> Mask {
    fitted.ink.dilated(EDIT_MARGIN, page_w, page_h)
}

/// Where the tiles start along one axis, so that they cover `extent` from
/// `start` with at least [`TILE_OVERLAP`] between neighbours.
///
/// An extent at or under the model input is one tile **centred on the region**,
/// which is what makes the padding "real surrounding page pixels" rather than a
/// border: a 160×290 region sits in the middle of its 512² crop and the model
/// sees page on all four sides of it.
///
/// Above it the origins are spread evenly rather than stepped from the left,
/// so the overlap is shared out instead of being dumped entirely into the last
/// tile. Integer arithmetic throughout, because a tile origin computed in
/// floating point is a determinism bug waiting for a machine with a different
/// rounding mode.
pub fn tile_origins(start: i64, extent: u32) -> Vec<i64> {
    if extent <= MODEL_INPUT {
        return vec![start + extent as i64 / 2 - MODEL_INPUT as i64 / 2];
    }
    // How far the first tile has to travel to reach the last, and how many
    // stops it needs to keep every gap inside the overlap.
    let span = (extent - MODEL_INPUT) as i64;
    let count = (extent - MODEL_INPUT).div_ceil(TILE_STRIDE) as i64 + 1;
    (0..count).map(|i| start + span * i / (count - 1)).collect()
}

/// The 512² crops one region is covered by, in page coordinates, in the order
/// they are run.
pub fn plan(bounds: Rect) -> Vec<Rect> {
    let side = MODEL_INPUT;
    let xs = tile_origins(bounds.x, bounds.w);
    let ys = tile_origins(bounds.y, bounds.h);
    let mut tiles = Vec::with_capacity(xs.len() * ys.len());
    for y in &ys {
        for x in &xs {
            tiles.push(Rect::new(*x, *y, side, side));
        }
    }
    tiles
}

/// One region's patch, and the two facts about how it was made that
/// `params_snapshot` wants.
#[derive(Debug, Clone)]
pub struct Rendered {
    /// The **applied** mask: the fitted mask plus the isolation margin, which is the set
    /// the compositor is allowed to write through and the set recorded as
    /// `mask_sha256`.
    pub mask: Mask,
    /// Pixels covering `mask.bounds`, in the page's own mode and depth, with
    /// everything outside the mask copied from the page unchanged.
    pub pixels: Raster,
    /// Whether any tile ran off the edge of `page` and was edge-replicated.
    /// Rule 4: "Record which was used in `params_snapshot`."
    pub pad: EdgePad,
    /// How many model runs this region cost. One for a region inside 512²; up
    /// to 25 at the decline threshold.
    pub tiles: u32,
    /// What a [`crate::engines::render::Preprocessing::V2`] composite aligned
    /// the answer's tone by. `None` for an answer blended as it came back.
    pub tone: Option<crate::engines::render::ToneReport>,
}

/// One model run's geometry: what it reads, and what it is allowed to keep.
#[derive(Debug, Clone, Copy)]
pub struct Tile {
    /// The 512² crop handed to the model.
    pub crop: Rect,
    /// The part of that crop this tile commits - everything, less the hand-off
    /// band on any side another tile still follows.
    pub commit: Rect,
}

/// [`plan`], with each crop paired to the part of it that tile owns.
///
/// Tiles are laid out in raster order and each one **commits only its core**.
/// The next tile therefore opens with [`TILE_HANDOFF`] pixels of
/// already-inpainted page as ordinary unmasked context and fills onward from
/// it. That is what §3's "run each tile against the running composite" buys:
/// without it two neighbouring tiles invent two continuations of the same
/// screentone independently and the overlap is a cross-fade between two guesses
/// rather than one fill carried across.
pub fn tiles(bounds: Rect) -> Vec<Tile> {
    let columns = tile_origins(bounds.x, bounds.w).len();
    let rows = tile_origins(bounds.y, bounds.h).len();
    plan(bounds)
        .into_iter()
        .enumerate()
        .map(|(index, crop)| {
            let (column, row) = (index % columns, index / columns);
            Tile {
                crop,
                commit: Rect::new(
                    crop.x,
                    crop.y,
                    crop.w - if column + 1 < columns { TILE_HANDOFF } else { 0 },
                    crop.h - if row + 1 < rows { TILE_HANDOFF } else { 0 },
                ),
            }
        })
        .collect()
}

/// The alpha the model's answer is written at, as a function of position.
///
/// Three dilations of [`Fitted::ink`] taken directly rather than composed, for
/// [`applied_mask`]'s reason. `core` is where the model's answer is written
/// whole; the rings outside it ramp it into the page over
/// [`ISOLATION_FEATHER`] pixels and reach zero exactly at the applied mask's
/// edge.
///
/// The ramp is therefore a **contour of the lettering**, and that is the half of
/// the box artefact the geometry alone does not fix. A model's answer sits a
/// level or two off the page's tone; over a mask grown across half a panel that
/// offset has a long straight-ish edge to show itself along, and the eye reads
/// the edge as a box. Around `ink` the same offset fades out over two pixels
/// along the outline of the glyphs themselves, which is a shape the eye reads as
/// nothing at all.
#[derive(Debug, Clone)]
pub struct AlphaRamp {
    core: Mask,
    rings: Vec<Mask>,
}

impl AlphaRamp {
    pub fn new(fitted: &Fitted, page_w: u32, page_h: u32) -> AlphaRamp {
        let inner = ISOLATION_RADIUS - ISOLATION_FEATHER;
        AlphaRamp {
            core: fitted.ink.dilated(inner, page_w, page_h),
            rings: (inner + 1..=ISOLATION_RADIUS)
                .map(|r| fitted.ink.dilated(r, page_w, page_h))
                .collect(),
        }
    }

    pub fn at(&self, x: i64, y: i64) -> f64 {
        if self.core.contains(x, y) {
            return 1.0;
        }
        for (step, ring) in self.rings.iter().enumerate() {
            if ring.contains(x, y) {
                // A linear ramp sampled at its midpoints: one ring gives 1/2,
                // two give 2/3 and 1/3.
                return (self.rings.len() - step) as f64 / (self.rings.len() + 1) as f64;
            }
        }
        0.0
    }
}

/// Model replies are encoded sRGB. Blend only covered pixels in linear light,
/// then convert back at native precision; alpha and uncovered samples stay exact.
pub fn commit_srgb(
    managed: &crate::image::color::ManagedColor, patch: &mut Raster,
    x: u32, y: u32, rgb: [f32; 3], coverage: f64, dither: f64,
) -> Result<(), Error> {
    use crate::image::color::{alpha, linear, encoded};
    crate::image::color::validate_editing(patch).map_err(|e|Error::Run(e.to_string()))?;
    if coverage <= 0.0 || alpha(patch, x, y) == 0.0 { return Ok(()); }
    let original = managed.pixel(patch, x, y);
    let a = coverage as f32;
    let blended = std::array::from_fn(|c| encoded(linear(rgb[c].clamp(0.0, 1.0)) * a + linear(original[c]) * (1.0 - a)));
    let native = managed.from_srgb(blended, patch.mode).map_err(|e| Error::Run(e.to_string()))?;
    let ceiling = ceiling_for(patch.depth);
    for (channel, sample) in native.iter().enumerate().take(patch.mode.samples()) {
        if patch.mode.alpha_channel() == Some(channel) { continue; }
        patch.set_sample(x, y, channel, (f64::from(*sample) * ceiling + dither).round().clamp(0.0, ceiling) as u16);
    }
    crate::image::color::avoid_transparent_key(patch, x, y);
    Ok(())
}

pub fn editable_color(page: &Raster) -> Result<std::rc::Rc<crate::image::color::ManagedColor>, Error> {
    crate::image::color::validate_editing(page).map_err(|e|Error::Run(e.to_string()))?;
    crate::image::color::ManagedColor::for_raster(page).map_err(|e|Error::Run(e.to_string()))
}

/// Native gray samples repeat over three model channels.
pub fn source_channel(mode: ColorMode, channel: usize) -> usize {
    if matches!(mode, ColorMode::Gray | ColorMode::GrayAlpha) { 0 } else { channel }
}

/// The mode a model's three-channel answer is read back in: grey for a grey
/// page, including one stored as RGB or RGBA ([`Raster::is_neutral`]).
///
/// Both model rungs are colour models and neither returns exact grey. On an
/// RGBA grey scan (Deli Health chapter 11) the 54 LaMa patches read per
/// channel carried a median 11 and at most 18 levels of chroma, which the
/// cleaned page and its export kept, and which sent the whole page down the
/// colour path of a later page denoise. Read as grey, every channel of the
/// patch takes the mean of the model's three, as on a page stored grey.
pub fn answer_mode(page: &Raster) -> ColorMode {
    match page.mode {
        ColorMode::Rgb | ColorMode::Rgba if page.is_neutral() => ColorMode::Gray,
        mode => mode,
    }
}

/// Model output has
/// ~8-bit effective precision; dither at the isolation boundary so the
/// transition does not band.
///
/// A 4×4 ordered matrix rather than noise, so the same input still produces the
/// same output; one model quantum wide, because the quantum is exactly what is
/// being smeared; and only where the ramp is partial, which is the isolation
/// boundary the sentence names. Below 16 bits the page's own quantum is no
/// finer than the model's and there is nothing to dither with.
pub fn dither_at(depth: BitDepth, alpha: f64, x: i64, y: i64, ceiling: f64) -> f64 {
    if depth != BitDepth::Sixteen || alpha <= 0.0 || alpha >= 1.0 {
        return 0.0;
    }
    const BAYER: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];
    let cell = BAYER[y.rem_euclid(4) as usize][x.rem_euclid(4) as usize] as f64;
    (cell / 16.0 - 0.5) * (ceiling / 255.0)
}

/// The page over `bounds`, as a raster of its own. Everything but the geometry
/// travels, so the patch is in the page's mode, depth and palette and the
/// "no helpful promotion" rule holds by construction.
pub fn page_crop(page: &Raster, bounds: Rect) -> Raster {
    let samples = page.mode.samples();
    let mut crop = Raster {
        width: bounds.w,
        height: bounds.h,
        mode: page.mode,
        depth: page.depth,
        icc: page.icc.clone(),
        palette: page.palette.clone(),
        trns: page.trns.clone(),
        srgb_intent: page.srgb_intent,
        color: page.color.clone(),
        data: vec![0; {
            let bits = bounds.w as usize * samples * page.depth.bits() as usize;
            bits.div_ceil(8) * bounds.h as usize
        }],
    };
    for y in 0..bounds.h {
        for x in 0..bounds.w {
            for channel in 0..samples {
                let value = page.sample(bounds.x as u32 + x, bounds.y as u32 + y, channel);
                crop.set_sample(x, y, channel, value);
            }
        }
    }
    crop
}

pub fn ceiling_for(depth: BitDepth) -> f64 {
    match depth {
        BitDepth::Sixteen => u16::MAX as f64,
        other => ((1u32 << other.bits()) - 1) as f64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grey_page_stored_as_rgb_reads_its_answer_as_grey() {
        let rgba = |tint: u8| Raster {
            width: 3, height: 2, mode: ColorMode::Rgba, depth: BitDepth::Eight, icc: None, palette: None,
            trns: None, srgb_intent: None, color: Default::default(), data: [120 + tint, 120, 120, 255].repeat(6),
        };
        assert_eq!(answer_mode(&rgba(0)), ColorMode::Gray);
        assert_eq!(answer_mode(&rgba(1)), ColorMode::Gray, "one level apart is still grey");
        assert_eq!(answer_mode(&rgba(2)), ColorMode::Rgba);
        let mut sixteen = rgba(0);
        sixteen.depth = BitDepth::Sixteen;
        sixteen.data = [30_000u16, 30_000, 30_400, 65_535].repeat(6).iter().flat_map(|v| v.to_be_bytes()).collect();
        assert_eq!(answer_mode(&sixteen), ColorMode::Rgba, "400 of 65535 is past 1.5 of 255");
        let gray = Raster { mode: ColorMode::GrayAlpha, data: vec![9; 12], ..rgba(0) };
        assert_eq!(answer_mode(&gray), ColorMode::GrayAlpha);
    }

    #[test]
    fn managed_model_boundary_preserves_native_precision_alpha_and_uncovered_samples() {
        use crate::image::{decode, color::ManagedColor};
        let source = decode(include_bytes!("../../tests/fixtures/color-reference/gamma-chrm.png")).unwrap();
        let managed = ManagedColor::for_raster(&source).unwrap();
        let mut patch = page_crop(&source, Rect::new(0, 0, source.width, source.height));
        assert_eq!(patch.color, source.color);
        assert_eq!(patch.icc, source.icc);
        let before = patch.data.clone();
        commit_srgb(&managed, &mut patch, 0, 0, [1.0; 3], 0.0, 0.0).unwrap();
        assert_eq!(patch.data, before);
        commit_srgb(&managed, &mut patch, 0, 0, [0.5; 3], 1.0, 0.0).unwrap();
        // Independent IEC sRGB inverse: encoded 0.5 is linear 0.214041.
        for c in 0..3 { assert!((patch.sample(0,0,c) as i32 - 55).abs() <= 1); }
        assert_eq!(&patch.data[3..], &before[3..]);

        let mut gray = crate::image::fixtures::by_name("l16").raster;
        gray.color.gamma = Some(100000);
        gray.set_sample(0,0,0, 12345);
        let managed = ManagedColor::for_raster(&gray).unwrap();
        commit_srgb(&managed, &mut gray, 0, 0, [0.5;3], 1.0, 0.0).unwrap();
        assert!((gray.sample(0,0,0) as i32 - 14027).abs() <= 3);

        let mut rgba = crate::image::fixtures::by_name("rgba8").raster;
        rgba.set_sample(0,0,3,0);
        let before = rgba.data.clone();
        let managed = ManagedColor::for_raster(&rgba).unwrap();
        commit_srgb(&managed, &mut rgba, 0, 0, [1.0;3], 1.0, 0.0).unwrap();
        assert_eq!(rgba.data, before);
        rgba.set_sample(0,0,3,99);
        commit_srgb(&managed, &mut rgba, 0, 0, [1.0;3], 0.5, 0.0).unwrap();
        assert_eq!(rgba.sample(0,0,3),99);
    }

    #[test]
    fn model_output_cannot_turn_opaque_color_key_samples_transparent() {
        let mut page = crate::image::fixtures::by_name("rgb8").raster;
        page.trns=Some(vec![0;6]);
        for c in 0..3 { page.set_sample(0,0,c,100); }
        let managed=crate::image::color::ManagedColor::for_raster(&page).unwrap();
        commit_srgb(&managed,&mut page,0,0,[0.0;3],1.0,0.0).unwrap();
        assert_eq!([page.sample(0,0,0),page.sample(0,0,1),page.sample(0,0,2)],[0,0,1]);
        assert_eq!(crate::image::color::alpha(&page,0,0),1.0);
        assert_eq!(page.trns,Some(vec![0;6]));
    }

    #[test]
    fn model_boundary_blends_in_linear_light() {
        let mut page = crate::image::fixtures::by_name("rgb8").raster;
        for c in 0..3 { page.set_sample(0,0,c,0); }
        let managed = crate::image::color::ManagedColor::for_raster(&page).unwrap();
        commit_srgb(&managed, &mut page, 0, 0, [1.0;3], 0.5, 0.0).unwrap();
        // IEC sRGB encoding of 0.5 linear light is 187.516/255.
        for c in 0..3 { assert_eq!(page.sample(0,0,c),188); }
    }

    #[test]
    fn every_decline_names_a_key_and_no_two_name_the_same_one() {
        let all = [
            Decline::Mode(ColorMode::Cmyk),
            Decline::Depth(BitDepth::One),
            Decline::DepthBeyondEngine(BitDepth::Sixteen),
            Decline::TooLarge { w: 9000, h: 1 },
            Decline::SidecarMachine { needed: 1, room: 0 },
            Decline::SidecarUnknownMachine,
            Decline::SidecarPlatform,
            Decline::SidecarBackendMissing,
            Decline::SidecarMemory,
            Decline::SidecarUnbounded,
            Decline::SidecarWeightsMissing,
        ];
        let mut keys: Vec<&str> = all.iter().map(|d| d.reason_key()).collect();
        keys.sort_unstable();
        let count = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), count, "two declines share a review string");
        for key in keys {
            assert!(key.starts_with("decline.reason."), "{key} is outside the family");
        }
    }

    #[test]
    fn a_tiles_commit_is_its_crop_less_the_hand_off_to_whoever_follows() {
        // One tile owns all of itself; there is nobody to hand off to.
        let single = tiles(Rect::new(100, 200, 160, 90));
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].commit.w, MODEL_INPUT);
        assert_eq!(single[0].commit.h, MODEL_INPUT);

        // A 2×2 tiling: only the last column and last row keep their full width
        // and height, and every commit is inside its own crop.
        let quad = tiles(Rect::new(0, 0, 700, 620));
        assert_eq!(quad.len(), 4);
        assert_eq!(quad[0].commit.w, MODEL_INPUT - TILE_HANDOFF);
        assert_eq!(quad[0].commit.h, MODEL_INPUT - TILE_HANDOFF);
        assert_eq!(quad[3].commit.w, MODEL_INPUT);
        assert_eq!(quad[3].commit.h, MODEL_INPUT);
        for tile in &quad {
            assert_eq!(tile.commit.x, tile.crop.x);
            assert!(tile.commit.right() <= tile.crop.right());
            assert!(tile.commit.bottom() <= tile.crop.bottom());
        }
    }
}
