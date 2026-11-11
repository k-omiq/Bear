//! Nano Banana: a region edited by Google's Gemini image model, one crop at a time.
//!
//! The model answers only at the sizes on its chart: a tier (512, 1K, 2K) and an
//! aspect ratio name one exact output size. A crop of any other size comes back
//! resampled, and manga screentone does not survive a resample. So the crop is
//! cut **to a chart size**: the smallest one that holds the region and some
//! context. Then the answer maps one to one onto the page and nothing is scaled.
//!
//! Ported from freeditor's `genai/sendRegion.ts`, with two changes:
//!
//! - A region too big for the tier moves up a tier. freeditor sent a crop that
//!   cut through the selection.
//! - The hole keeps [`CONTEXT`] pixels of page around it, because a mask touching
//!   the crop's edge gives the model nothing to continue. A region too big for
//!   that on its page keeps less, down to none, rather than being refused.
//!
//! This module is geometry and pixels. The request itself is in
//! `src-tauri/src/inference/nano_banana.rs`, the only place that touches the
//! network. The boundary is ours, as on every model rung: the model redraws the
//! crop, and [`PreparedRender`] writes only through the applied mask.

use std::io::Cursor;

use crate::engines::model::{applied_mask, Error, Rendered};
use crate::engines::postprocess::Choice;
use crate::engines::render::{GeneratedCrop, Preprocessing, PreparedRender};
use crate::fit::Fitted;
use crate::image::Raster;
use crate::mask::{Mask, Rect};

/// Page kept around the hole, in pixels, on every side the page has, when the
/// page and the chart allow it.
///
/// Enough for the finishing. On 48 screentone holes over 7 real grey pages
/// (2026-11-03), an answer that kept the tone and lost the dots came out of the
/// default finishing 39.2 levels off the page with 24 px around the mask, 39.3
/// with 64 and 39.2 with 128. With none it was 47.1, no better than the answer.
pub const CONTEXT: u32 = 24;

/// The largest answer decoded. A 2K tier 8:1 answer is 6144 x 768, about 4.7 MP.
const MAX_ANSWER_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_ANSWER_SIDE: u32 = 8192;

/// How far an answer's shape may be from the output's before it is refused. A
/// wrong shape stretched onto the page moves every line it holds.
const SHAPE_TOLERANCE: f64 = 0.03;
/// How far an answer's size may be from the output's when the page was smaller
/// than the output and the answer is resampled anyway. A 1K answer to a 512
/// request is the model answering another size, not a near miss.
const MAX_ANSWER_SCALE: f64 = 1.1;

/// The tint that shows the person what will be edited, in the crop window:
/// freeditor's overlay colour. It is never sent. The model gets the plain crop
/// and a separate mask ([`Prepared::page_png`], [`Prepared::mask_png`]), so a
/// colour page with blue or violet art is never read as a tint the model kept.
const MARK_RGB: [f64; 3] = [45.0, 127.0, 249.0];
const MARK_ALPHA: f64 = 0.32;

/// How much the model is asked to return for one crop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Half,
    One,
    Two,
}

impl Tier {
    pub const ALL: [Tier; 3] = [Tier::Half, Tier::One, Tier::Two];

    /// `generationConfig.imageConfig.imageSize`. The API takes `512`, not
    /// freeditor's `512px`.
    pub fn wire(self) -> &'static str {
        match self {
            Tier::Half => "512",
            Tier::One => "1K",
            Tier::Two => "2K",
        }
    }
}

/// An output's width and height.
type Size = (u32, u32);

/// The published chart: ratio, then the output at 512, 1K and 2K.
///
/// 21:9 at 512 is left out. The chart says 792 x 168, which is not 21:9 (1K and
/// 2K are), so what the model really returns there is not known.
///
/// An answer must match its entry exactly when the crop is cut to it, so a
/// wrong entry costs a refused answer, never a resampled one.
const CHART: [(&str, Option<Size>, Size, Size); 14] = [
    ("1:1", Some((512, 512)), (1024, 1024), (2048, 2048)),
    ("1:4", Some((256, 1024)), (512, 2048), (1024, 4096)),
    ("1:8", Some((192, 1536)), (384, 3072), (768, 6144)),
    ("2:3", Some((424, 632)), (848, 1264), (1696, 2528)),
    ("3:2", Some((632, 424)), (1264, 848), (2528, 1696)),
    // The published chart says 448 x 600. The model answered 448 x 592 (2026-11-02).
    ("3:4", Some((448, 592)), (896, 1200), (1792, 2400)),
    ("4:1", Some((1024, 256)), (2048, 512), (4096, 1024)),
    // The published chart says 600 x 448. 592 x 448 mirrors the 3:4 answer above
    // and is inferred, not measured.
    ("4:3", Some((592, 448)), (1200, 896), (2400, 1792)),
    ("4:5", Some((464, 576)), (928, 1152), (1856, 2304)),
    ("5:4", Some((576, 464)), (1152, 928), (2304, 1856)),
    ("8:1", Some((1536, 192)), (3072, 384), (6144, 768)),
    ("9:16", Some((384, 688)), (768, 1376), (1536, 2752)),
    ("16:9", Some((688, 384)), (1376, 768), (2752, 1536)),
    ("21:9", None, (1584, 672), (3168, 1344)),
];

/// The ratios only Nano Banana 2.1 and 2 take. Nano Banana Pro does not.
const BANNER_RATIOS: [&str; 4] = ["1:4", "1:8", "4:1", "8:1"];

/// A model this app can send to, and what its chart holds (Google's image
/// generation page, read 2026-11-03). Only these ids are ever put in a request,
/// so a stored setting can not name another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Model {
    pub id: &'static str,
    /// The smallest tier it answers at. Only Nano Banana 2 answers at 512.
    lowest: Tier,
    /// Whether the 1:4, 1:8, 4:1 and 8:1 ratios are on its chart.
    banner_ratios: bool,
    /// Whether 2K costs what 1K does, so a smaller crop saves nothing.
    two_k_at_one_k_price: bool,
}

/// Nano Banana 2.1, the newest. No 512 tier.
pub const NANO_BANANA_2_1: Model =
    Model { id: "gemini-nano-banana-2.1", lowest: Tier::One, banner_ratios: true, two_k_at_one_k_price: false };
/// Nano Banana 2: the one model with the 512 tier, and the one the crops were benched on.
pub const NANO_BANANA_2: Model =
    Model { id: "gemini-3.1-flash-image", lowest: Tier::Half, banner_ratios: true, two_k_at_one_k_price: false };
/// Nano Banana Pro. No 512 tier, no banner ratios, and 2K at the 1K price.
pub const NANO_BANANA_PRO: Model =
    Model { id: "gemini-3-pro-image", lowest: Tier::One, banner_ratios: false, two_k_at_one_k_price: true };

impl Model {
    pub const ALL: [Model; 3] = [NANO_BANANA_2_1, NANO_BANANA_2, NANO_BANANA_PRO];
    pub const DEFAULT: Model = NANO_BANANA_2;

    /// A stored setting. An id that is not one of ours is the default, which also
    /// keeps anything but a known id out of the request's path.
    pub fn from_setting(value: Option<&str>) -> Model {
        Model::ALL.into_iter().find(|model| Some(model.id) == value).unwrap_or(Model::DEFAULT)
    }

