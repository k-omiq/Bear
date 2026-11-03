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
//!   the crop's edge gives the model nothing to continue.
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

/// Page kept around the hole, in pixels, on every side the page has.
pub const CONTEXT: u32 = 24;

/// The largest answer decoded. A 2K tier 8:1 answer is 6144 x 768, about 4.7 MP.
const MAX_ANSWER_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_ANSWER_SIDE: u32 = 8192;

/// How far an answer's shape may be from the crop's before it is refused. A
/// wrong shape stretched onto the page moves every line it holds.
const SHAPE_TOLERANCE: f64 = 0.03;

/// The mark that shows the model what to edit: freeditor's overlay colour.
const MARK_RGB: [f64; 3] = [45.0, 127.0, 249.0];
const MARK_ALPHA: f64 = 0.32;

/// How much bluer the hole may read than the page around it. The mark adds about
/// 65 on white paper, so an answer past this kept the mark.
const MARK_KEPT_BIAS: f64 = 24.0;
/// The ring of page, outside the mask, that the hole is compared with.
const MARK_RING: i64 = 16;

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

/// The published chart: ratio, then the output at 512, 1K and 2K.
///
/// 21:9 at 512 is left out. The chart says 792 x 168, which is not 21:9 (1K and
/// 2K are), so what the model really returns there is not known.
const CHART: [(&str, Option<(u32, u32)>, (u32, u32), (u32, u32)); 14] = [
    ("1:1", Some((512, 512)), (1024, 1024), (2048, 2048)),
    ("1:4", Some((256, 1024)), (512, 2048), (1024, 4096)),
    ("1:8", Some((192, 1536)), (384, 3072), (768, 6144)),
    ("2:3", Some((424, 632)), (848, 1264), (1696, 2528)),
    ("3:2", Some((632, 424)), (1264, 848), (2528, 1696)),
    // The published chart says 448 x 600. The model answered 448 x 592 (2026-11-02).
    ("3:4", Some((448, 592)), (896, 1200), (1792, 2400)),
    ("4:1", Some((1024, 256)), (2048, 512), (4096, 1024)),
    ("4:3", Some((600, 448)), (1200, 896), (2400, 1792)),
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
}

/// Nano Banana 2.1, the newest. No 512 tier.
pub const NANO_BANANA_2_1: Model = Model { id: "gemini-nano-banana-2.1", lowest: Tier::One, banner_ratios: true };
/// Nano Banana 2: the one model with the 512 tier, and the one the crops were benched on.
pub const NANO_BANANA_2: Model = Model { id: "gemini-3.1-flash-image", lowest: Tier::Half, banner_ratios: true };
/// Nano Banana Pro. No 512 tier, and no banner ratios.
pub const NANO_BANANA_PRO: Model = Model { id: "gemini-3-pro-image", lowest: Tier::One, banner_ratios: false };

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
    /// as it can and is never cut.
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
/// 1:1 leads when it holds the hole, since it gives the most context on every
/// side. After it come the crops that fit the page at full chart size, since
/// they need no resampling, and then the ones shrunk to fit, each group by how
/// little page it spends. The caller sends the first, and the others are there
/// because Google can refuse one crop of a region and accept another of the same
/// region. Empty means not even 2K holds it.
pub fn send_regions(hole: Rect, page_w: u32, page_h: u32, model: Model) -> Vec<SendRegion> {
    send_regions_in(hole, page_w, page_h, model, model.smallest(), Tier::Two)
}

/// [`send_regions`] held to the tiers from `start` to `last`.
fn send_regions_in(hole: Rect, page_w: u32, page_h: u32, model: Model, start: Tier, last: Tier) -> Vec<SendRegion> {
    if page_w == 0 || page_h == 0 || hole.w == 0 || hole.h == 0 {
        return Vec::new();
    }
    let context = i64::from(CONTEXT);
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

    for tier in Tier::ALL.into_iter().filter(|tier| *tier >= start && *tier <= last) {
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
        fits.sort_by_key(|region| {
            let SendRegion { crop, output, ratio, .. } = *region;
            (ratio != "1:1", (crop.w, crop.h) != output, u64::from(crop.w) * u64::from(crop.h))
        });
        if !fits.is_empty() {
            return fits;
        }
    }
    Vec::new()
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
/// The size is the one `own` needs alone, so asking for company never costs more
/// than asking for `own`. Then the nearest region goes in if the crop that holds
/// it as well is still the same size, and so on outward. Each region goes in
/// whole, so no crop ever cuts through one. `others` are the applied bounds of
/// regions on the same page. `None` means `own` itself fits no crop.
pub fn plan_group(own: Rect, others: &[Rect], page_w: u32, page_h: u32, model: Model) -> Option<Group> {
    let alone = send_regions(own, page_w, page_h, model);
    let tier = alone.first()?.tier;
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
        if !send_regions_in(wider, page_w, page_h, model, tier, tier).is_empty() {
            joined = wider;
            members.push(at);
        }
    }
    members.sort_unstable();
    let regions = if members.is_empty() { alone } else { send_regions_in(joined, page_w, page_h, model, tier, tier) };
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
    #[error("the answer is {got_w}x{got_h}, too far from the shape of the {want_w}x{want_h} crop")]
    WrongShape { got_w: u32, got_h: u32, want_w: u32, want_h: u32 },
    #[error("the answer kept the blue mark")]
    KeptMark,
    #[error(transparent)]
    Render(#[from] Error),
}

/// One region, ready to send.
pub struct Prepared {
    render: PreparedRender,
    region: SendRegion,
    marked_png: Vec<u8>,
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
        let marked_png = encode_rgb_png(region.crop.w, region.crop.h, &marked(&render, companions))?;
        Ok(Prepared { render, region, marked_png })
    }

    pub fn region(&self) -> SendRegion {
        self.region
    }

    /// The crop with the hole painted blue, as PNG. This is all that leaves the
    /// machine.
    pub fn marked_png(&self) -> &[u8] {
        &self.marked_png
    }

    /// The model's answer, written through the applied mask.
    pub fn finish(&self, answer: &[u8]) -> Result<Rendered, AnswerError> {
        let crop = self.region.crop;
        let rgb = decode_answer(answer, crop.w, crop.h)?;
        if kept_mark(&self.render, &rgb) {
            return Err(AnswerError::KeptMark);
        }
        Ok(self.render.composite(&GeneratedCrop::new(crop.w, crop.h, &rgb))?)
    }
}