    /// The tier a crop starts at. The size is never a setting: it is the smallest
    /// one on the model's chart that holds the region, so a region costs as little
    /// as it can and is never cut. Where 2K costs what 1K does, a 2K crop that
    /// needs no resample goes first ([`send_regions`]).
    pub fn smallest(self) -> Tier {
        self.lowest
    }
}

fn chart(model: Model, tier: Tier) -> impl Iterator<Item = (&'static str, (u32, u32))> {
    CHART
        .into_iter()
        .filter(move |(ratio, ..)| model.banner_ratios || !BANNER_RATIOS.contains(ratio))
        .filter_map(move |(ratio, half, one, two)| {
            Some((ratio, match tier {
                Tier::Half => half?,
                Tier::One => one,
                Tier::Two => two,
            }))
        })
}

/// What one request sends and asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendRegion {
    /// In page pixels, always inside the page.
    pub crop: Rect,
    /// The chart's output size. Equal to the crop's size unless the page itself
    /// is smaller than the output.
    pub output: (u32, u32),
    pub ratio: &'static str,
    pub tier: Tier,
}

impl SendRegion {
    /// A region as a patch's record keeps it (`crop`, `output`,
    /// `aspect_ratio`, `image_size`), so its kept answer can be finished
    /// again. `None` for a ratio or size the chart does not have.
    pub fn recorded(crop: Rect, output: (u32, u32), ratio: &str, tier: &str) -> Option<SendRegion> {
        let ratio = CHART.iter().find(|(name, ..)| *name == ratio)?.0;
        let tier = Tier::ALL.into_iter().find(|t| t.wire() == tier)?;
        Some(SendRegion { crop, output, ratio, tier })
    }

    /// Whether the answer maps one to one onto the page, with no resample.
    pub fn full_size(&self) -> bool {
        (self.crop.w, self.crop.h) == self.output
    }
}

/// The part of the page an output of this size covers: the output itself, or as
/// much of its shape as fits a smaller page.
fn footprint(output: (u32, u32), page_w: u32, page_h: u32) -> (u32, u32) {
    let (out_w, out_h) = (f64::from(output.0), f64::from(output.1));
    let scale = (f64::from(page_w) / out_w).min(f64::from(page_h) / out_h).min(1.0);
    (
        ((out_w * scale).round() as u32).clamp(1, page_w),
        ((out_h * scale).round() as u32).clamp(1, page_h),
    )
}

/// The crops that could carry a hole on a page, best first, from the first tier
/// at or above the model's smallest that has any.
///
/// The crops that fit the page at full chart size lead, since they need no
/// resampling, and among them 1:1, since it gives the most context on every
/// side. Then the ones shrunk to fit the page. Each group goes by how little page
/// it spends. The caller sends the first, and the others are there because
/// Google can refuse one crop of a region and accept another of the same region.
///
/// The hole keeps [`CONTEXT`] px of page where some tier allows it, and less
/// only where none does. Where 2K costs what 1K does, 2K goes first when it holds
/// the hole at full size, since it holds more page for the same price. Empty
/// means not even 2K holds the hole with no context.
pub fn send_regions(hole: Rect, page_w: u32, page_h: u32, model: Model) -> Vec<SendRegion> {
    plan(hole, page_w, page_h, model).map(|(_, regions)| regions).unwrap_or_default()
}

/// [`send_regions`], with the context the crops keep.
fn plan(hole: Rect, page_w: u32, page_h: u32, model: Model) -> Option<(u32, Vec<SendRegion>)> {
    (0..=CONTEXT).rev().find_map(|context| {
        let mut tiers = Tier::ALL
            .into_iter()
            .filter(|tier| *tier >= model.smallest())
            .map(|tier| crops_at(hole, page_w, page_h, model, tier, context))
            .filter(|crops| !crops.is_empty());
        let first = tiers.next()?;
        let full_size_two_k = |crops: &Vec<SendRegion>| crops[0].tier == Tier::Two && crops[0].full_size();
        if model.two_k_at_one_k_price && !full_size_two_k(&first) {
            if let Some(two_k) = tiers.find(full_size_two_k) {
                return Some((context, two_k));
            }
        }
        Some((context, first))
    })
}

/// The crops of one tier that hold `hole` with `context` px of page around it,
/// best first ([`send_regions`]).
fn crops_at(hole: Rect, page_w: u32, page_h: u32, model: Model, tier: Tier, context: u32) -> Vec<SendRegion> {
    if page_w == 0 || page_h == 0 || hole.w == 0 || hole.h == 0 {
        return Vec::new();
    }
    let context = i64::from(context);
    let left = hole.x.saturating_sub(context).max(0);
    let top = hole.y.saturating_sub(context).max(0);
    let right = hole.right().saturating_add(context).min(i64::from(page_w));
    let bottom = hole.bottom().saturating_add(context).min(i64::from(page_h));
    if right <= left || bottom <= top {
        return Vec::new();
    }
    let (need_w, need_h) = ((right - left) as u32, (bottom - top) as u32);
    let center_x = left + i64::from(need_w) / 2;
    let center_y = top + i64::from(need_h) / 2;
    let mut fits: Vec<SendRegion> = chart(model, tier)
        .filter_map(|(ratio, output)| {
            let (w, h) = footprint(output, page_w, page_h);
            if w < need_w || h < need_h {
                return None;
            }
            let x = (center_x - i64::from(w) / 2).clamp(0, i64::from(page_w - w));
            let y = (center_y - i64::from(h) / 2).clamp(0, i64::from(page_h - h));
            Some(SendRegion { crop: Rect::new(x, y, w, h), output, ratio, tier })
        })
        .collect();
    fits.sort_by_key(|region| (!region.full_size(), region.ratio != "1:1", u64::from(region.crop.w) * u64::from(region.crop.h)));
    fits
}

/// The smallest rectangle holding both.
pub fn union_rect(a: Rect, b: Rect) -> Rect {
    let (x, y) = (a.x.min(b.x), a.y.min(b.y));
    let (right, bottom) = (a.right().max(b.right()), a.bottom().max(b.bottom()));
    Rect::new(x, y, (right - x) as u32, (bottom - y) as u32)
}

/// One request's worth of a page: the regions it cleans and the crops it may go
/// out as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// Indices into the `others` the group was planned from: the regions that
    /// share the crop with the first one.
    pub members: Vec<usize>,
    /// Every crop that holds all of them, best first ([`send_regions`]).
    pub regions: Vec<SendRegion>,
}

/// The crop for `own` that holds as many of `others` as it can without growing.
///
/// The size and the context are the ones `own` needs alone, so asking for
/// company never costs more than asking for `own`. Then the nearest region goes
/// in if the crop that holds it as well is still the same size and keeps the
/// same context, and so on outward. Each region goes in
/// whole, so no crop ever cuts through one. `others` are the applied bounds of
/// regions on the same page. `None` means `own` itself fits no crop.
pub fn plan_group(own: Rect, others: &[Rect], page_w: u32, page_h: u32, model: Model) -> Option<Group> {
    let (context, alone) = plan(own, page_w, page_h, model)?;
    let tier = alone[0].tier;
    let page = Rect::new(0, 0, page_w, page_h);
    let centre = |rect: Rect| (rect.x * 2 + i64::from(rect.w), rect.y * 2 + i64::from(rect.h));
    let (own_x, own_y) = centre(own);
    let mut order: Vec<usize> = (0..others.len())
        .filter(|at| others[*at].w > 0 && others[*at].h > 0 && page.intersection(others[*at]) == others[*at])
        .collect();
    order.sort_by_key(|at| {
        let (x, y) = centre(others[*at]);
        ((x - own_x).pow(2) + (y - own_y).pow(2), *at)
    });
    let mut joined = own;
    let mut members = Vec::new();
    for at in order {
        let wider = union_rect(joined, others[at]);
        if !crops_at(wider, page_w, page_h, model, tier, context).is_empty() {
            joined = wider;
            members.push(at);
        }
    }
    members.sort_unstable();
    let regions = if members.is_empty() { alone } else { crops_at(joined, page_w, page_h, model, tier, context) };
    Some(Group { members, regions })
}

/// The best crop for a hole ([`send_regions`]).
pub fn send_region(hole: Rect, page_w: u32, page_h: u32, model: Model) -> Option<SendRegion> {
    send_regions(hole, page_w, page_h, model).into_iter().next()
}

/// The crops a region may be sent as with the regions that share each, or why it
/// cannot be sent at all. `others` are the applied masks of the page's other
/// regions, in this page's pixels.
pub fn group_for(page: &Raster, fitted: &Fitted, model: Model, others: &[Mask]) -> Result<Group, Error> {
    if let Some(decline) = crate::engines::render::declines(page, fitted) {
        return Err(decline.into());
    }
    let bounds = applied_mask(fitted, page.width, page.height).bounds;
    if bounds.w == 0 || bounds.h == 0 {
        return Err(Error::Run("the region is empty on this page".to_owned()));
    }
    let boxes: Vec<Rect> = others.iter().map(|mask| mask.bounds).collect();
    plan_group(bounds, &boxes, page.width, page.height, model)
        .ok_or_else(|| crate::engines::model::Decline::TooLarge { w: bounds.w, h: bounds.h }.into())
}

/// The crops a region may be sent as alone ([`group_for`]).
pub fn regions_for(page: &Raster, fitted: &Fitted, model: Model) -> Result<Vec<SendRegion>, Error> {
    Ok(group_for(page, fitted, model, &[])?.regions)
}

/// Why a Nano Banana answer could not be used.
#[derive(Debug, thiserror::Error)]
pub enum AnswerError {
    #[error("the answer image could not be read: {0}")]
    Unreadable(String),
    #[error("the answer is {got_w}x{got_h}, not the {want_w}x{want_h} that was asked for")]
    WrongShape { got_w: u32, got_h: u32, want_w: u32, want_h: u32 },
    #[error(transparent)]
    Render(#[from] Error),
}

/// One region, ready to send.
pub struct Prepared {
    render: PreparedRender,
    region: SendRegion,
    /// The crop with the mark painted, all of it, as RGB8: what the person sees.
    marked_rgb: Vec<u8>,
    marked_png: Vec<u8>,
    /// The crop as the page has it, white where restricted: the first image sent.
    page_png: Vec<u8>,
    /// White where the mark is, black everywhere else: the second image sent.
    mask_png: Vec<u8>,
    /// Per crop pixel, whether the mark was painted there.
    marks: Vec<bool>,
    /// The bounds of every marked region inside the crop, in page pixels.
    holes: Vec<Rect>,
    /// Per crop pixel, whether it is sent as the page ([`Prepared::restrict`]).
    /// `None` when the whole crop is.
    kept: Option<Vec<bool>>,
}

impl Prepared {
    /// Cut this crop and paint the mark. `finishing` is how the answer is matched
    /// to the page, as for this computer's other models. `companions` are the
    /// applied masks of the other regions that share the crop: they are marked
    /// for the model too, and nothing is written through them here.
    pub fn new(page: &Raster, fitted: &Fitted, region: SendRegion, finishing: Option<Choice>, companions: &[Mask])
        -> Result<Prepared, Error> {
        let mut render = PreparedRender::prepare_in_crop(page, fitted, region.crop, Preprocessing::V3)?;
        render.set_finishing(finishing);
        let (rgb, marks) = marked(&render, companions);
        let marked_png = encode_rgb_png(region.crop.w, region.crop.h, &rgb)?;
        let page_png = encode_rgb_png(region.crop.w, region.crop.h, render.image_rgb8())?;
        let mask_rgb: Vec<u8> = marks.iter().flat_map(|mark| [if *mark { 255 } else { 0 }; 3]).collect();
        let mask_png = encode_rgb_png(region.crop.w, region.crop.h, &mask_rgb)?;
        let crop = region.crop;
        let holes = std::iter::once(render.applied().bounds)
            .chain(companions.iter().map(|mask| mask.bounds))
            .map(|bounds| crop.intersection(bounds))
            .filter(|bounds| bounds.w > 0 && bounds.h > 0)
            .collect();
        Ok(Prepared { render, region, marked_rgb: rgb, marked_png, page_png, mask_png, marks, holes, kept: None })
    }

    pub fn region(&self) -> SendRegion {
        self.region
    }

    /// The bounds of every region marked in the crop, in page pixels: this
    /// one's applied mask and the companions'. A restricted crop always keeps
    /// them ([`Prepared::restrict`]).
    pub fn holes(&self) -> &[Rect] {
        &self.holes
    }

    /// The areas a restricted crop keeps when the person picks none: each
    /// hole with [`CONTEXT`] px of page around it, inside the crop.
    pub fn suggested(&self) -> Vec<Rect> {
        let crop = self.region.crop;
        let context = i64::from(CONTEXT);
        self.holes.iter().map(|hole| {
            let grown = Rect::new(hole.x - context, hole.y - context, hole.w + 2 * CONTEXT, hole.h + 2 * CONTEXT);
            crop.intersection(grown)
        }).collect()
    }

    /// Send only `areas` (page pixels) and the holes; every other pixel of
    /// the crop goes out plain white, so the model never sees that part of
    /// the page. The answer is read with the page put back there
    /// ([`Prepared::finish`]), so the finishing reads the page and not the
    /// white. Returns the areas kept, holes included,
    /// as the record keeps them.
    pub fn restrict(&mut self, areas: &[Rect]) -> Result<Vec<Rect>, Error> {
        let crop = self.region.crop;
        let mut kept_areas: Vec<Rect> = Vec::new();
        for area in areas.iter().chain(self.holes.iter()) {
            let area = crop.intersection(*area);
            if area.w > 0 && area.h > 0 && !kept_areas.contains(&area) {
                kept_areas.push(area);
            }
        }
        let (w, h) = (crop.w as usize, crop.h as usize);
        let mut kept = vec![false; w * h];
        for area in &kept_areas {
            for y in area.y..area.bottom() {
                let row = (y - crop.y) as usize * w;
                kept[row + (area.x - crop.x) as usize..row + (area.right() - crop.x) as usize].fill(true);
            }
        }
        // A mark is always sent, even a companion's pixel outside its bounds' clip.
        for (at, mark) in self.marks.iter().enumerate() {
            kept[at] |= *mark;
        }
        let (mut rgb, mut page) = (self.marked_rgb.clone(), self.render.image_rgb8().to_vec());
        for (at, keep) in kept.iter().enumerate() {
            if !keep {
                rgb[at * 3..at * 3 + 3].fill(255);
                page[at * 3..at * 3 + 3].fill(255);
            }
        }
        self.marked_png = encode_rgb_png(crop.w, crop.h, &rgb)?;
        self.page_png = encode_rgb_png(crop.w, crop.h, &page)?;
        self.kept = Some(kept);
        Ok(kept_areas)
    }