/// The crop with the mark over every pixel the render may write, and over the
/// companions' masks.
fn marked(render: &PreparedRender, companions: &[Mask]) -> Vec<u8> {
    let crop = render.crop();
    let mut out = render.image_rgb8().to_vec();
    for y in 0..crop.h as usize {
        for x in 0..crop.w as usize {
            let (page_x, page_y) = (crop.x + x as i64, crop.y + y as i64);
            if render.alpha(page_x, page_y) > 0.0 || companions.iter().any(|mask| mask.contains(page_x, page_y)) {
                let at = (y * crop.w as usize + x) * 3;
                for channel in 0..3 {
                    let page = f64::from(out[at + channel]);
                    out[at + channel] = (page + (MARK_RGB[channel] - page) * MARK_ALPHA).round() as u8;
                }
            }
        }
    }
    out
}

/// Whether the hole reads bluer than the page ringed around it, by more than a
/// clean answer could.
fn kept_mark(render: &PreparedRender, answer: &[u8]) -> bool {
    let (crop, applied) = (render.crop(), render.applied().bounds);
    let (mut hole_bias, mut hole_count) = (0.0, 0u64);
    let (mut ring_bias, mut ring_count) = (0.0, 0u64);
    for y in 0..crop.h as i64 {
        for x in 0..crop.w as i64 {
            let (px, py) = (crop.x + x, crop.y + y);
            let at = (y * i64::from(crop.w) + x) as usize * 3;
            if render.alpha(px, py) > 0.0 {
                hole_bias += f64::from(answer[at + 2]) - f64::from(answer[at]);
                hole_count += 1;
            } else if px >= applied.x - MARK_RING
                && px < applied.right() + MARK_RING
                && py >= applied.y - MARK_RING
                && py < applied.bottom() + MARK_RING
            {
                let page = render.image_rgb8();
                ring_bias += f64::from(page[at + 2]) - f64::from(page[at]);
                ring_count += 1;
            }
        }
    }
    if hole_count == 0 {
        return false;
    }
    let ring = if ring_count == 0 { 0.0 } else { ring_bias / ring_count as f64 };
    hole_bias / hole_count as f64 - ring > MARK_KEPT_BIAS
}

/// The answer as RGB8 at the crop's size. Equal sizes are copied as they came.
/// Only a page smaller than the chart size resamples.
fn decode_answer(answer: &[u8], want_w: u32, want_h: u32) -> Result<Vec<u8>, AnswerError> {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_ANSWER_SIDE);
    limits.max_image_height = Some(MAX_ANSWER_SIDE);
    limits.max_alloc = Some(MAX_ANSWER_PIXELS * 4);
    let unreadable = |detail: String| AnswerError::Unreadable(detail);
    let mut reader = image::ImageReader::new(Cursor::new(answer))
        .with_guessed_format()
        .map_err(|error| unreadable(error.to_string()))?;
    reader.limits(limits);
    let rgb = reader.decode().map_err(|error| unreadable(error.to_string()))?.into_rgb8();
    let (got_w, got_h) = rgb.dimensions();
    if got_w == want_w && got_h == want_h {
        return Ok(rgb.into_raw());
    }
    let got = f64::from(got_w) / f64::from(got_h);
    let want = f64::from(want_w) / f64::from(want_h);
    if (got / want - 1.0).abs() > SHAPE_TOLERANCE {
        return Err(AnswerError::WrongShape { got_w, got_h, want_w, want_h });
    }
    Ok(image::imageops::resize(&rgb, want_w, want_h, image::imageops::FilterType::Lanczos3).into_raw())
}