    /// The crop with the hole tinted blue, and white where it is restricted,
    /// as PNG: how the crop is shown to the person. It is never sent.
    pub fn marked_png(&self) -> &[u8] {
        &self.marked_png
    }

    /// The crop as the page has it, white where it is restricted, as PNG: the
    /// first of the two images sent, and with [`Prepared::mask_png`] all that
    /// leaves the machine.
    pub fn page_png(&self) -> &[u8] {
        &self.page_png
    }

    /// The mask the model edits inside, as PNG at the crop's size: white over
    /// this region and every companion, black everywhere else. The second
    /// image sent.
    pub fn mask_png(&self) -> &[u8] {
        &self.mask_png
    }

    /// The model's answer, written through the applied mask.
    pub fn finish(&self, answer: &[u8]) -> Result<Rendered, AnswerError> {
        let crop = self.region.crop;
        let mut rgb = decode_answer(answer, self.region)?;
        if let Some(kept) = &self.kept {
            // The model was shown white there: the page stands in for its answer.
            let page = self.render.image_rgb8();
            for (at, keep) in kept.iter().enumerate() {
                if !keep {
                    rgb[at * 3..at * 3 + 3].copy_from_slice(&page[at * 3..at * 3 + 3]);
                }
            }
        }
        Ok(self.render.composite(&GeneratedCrop::new(crop.w, crop.h, &rgb))?)
    }
}

/// The crop with the mark over every pixel the render may write, and over the
/// companions' masks, and where the mark is: the shown crop and the sent mask.
fn marked(render: &PreparedRender, companions: &[Mask]) -> (Vec<u8>, Vec<bool>) {
    let crop = render.crop();
    let mut out = render.image_rgb8().to_vec();
    let mut marks = vec![false; crop.w as usize * crop.h as usize];
    for y in 0..crop.h as usize {
        for x in 0..crop.w as usize {
            let (page_x, page_y) = (crop.x + x as i64, crop.y + y as i64);
            if render.alpha(page_x, page_y) > 0.0 || companions.iter().any(|mask| mask.contains(page_x, page_y)) {
                marks[y * crop.w as usize + x] = true;
                let at = (y * crop.w as usize + x) * 3;
                for channel in 0..3 {
                    let page = f64::from(out[at + channel]);
                    out[at + channel] = (page + (MARK_RGB[channel] - page) * MARK_ALPHA).round() as u8;
                }
            }
        }
    }
    (out, marks)
}

/// The answer as RGB8 at the crop's size.
///
/// A crop cut to its chart size takes only an answer of exactly that size,
/// copied as it came: any resample moves the screentone. A crop shrunk to a
/// smaller page takes an answer near the output's size and shape, resampled to
/// the crop.
fn decode_answer(answer: &[u8], region: SendRegion) -> Result<Vec<u8>, AnswerError> {
    let (want_w, want_h) = (region.crop.w, region.crop.h);
    let (out_w, out_h) = region.output;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_ANSWER_SIDE);
    limits.max_image_height = Some(MAX_ANSWER_SIDE);
    limits.max_alloc = Some(MAX_ANSWER_PIXELS * 4);
    let mut reader = image::ImageReader::new(Cursor::new(answer))
        .with_guessed_format()
        .map_err(|error| AnswerError::Unreadable(error.to_string()))?;
    reader.limits(limits);
    let rgb = reader.decode().map_err(|error| AnswerError::Unreadable(error.to_string()))?.into_rgb8();
    let (got_w, got_h) = rgb.dimensions();
    if (got_w, got_h) == (want_w, want_h) {
        return Ok(rgb.into_raw());
    }
    let wrong = AnswerError::WrongShape { got_w, got_h, want_w: out_w, want_h: out_h };
    if region.full_size() {
        return Err(wrong);
    }
    let shape = (f64::from(got_w) / f64::from(got_h)) / (f64::from(out_w) / f64::from(out_h));
    let scale = f64::from(got_w) / f64::from(out_w);
    if (shape - 1.0).abs() > SHAPE_TOLERANCE || !(1.0 / MAX_ANSWER_SCALE..=MAX_ANSWER_SCALE).contains(&scale) {
        return Err(wrong);
    }
    Ok(image::imageops::resize(&rgb, want_w, want_h, image::imageops::FilterType::Lanczos3).into_raw())
}

fn encode_rgb_png(width: u32, height: u32, rgb: &[u8]) -> Result<Vec<u8>, Error> {
    let failed = |error: png::EncodingError| Error::Run(format!("the crop could not be encoded: {error}"));
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    // The crop is read out in sRGB (`crop_samples`), and the answer is read
    // back as sRGB.
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    let mut writer = encoder.write_header().map_err(failed)?;
    writer.write_image_data(rgb).map_err(failed)?;
    writer.finish().map_err(failed)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BIG: u32 = 5000;

    fn sent(w: u32, h: u32) -> SendRegion {
        send_region(Rect::new(2000, 2000, w, h), BIG, BIG, Model::DEFAULT).unwrap()
    }

    #[test]
    fn every_chart_size_has_the_shape_it_is_named_for() {
        for model in Model::ALL {
            for tier in Tier::ALL.into_iter().filter(|tier| *tier >= model.lowest) {
                for (ratio, (w, h)) in chart(model, tier) {
                    let (a, b) = ratio.split_once(':').unwrap();
                    let named = a.parse::<f64>().unwrap() / b.parse::<f64>().unwrap();
                    let real = f64::from(w) / f64::from(h);
                    assert!((real / named - 1.0).abs() < 0.03, "{ratio} at {tier:?} is {w}x{h}");
                }
            }
        }
    }

    #[test]
    fn a_small_hole_gets_the_square_at_chart_size() {
        let region = sent(100, 100);
        assert_eq!((region.ratio, region.tier), ("1:1", Tier::Half));
        assert_eq!((region.crop.w, region.crop.h), (512, 512));
        assert_eq!(region.output, (512, 512));
    }

    #[test]
    fn a_wide_hole_gets_the_tightest_wide_crop_with_no_scaling() {
        let region = sent(900, 100);
        assert_eq!(region.ratio, "4:1");
        assert_eq!((region.crop.w, region.crop.h), (1024, 256));
        assert_eq!(region.output, (region.crop.w, region.crop.h));
    }

    #[test]
    fn a_tall_hole_gets_the_tightest_tall_crop() {
        let region = sent(100, 900);
        assert_eq!(region.ratio, "1:4");
        assert_eq!((region.crop.w, region.crop.h), (256, 1024));
    }

    #[test]
    fn a_hole_too_big_for_the_tier_moves_up_rather_than_being_cut() {
        let region = sent(800, 800);
        assert_eq!((region.ratio, region.tier), ("1:1", Tier::One));
        assert_eq!((region.crop.w, region.crop.h), (1024, 1024));
        let region = sent(1500, 1500);
        assert_eq!((region.ratio, region.tier), ("1:1", Tier::Two));
    }

    #[test]
    fn nothing_holds_a_hole_past_the_top_tier() {
        assert_eq!(send_region(Rect::new(0, 0, 4900, 4900), BIG, BIG, Model::DEFAULT), None);
    }

    #[test]
    fn the_crop_stays_on_the_page_and_holds_the_hole_with_context() {
        for (x, y) in [(0, 0), (4900, 4900), (0, 4900), (2000, 0)] {
            let hole = Rect::new(x, y, 100, 100);
            let SendRegion { crop, .. } = send_region(hole, BIG, BIG, Model::DEFAULT).unwrap();
            assert!(crop.x >= 0 && crop.y >= 0);
            assert!(crop.right() <= i64::from(BIG) && crop.bottom() <= i64::from(BIG));
            assert!(crop.x <= hole.x && crop.y <= hole.y);
            assert!(crop.right() >= hole.right() && crop.bottom() >= hole.bottom());
        }
        let SendRegion { crop, .. } = sent(100, 100);
        assert!(crop.x <= 2000 - i64::from(CONTEXT) && crop.right() >= 2100 + i64::from(CONTEXT));
    }

    #[test]
    fn a_page_smaller_than_the_chart_size_keeps_the_shape_and_the_output() {
        let region = send_region(Rect::new(100, 100, 50, 50), 400, 600, Model::DEFAULT).unwrap();
        assert_eq!(region.ratio, "1:1");
        assert_eq!((region.crop.w, region.crop.h), (400, 400));
        assert_eq!(region.output, (512, 512));
    }

    #[test]
    fn a_crop_shrunk_to_fit_the_page_is_tried_after_every_full_size_one() {
        let regions = send_regions(Rect::new(600, 700, 40, 20), 1200, 1600, Model::DEFAULT);
        assert_eq!(regions[1].ratio, "1:4");
        let shrunk = regions.iter().position(|region| region.ratio == "8:1").unwrap();
        assert_eq!((regions[shrunk].crop.w, regions[shrunk].crop.h), (1200, 150));
        assert!(regions[..shrunk].iter().all(|region| (region.crop.w, region.crop.h) == region.output));
    }

    #[test]
    fn every_other_crop_of_a_hole_holds_it_too_and_the_square_leads() {
        let hole = Rect::new(2000, 2000, 144, 444);
        let regions = send_regions(hole, BIG, BIG, Model::DEFAULT);
        assert_eq!(regions[0].ratio, "1:1");
        assert!(regions.len() > 3, "{regions:?}");
        for SendRegion { crop, tier, .. } in &regions {
            assert_eq!(*tier, Tier::Half);
            assert!(crop.x <= hole.x && crop.right() >= hole.right());
            assert!(crop.y <= hole.y && crop.bottom() >= hole.bottom());
        }
        assert!(regions.iter().any(|region| region.ratio == "1:4"));
        assert!(!regions.iter().any(|region| region.ratio == "4:1"), "too short for the hole");
    }

    #[test]
    fn a_model_without_the_512_tier_is_never_sent_one() {
        for model in [NANO_BANANA_2_1, NANO_BANANA_PRO] {
            let regions = send_regions(Rect::new(600, 700, 40, 20), 1200, 1600, model);
            assert!(!regions.is_empty());
            assert!(regions.iter().all(|region| region.tier == Tier::One), "{model:?}");
            assert_eq!(regions[0].output, (1024, 1024));
        }
        assert_eq!(NANO_BANANA_2.smallest(), Tier::Half);
        assert_eq!(NANO_BANANA_2_1.smallest(), Tier::One);
    }

    /// 2K costs Pro what 1K does, so it goes first where it holds the hole at
    /// full size: more page for the same price. Not where it would resample.
    #[test]
    fn pro_takes_2k_when_it_needs_no_resample() {
        let region = send_region(Rect::new(2000, 2000, 100, 100), BIG, BIG, NANO_BANANA_PRO).unwrap();
        assert_eq!((region.tier, region.ratio, region.output), (Tier::Two, "1:1", (2048, 2048)));
        assert!(region.full_size());
        let region = send_region(Rect::new(600, 700, 40, 20), 1200, 1600, NANO_BANANA_PRO).unwrap();
        assert_eq!((region.tier, region.output), (Tier::One, (1024, 1024)), "2K would be shrunk on this page");
        let region = send_region(Rect::new(2000, 2000, 100, 100), BIG, BIG, NANO_BANANA_2_1).unwrap();
        assert_eq!(region.tier, Tier::One, "2.1 still saves by going small");
    }

    /// A shrunk 1:1 is a resample, and a full-size crop of another shape is not.
    #[test]
    fn a_full_size_crop_of_any_shape_wins_over_a_shrunk_square() {
        // 2.1 has no 512 tier, and 1:1 at 1K is wider than the page.
        let region = send_region(Rect::new(450, 700, 100, 100), 1000, 1500, NANO_BANANA_2_1).unwrap();
        assert_eq!((region.ratio, region.crop.w, region.crop.h), ("9:16", 768, 1376), "the least page at full size");
        assert!(region.full_size());
        // A 500 px wide strip: 1:1 at 512 would be shrunk to 500.
        let regions = send_regions(Rect::new(200, 1400, 100, 100), 500, 3000, NANO_BANANA_2);
        assert_eq!((regions[0].ratio, regions[0].crop.w, regions[0].crop.h), ("1:4", 256, 1024));
        let square = regions.iter().position(|region| region.ratio == "1:1").unwrap();
        assert!(!regions[square].full_size());
        assert!(regions[..square].iter().all(SendRegion::full_size));
    }

    /// A region nearly as big as its page keeps less context rather than being
    /// refused.
    #[test]
    fn a_region_too_big_for_the_context_keeps_less_and_is_still_sent() {
        let hole = Rect::new(50, 50, 900, 1300);
        let region = send_region(hole, 1000, 1400, NANO_BANANA_2).unwrap();
        let SendRegion { crop, .. } = region;
        assert!(crop.x >= 0 && crop.y >= 0 && crop.right() <= 1000 && crop.bottom() <= 1400, "{crop:?}");
        assert!(crop.x <= hole.x && crop.y <= hole.y && crop.right() >= hole.right() && crop.bottom() >= hole.bottom());
        let (context, _) = plan(hole, 1000, 1400, NANO_BANANA_2).unwrap();
        assert!(context < CONTEXT, "{context}");
    }

    #[test]
    fn a_hole_partly_off_the_page_gets_a_crop_on_the_page() {
        let hole = Rect::new(-30, 100, 80, 50);
        let SendRegion { crop, .. } = send_region(hole, 600, 600, NANO_BANANA_2).unwrap();
        assert!(crop.x == 0 && crop.right() <= 600 && crop.y >= 0 && crop.bottom() <= 600, "{crop:?}");
        assert!(crop.right() >= hole.right() && crop.y <= hole.y && crop.bottom() >= hole.bottom());
        assert!(send_regions(Rect::new(-100, 100, 50, 50), 600, 600, NANO_BANANA_2).is_empty());
    }

    #[test]
    fn pro_is_never_sent_a_banner_ratio() {
        let hole = Rect::new(2000, 2000, 900, 100);
        let ratios = |model| send_regions(hole, BIG, BIG, model)
            .into_iter().map(|region| region.ratio).collect::<Vec<_>>();
        assert!(ratios(NANO_BANANA_2).contains(&"4:1"));
        assert!(ratios(NANO_BANANA_PRO).iter().all(|ratio| !BANNER_RATIOS.contains(ratio)));
        assert!(!ratios(NANO_BANANA_PRO).is_empty());
    }

    #[test]
    fn only_a_known_model_id_is_used() {
        assert_eq!(Model::from_setting(Some("gemini-nano-banana-2.1")), NANO_BANANA_2_1);
        assert_eq!(Model::from_setting(Some("gemini-3-pro-image")), NANO_BANANA_PRO);
        assert_eq!(Model::from_setting(Some("../../evil?x=")), Model::DEFAULT);
        assert_eq!(Model::from_setting(Some("gemini-2.5-flash-image")), Model::DEFAULT);
        assert_eq!(Model::from_setting(None), Model::DEFAULT);
    }

    fn at(x: i64, y: i64, w: u32, h: u32) -> Rect {
        Rect::new(x, y, w, h)
    }

    #[test]
    fn regions_near_each_other_share_one_crop_of_the_size_the_first_needs() {
        let own = at(2000, 2000, 100, 60);
        let near = at(2200, 2100, 80, 50);
        let group = plan_group(own, &[near], BIG, BIG, Model::DEFAULT).unwrap();
        assert_eq!(group.members, vec![0]);
        let alone = send_region(own, BIG, BIG, Model::DEFAULT).unwrap();
        for region in &group.regions {
            assert_eq!(region.tier, alone.tier, "company never raises the size");
            assert!(region.crop.x <= own.x && region.crop.right() >= own.right());
            assert!(region.crop.x <= near.x && region.crop.right() >= near.right());
            assert!(region.crop.y <= near.y && region.crop.bottom() >= near.bottom());
        }
    }

    #[test]
    fn a_region_too_far_to_share_the_crop_is_left_out_whole() {
        let own = at(2000, 2000, 100, 60);
        let far = at(2000, 3500, 100, 60);
        let group = plan_group(own, &[far], BIG, BIG, Model::DEFAULT).unwrap();
        assert!(group.members.is_empty());
        assert_eq!(group.regions, send_regions(own, BIG, BIG, Model::DEFAULT));
    }

    #[test]
    fn a_group_takes_as_many_as_fit_nearest_first_and_never_cuts_one() {
        let own = at(2000, 2000, 60, 40);
        let others = [at(2100, 2000, 40, 40), at(2200, 2000, 40, 40), at(2300, 2000, 40, 40), at(3900, 2000, 40, 40)];
        let group = plan_group(own, &others, BIG, BIG, Model::DEFAULT).unwrap();
        assert_eq!(group.members, vec![0, 1, 2]);
        for region in &group.regions {
            for member in &group.members {
                let rect = others[*member];
                assert!(region.crop.x <= rect.x && region.crop.right() >= rect.right(), "{region:?} cuts {rect:?}");
                assert!(region.crop.y <= rect.y && region.crop.bottom() >= rect.bottom());
            }
        }
    }

    #[test]
    fn a_region_off_the_page_is_not_a_companion() {
        let own = at(100, 100, 50, 50);
        let off = at(-20, 100, 50, 50);
        assert!(plan_group(own, &[off], 600, 600, Model::DEFAULT).unwrap().members.is_empty());
    }

    /// `others` should not name `own`, but if it does the plan is the same.
    #[test]
    fn own_among_the_others_changes_nothing() {
        let own = at(2000, 2000, 100, 60);
        let near = at(2200, 2100, 80, 50);
        let group = plan_group(own, &[own, near], BIG, BIG, Model::DEFAULT).unwrap();
        assert_eq!(group.members, vec![0, 1]);
        assert_eq!(group.regions, plan_group(own, &[near], BIG, BIG, Model::DEFAULT).unwrap().regions);
    }

    #[test]
    fn a_region_that_fits_nowhere_has_no_group() {
        assert_eq!(plan_group(at(0, 0, 4900, 4900), &[], BIG, BIG, Model::DEFAULT), None);
    }

    #[test]
    fn the_wire_names_are_the_api_values() {
        assert_eq!([Tier::Half.wire(), Tier::One.wire(), Tier::Two.wire()], ["512", "1K", "2K"]);
    }

    #[test]
    fn a_recorded_region_reads_back_only_with_a_chart_ratio_and_size() {
        let crop = Rect::new(3, 4, 512, 512);
        let region = SendRegion::recorded(crop, (512, 512), "1:1", "512").unwrap();
        assert_eq!((region.crop, region.output, region.ratio, region.tier), (crop, (512, 512), "1:1", Tier::Half));
        assert!(SendRegion::recorded(crop, (512, 512), "7:5", "512").is_none());
        assert!(SendRegion::recorded(crop, (512, 512), "1:1", "4K").is_none());
    }

    fn answer_png(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
        let data: Vec<u8> = rgb.iter().copied().cycle().take((width * height * 3) as usize).collect();
        encode_rgb_png(width, height, &data).unwrap()
    }

    /// A crop cut to its chart size, and one shrunk to a 400 px page.
    fn full(w: u32, h: u32) -> SendRegion {
        SendRegion { crop: Rect::new(0, 0, w, h), output: (w, h), ratio: "1:1", tier: Tier::Half }
    }
    fn shrunk() -> SendRegion {
        SendRegion { crop: Rect::new(0, 0, 400, 400), output: (512, 512), ratio: "1:1", tier: Tier::Half }
    }

    fn png_of(width: u32, height: u32, color: png::ColorType, depth: png::BitDepth, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(color);
        encoder.set_depth(depth);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(data).unwrap();
        writer.finish().unwrap();
        out
    }

    #[test]
    fn an_answer_of_the_wrong_shape_is_refused() {
        let error = decode_answer(&answer_png(512, 256, [255; 3]), full(512, 512)).unwrap_err();
        assert!(matches!(error, AnswerError::WrongShape { .. }), "{error}");
        let error = decode_answer(&answer_png(512, 256, [255; 3]), shrunk()).unwrap_err();
        assert!(matches!(error, AnswerError::WrongShape { .. }), "{error}");
    }

    /// A crop at chart size maps one to one onto the page, so an answer of any
    /// other size, even the right shape, is refused rather than resampled.
    #[test]
    fn a_full_size_crop_takes_only_an_answer_of_its_exact_size() {
        for (w, h) in [(1024, 1024), (520, 520), (512, 508)] {
            let error = decode_answer(&answer_png(w, h, [200; 3]), full(512, 512)).unwrap_err();
            assert!(matches!(error, AnswerError::WrongShape { got_w, got_h, want_w: 512, want_h: 512 } if (got_w, got_h) == (w, h)), "{error}");
        }
    }

    #[test]
    fn an_answer_at_crop_size_is_not_resampled() {
        let data: Vec<u8> = (0..16 * 16 * 3).map(|index| (index % 251) as u8).collect();
        let answer = encode_rgb_png(16, 16, &data).unwrap();
        assert_eq!(decode_answer(&answer, full(16, 16)).unwrap(), data);
    }

    #[test]
    fn an_answer_for_a_shrunk_crop_is_resized_to_the_crop() {
        // Black on the left half, white on the right.
        let data: Vec<u8> = (0..512 * 512).flat_map(|at| [if at % 512 < 256 { 0 } else { 255 }; 3]).collect();
        let rgb = decode_answer(&encode_rgb_png(512, 512, &data).unwrap(), shrunk()).unwrap();
        assert_eq!(rgb.len(), 400 * 400 * 3);
        for y in [0usize, 200, 399] {
            assert_eq!(&rgb[(y * 400 + 20) * 3..][..3], &[0, 0, 0]);
            assert_eq!(&rgb[(y * 400 + 380) * 3..][..3], &[255, 255, 255]);
        }
        // Near the asked size is a near miss; twice it is another size.
        assert!(decode_answer(&answer_png(540, 540, [9; 3]), shrunk()).is_ok());
        assert!(matches!(decode_answer(&answer_png(1024, 1024, [9; 3]), shrunk()), Err(AnswerError::WrongShape { .. })));
    }

    #[test]
    fn an_answer_with_alpha_keeps_its_colour() {
        let data: Vec<u8> = [10u8, 20, 30, 128].iter().copied().cycle().take(8 * 8 * 4).collect();
        let answer = png_of(8, 8, png::ColorType::Rgba, png::BitDepth::Eight, &data);
        let rgb = decode_answer(&answer, full(8, 8)).unwrap();
        assert!(rgb.chunks(3).all(|pixel| pixel == [10, 20, 30]), "{:?}", &rgb[..6]);
    }

    #[test]
    fn a_16_bit_answer_is_read_at_8_bits() {
        let data: Vec<u8> = [0xffu8, 0xff, 0x00, 0x00, 0x64, 0x64].iter().copied().cycle().take(8 * 8 * 6).collect();
        let answer = png_of(8, 8, png::ColorType::Rgb, png::BitDepth::Sixteen, &data);
        let rgb = decode_answer(&answer, full(8, 8)).unwrap();
        assert!(rgb.chunks(3).all(|pixel| pixel == [255, 0, 100]), "{:?}", &rgb[..6]);
    }

    #[test]
    fn a_jpeg_answer_is_read() {
        let mut answer = Vec::new();
        let gray = image::RgbImage::from_pixel(64, 64, image::Rgb([120, 120, 120]));
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut answer, 90)
            .encode_image(&gray)
            .unwrap();
        let rgb = decode_answer(&answer, full(64, 64)).unwrap();
        assert!(rgb.iter().all(|sample| sample.abs_diff(120) <= 2));
    }

    #[test]
    fn an_answer_past_the_largest_side_is_unreadable_and_never_decoded() {
        let answer = png_of(MAX_ANSWER_SIDE + 1, 1, png::ColorType::Grayscale, png::BitDepth::Eight, &vec![0; MAX_ANSWER_SIDE as usize + 1]);
        assert!(matches!(decode_answer(&answer, full(MAX_ANSWER_SIDE + 1, 1)), Err(AnswerError::Unreadable(_))));
    }

    #[test]
    fn garbage_is_unreadable_not_a_panic() {
        assert!(matches!(decode_answer(b"not an image", full(8, 8)), Err(AnswerError::Unreadable(_))));
    }

    #[test]
    fn the_sent_crop_says_it_is_srgb() {
        let page = gray_page(1000, 1000);
        let prepared = prepared_over(&page, Rect::new(400, 400, 60, 30));
        for sent in [prepared.page_png(), prepared.mask_png()] {
            let reader = png::Decoder::new(Cursor::new(sent)).read_info().unwrap();
            assert!(reader.info().srgb.is_some());
        }
    }

    fn gray_page(width: u32, height: u32) -> Raster {
        Raster {
            width,
            height,
            mode: crate::image::ColorMode::Gray,
            depth: crate::image::BitDepth::Eight,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None,
            color: Default::default(),
            data: (0..width * height).map(|at| (at % 200) as u8 + 40).collect(),
        }
    }

    fn fitted_over(page: &Raster, rect: Rect) -> Fitted {
        crate::fit::fit(page, &crate::mask::Mask::filled(rect), 1.0, 0.0, &crate::fit::EdgeMap::none(page.width, page.height), true)
    }

    /// A colour page: a warm gradient, with no blue in it.
    fn colour_page(width: u32, height: u32) -> Raster {
        Raster {
            mode: crate::image::ColorMode::Rgb,
            data: (0..width * height).flat_map(|at| {
                let (x, y) = (at % width, at / width);
                [(150 + x % 100) as u8, (90 + y % 80) as u8, 60]
            }).collect(),
            ..gray_page(width, height)
        }
    }

    fn prepared_over(page: &Raster, hole: Rect) -> Prepared {
        let fitted = fitted_over(page, hole);
        let region = regions_for(page, &fitted, Model::DEFAULT).unwrap()[0];
        Prepared::new(page, &fitted, region, None, &[]).unwrap()
    }

    #[test]
    fn the_crop_is_sent_plain_with_a_mask_and_shown_marked() {
        let page = gray_page(1000, 1000);
        let hole = Rect::new(400, 400, 60, 30);
        let prepared = prepared_over(&page, hole);
        let SendRegion { crop, ratio, .. } = prepared.region();
        assert_eq!((crop.w, crop.h, ratio), (512, 512, "1:1"));
        let (x, y) = ((hole.x + 30 - crop.x) as u32, (hole.y + 15 - crop.y) as u32);
        let sent = image::load_from_memory(prepared.page_png()).unwrap().into_rgb8();
        assert_eq!(sent.dimensions(), (512, 512));
        assert_eq!(sent.as_raw(), prepared.render.image_rgb8(), "the page goes with no mark on it");
        let mask = image::load_from_memory(prepared.mask_png()).unwrap().into_rgb8();
        assert_eq!(mask.dimensions(), (512, 512));
        assert_eq!(mask.get_pixel(x, y).0, [255; 3], "the hole is white");
        assert_eq!(mask.get_pixel(0, 0).0, [0; 3], "the rest is black");
        let shown = image::load_from_memory(prepared.marked_png()).unwrap().into_rgb8();
        let inside = shown.get_pixel(x, y);
        assert!(inside[2] > inside[0], "the person sees the hole marked: {inside:?}");
        let outside = shown.get_pixel(0, 0);
        assert_eq!(outside[0], outside[2], "{outside:?}");
    }

    #[test]
    fn a_companion_is_marked_for_the_model_but_not_written() {
        let page = gray_page(1000, 1000);
        let fitted = fitted_over(&page, Rect::new(400, 400, 60, 30));
        let region = regions_for(&page, &fitted, Model::DEFAULT).unwrap()[0];
        // Close enough that its box overlaps the own applied mask's box.
        let companion_box = Rect::new(462, 400, 40, 20);
        let companion = Mask::filled(companion_box);
        let prepared = Prepared::new(&page, &fitted, region, None, std::slice::from_ref(&companion)).unwrap();
        let mask = image::load_from_memory(prepared.mask_png()).unwrap().into_rgb8();
        let crop = region.crop;
        assert_eq!(mask.get_pixel((480 - crop.x) as u32, (410 - crop.y) as u32).0, [255; 3], "the companion is in the mask");
        let rendered = prepared.finish(&answer_png(512, 512, [128, 128, 128])).unwrap();
        let bounds = rendered.mask.bounds;
        assert!(bounds.intersection(companion_box).w > 0, "the test reaches the companion: {bounds:?}");
        let own = prepared.render.applied();
        for y in companion_box.y..companion_box.bottom() {
            for x in companion_box.x..companion_box.right() {
                if !own.contains(x, y) {
                    assert!(!rendered.mask.contains(x, y), "written over the companion at ({x}, {y})");
                }
            }
        }
        for y in 0..bounds.h {
            for x in 0..bounds.w {
                let (px, py) = (bounds.x + i64::from(x), bounds.y + i64::from(y));
                if !own.contains(px, py) {
                    assert_eq!(rendered.pixels.sample(x, y, 0), page.sample(px as u32, py as u32, 0), "({px}, {py})");
                }
            }
        }
    }

    #[test]
    fn the_answer_is_written_only_inside_the_applied_mask() {
        let page = gray_page(1000, 1000);
        let prepared = prepared_over(&page, Rect::new(400, 400, 60, 30));
        let rendered = prepared.finish(&answer_png(512, 512, [128, 128, 128])).unwrap();
        let bounds = rendered.mask.bounds;
        for y in 0..bounds.h {
            for x in 0..bounds.w {
                let (px, py) = (bounds.x + i64::from(x), bounds.y + i64::from(y));
                if !prepared.render.applied().contains(px, py) {
                    assert_eq!(rendered.pixels.sample(x, y, 0), page.sample(px as u32, py as u32, 0));
                }
            }
        }
    }

    #[test]
    fn a_restricted_crop_sends_white_outside_its_areas() {
        let page = gray_page(1000, 1000);
        let hole = Rect::new(400, 400, 60, 30);
        let mut prepared = prepared_over(&page, hole);
        let crop = prepared.region().crop;
        let applied = prepared.render.applied().bounds;
        assert_eq!(prepared.holes(), &[applied]);
        let suggested = prepared.suggested();
        assert_eq!(suggested, vec![Rect::new(applied.x - 24, applied.y - 24, applied.w + 48, applied.h + 48)]);
        let picked = Rect::new(crop.x, crop.y, 20, 20);
        let kept = prepared.restrict(&[picked]).unwrap();
        assert_eq!(kept, vec![picked, applied], "the hole is always kept");
        let page_at = |x: i64, y: i64| page.sample(x as u32, y as u32, 0) as u8;
        let read = |png: &[u8]| image::load_from_memory(png).unwrap().into_rgb8();
        let (sent, mask, shown) = (read(prepared.page_png()), read(prepared.mask_png()), read(prepared.marked_png()));
        let at = |image: &image::RgbImage, x: i64, y: i64| image.get_pixel((x - crop.x) as u32, (y - crop.y) as u32).0;
        for image in [&sent, &shown] {
            assert_eq!(at(image, crop.x + 5, crop.y + 5), [page_at(crop.x + 5, crop.y + 5); 3], "a picked area is the page");
            assert_eq!(at(image, crop.right() - 1, crop.bottom() - 1), [255; 3], "the rest is white");
            assert_eq!(at(image, applied.x - 10, applied.y), [255; 3], "past the hole is white when not picked");
        }
        assert_eq!(at(&sent, hole.x + 30, hole.y + 15), [page_at(hole.x + 30, hole.y + 15); 3], "the hole goes as the page");
        assert_eq!(at(&mask, hole.x + 30, hole.y + 15), [255; 3], "and is white in the mask");
        assert_eq!(at(&mask, crop.right() - 1, crop.bottom() - 1), [0; 3]);
        let mark = at(&shown, hole.x + 30, hole.y + 15);
        assert!(mark[2] > mark[0], "the hole is still shown marked: {mark:?}");
    }

    #[test]
    fn a_restricted_answer_reads_the_page_where_white_was_sent() {
        let page = gray_page(1000, 1000);
        let hole = Rect::new(400, 400, 60, 30);
        let mut whole = prepared_over(&page, hole);
        let mut restricted = prepared_over(&page, hole);
        let areas = restricted.suggested();
        let kept = restricted.restrict(&areas).unwrap();
        // One answer that is the page outside the kept areas, one that is black there.
        let crop = whole.region().crop;
        let page_rgb = whole.render.image_rgb8().to_vec();
        let (mut as_page, mut black) = (page_rgb.clone(), page_rgb);
        for y in 0..crop.h as i64 {
            for x in 0..crop.w as i64 {
                let at = (y * i64::from(crop.w) + x) as usize * 3;
                let inside = kept.iter().any(|area| area.contains(crop.x + x, crop.y + y));
                if inside {
                    as_page[at..at + 3].fill(128);
                    black[at..at + 3].fill(128);
                } else {
                    black[at..at + 3].fill(0);
                }
            }
        }
        let _ = whole.restrict(&[crop]);
        let expected = whole.finish(&encode_rgb_png(crop.w, crop.h, &as_page).unwrap()).unwrap();
        let got = restricted.finish(&encode_rgb_png(crop.w, crop.h, &black).unwrap()).unwrap();
        assert_eq!(got.mask.bounds, expected.mask.bounds);
        assert!(got.pixels.data == expected.pixels.data, "white sent outside the areas changes nothing written");
    }

    /// Blue and violet art in the answer is the art, not a kept mark: on
    /// 2026-11-10 a clean answer over dark violet hair (chapter 50, page 2) was
    /// refused as keeping the mark when one was still sent.
    #[test]
    fn a_blue_answer_on_a_colour_page_is_written() {
        let page = colour_page(1000, 1000);
        let prepared = prepared_over(&page, Rect::new(400, 400, 200, 200));
        assert!(prepared.finish(&answer_png(512, 512, [70, 40, 160])).is_ok());
        assert!(prepared.finish(prepared.marked_png()).is_ok(), "nothing in an answer is read as a mark");
    }

    #[test]
    fn an_empty_page_is_an_empty_region() {
        let page = gray_page(1000, 1000);
        let fitted = fitted_over(&page, Rect::new(400, 400, 60, 30));
        let empty = gray_page(0, 0);
        let error = group_for(&empty, &fitted, Model::DEFAULT, &[]).unwrap_err();
        assert!(matches!(error, Error::Run(_)), "{error}");
        let crop = Rect::new(0, 0, 512, 512);
        assert!(matches!(PreparedRender::prepare_in_crop(&empty, &fitted, crop, Preprocessing::V3), Err(Error::Run(_))));
    }

    #[test]
    fn a_crop_off_the_page_is_refused_before_it_is_cut() {
        let page = gray_page(1000, 1000);
        let fitted = fitted_over(&page, Rect::new(900, 400, 60, 30));
        let crop = Rect::new(600, 200, 512, 512);
        assert!(matches!(PreparedRender::prepare_in_crop(&page, &fitted, crop, Preprocessing::V3), Err(Error::Run(_))));
    }

    #[test]
    fn a_region_partly_off_the_page_is_cleaned_on_the_page() {
        let page = gray_page(600, 600);
        let fitted = fitted_over(&page, Rect::new(-30, 100, 80, 50));
        let region = regions_for(&page, &fitted, Model::DEFAULT).unwrap()[0];
        assert!(region.crop.x >= 0 && region.crop.right() <= 600);
        let prepared = Prepared::new(&page, &fitted, region, None, &[]).unwrap();
        let rendered = prepared.finish(&answer_png(region.crop.w, region.crop.h, [128; 3])).unwrap();
        let bounds = rendered.mask.bounds;
        assert!(bounds.x >= 0 && bounds.right() <= 600 && bounds.w > 0, "{bounds:?}");
    }
}