fn encode_rgb_png(width: u32, height: u32, rgb: &[u8]) -> Result<Vec<u8>, Error> {
    let failed = |error: png::EncodingError| Error::Run(format!("the crop could not be encoded: {error}"));
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
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
            let regions = send_regions(Rect::new(2000, 2000, 100, 100), BIG, BIG, model);
            assert!(!regions.is_empty());
            assert!(regions.iter().all(|region| region.tier == Tier::One), "{model:?}");
            assert_eq!(regions[0].output, (1024, 1024));
        }
        assert_eq!(NANO_BANANA_2.smallest(), Tier::Half);
        assert_eq!(NANO_BANANA_2_1.smallest(), Tier::One);
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

    #[test]
    fn a_region_that_fits_nowhere_has_no_group() {
        assert_eq!(plan_group(at(0, 0, 4900, 4900), &[], BIG, BIG, Model::DEFAULT), None);
    }

    #[test]
    fn the_wire_names_are_the_api_values() {
        assert_eq!([Tier::Half.wire(), Tier::One.wire(), Tier::Two.wire()], ["512", "1K", "2K"]);
    }

    fn answer_png(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
        let data: Vec<u8> = rgb.iter().copied().cycle().take((width * height * 3) as usize).collect();
        encode_rgb_png(width, height, &data).unwrap()
    }

    #[test]
    fn an_answer_of_the_wrong_shape_is_refused() {
        let error = decode_answer(&answer_png(512, 256, [255; 3]), 512, 512).unwrap_err();
        assert!(matches!(error, AnswerError::WrongShape { .. }), "{error}");
    }

    #[test]
    fn an_answer_at_crop_size_is_not_resampled() {
        let data: Vec<u8> = (0..16 * 16 * 3).map(|index| (index % 251) as u8).collect();
        let answer = encode_rgb_png(16, 16, &data).unwrap();
        assert_eq!(decode_answer(&answer, 16, 16).unwrap(), data);
    }

    #[test]
    fn an_answer_of_the_right_shape_is_resized_to_the_crop() {
        let rgb = decode_answer(&answer_png(512, 512, [9, 9, 9]), 400, 400).unwrap();
        assert_eq!(rgb.len(), 400 * 400 * 3);
    }

    #[test]
    fn garbage_is_unreadable_not_a_panic() {
        assert!(matches!(decode_answer(b"not an image", 8, 8), Err(AnswerError::Unreadable(_))));
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

    fn prepared_over(page: &Raster, hole: Rect) -> Prepared {
        let fitted = fitted_over(page, hole);
        let region = regions_for(page, &fitted, Model::DEFAULT).unwrap()[0];
        Prepared::new(page, &fitted, region, None, &[]).unwrap()
    }

    #[test]
    fn the_sent_crop_is_marked_inside_the_mask_and_untouched_outside() {
        let page = gray_page(1000, 1000);
        let hole = Rect::new(400, 400, 60, 30);
        let prepared = prepared_over(&page, hole);
        let SendRegion { crop, ratio, .. } = prepared.region();
        assert_eq!((crop.w, crop.h, ratio), (512, 512, "1:1"));
        let sent = image::load_from_memory(prepared.marked_png()).unwrap().into_rgb8();
        assert_eq!(sent.dimensions(), (512, 512));
        let inside = sent.get_pixel((hole.x + 30 - crop.x) as u32, (hole.y + 15 - crop.y) as u32);
        assert!(inside[2] > inside[0], "{inside:?}");
        let outside = sent.get_pixel(0, 0);
        assert_eq!(outside[0], outside[2], "{outside:?}");
    }

    #[test]
    fn a_companion_is_marked_for_the_model_but_not_written() {
        let page = gray_page(1000, 1000);
        let fitted = fitted_over(&page, Rect::new(400, 400, 60, 30));
        let region = regions_for(&page, &fitted, Model::DEFAULT).unwrap()[0];
        let companion = Mask::filled(Rect::new(500, 420, 40, 20));
        let prepared = Prepared::new(&page, &fitted, region, None, std::slice::from_ref(&companion)).unwrap();
        let sent = image::load_from_memory(prepared.marked_png()).unwrap().into_rgb8();
        let crop = region.crop;
        let mark = sent.get_pixel((520 - crop.x) as u32, (430 - crop.y) as u32);
        assert!(mark[2] > mark[0], "the companion is marked: {mark:?}");
        let rendered = prepared.finish(&answer_png(512, 512, [128, 128, 128])).unwrap();
        let bounds = rendered.mask.bounds;
        assert!(!bounds.contains(520, 430), "nothing is written over the companion: {bounds:?}");
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
    fn an_answer_that_kept_the_mark_is_refused_and_a_clean_one_is_not() {
        let page = gray_page(1000, 1000);
        let prepared = prepared_over(&page, Rect::new(400, 400, 200, 200));
        assert!(matches!(prepared.finish(&answer_png(512, 512, [150, 170, 230])), Err(AnswerError::KeptMark)));
        assert!(prepared.finish(&answer_png(512, 512, [170, 170, 170])).is_ok());
    }
}
