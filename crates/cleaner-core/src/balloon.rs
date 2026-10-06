//! What decides "in a balloon".
//!
//! The script gate is scoped by whether text sits inside a speech balloon:
//! inside, a confident verdict cleans or skips; outside, the gate does not
//! decide and the region goes to review. Revisions 1-3 read that off a
//! `comic_text_detector` class, and Phase 0 spike 6 found the class does
//! not exist - that model emits `eng`/`ja`.
//!
//! The three names those revisions used are real and belong here:
//! `ogkalu/comic-text-and-bubble-detector`, Apache-2.0, RT-DETR-v2 (so clear
//! of the YOLOv8 AGPL trap), `detector-v4-s_int8.onnx` at 11.1 MB, classes
//! `bubble` / `text_bubble` / `text_free`. It was set aside as a
//! *replacement* for the detector because it has no segmentation mask; that
//! objection does not apply to running it beside one purely for this question.
//!
//! It answers directly: a region overlapping a `text_bubble` box is in a
//! balloon, one overlapping `text_free` is not.
//!
//! ## The model is not the whole answer, and the page can be asked too
//!
//! One box at 0.35 confidence decides whether a region is gated or sent to
//! review as out of a balloon, and the misses are one-sided in the direction
//! that costs the most: a balloon the detector did not emit a box for reads as
//! `OutOfBalloon`, so dialogue in an ordinary bubble arrives in review with
//! *"text outside a speech bubble"* beside it. The other candidate
//! mechanism - the segmentation mask's own geometry - was set aside as a
//! heuristic that "fails on open-tail balloons and on white text over
//! black". Neither objection applies to it as a *second*
//! opinion beside the model, and the second of the two does not apply at all to
//! the test [`interior`] actually makes.
//!
//! That test is one sentence: **the paper immediately outside the text is a
//! balloon's interior when it is a uniform fill that an outline closes**,
//! walked outward ring by ring from the text's own box, with the text's
//! strokes excluded. Uniform, not white - a black balloon under white lettering
//! is as uniform as a white one, which is the *"white text over black"* half of
//! §3's objection answered by measuring spread rather than level. What the test
//! refuses is picture: screentone, hatching and art all break the uniformity in
//! the same way, by putting many separate runs of off-tone pixels on one ring.
//!
//! ## One answer, and the page may only add to it
//!
//! [`in_bubble`] is where the two opinions meet, and it is the only in/out
//! answer a run uses: the engine pick, the gate, the stored detection and a
//! held candidate's row all read it. Text is in a bubble when the detector says
//! so ([`Detected::inside`]) **or** the paper walk reads [`Interior::Solid`]
//! around the lettering. The page rescues text the detector left outside - chat
//! boxes, signs, square and diamond boxes, interface text on flat white, which
//! the detector labels `text_free` or emits no box for - and it never takes an
//! *inside* away. Measured over 1,098 text groups on three real chapters, the
//! page overruling a detector *inside* happened in 3 groups, which is too rare
//! to be worth a heuristic vetoing a model on the scans the heuristic is worst
//! at.
//!
//! The walk is strict about what counts as a band, because the page's own
//! false positive is a sound effect drawn with a white outline over art: that
//! outline is 3 to 8 pixels of white around the strokes, which is fill as far
//! as one ring can tell. So a band counts only when a thin outline closes it
//! ([`RingRead::is_outline`], at least [`MIN_FILL_DEPTH`] behind it) or when
//! nothing ends it across the whole walk. A band that runs into picture counts
//! for nothing however deep it was, and the paper between the strokes is not
//! read at all: the reading that used to do that took the white outline
//! between an effect's strokes for paper. On the same 1,098 groups this kept
//! 121 rescues (the chat boxes, signs and boxed text) and dropped the outlined
//! sound effects the looser walk had let in.
//!
//! ## Enclosure is the model answering the question too
//!
//! A balloon with a jagged or wobbly outline whose lettering nearly fills it
//! comes back as `bubble` at 0.86 to 0.96 with **no** `text_bubble` box at or
//! above [`SURE_SCORE`]. Before [`in_bubble`], the paper walk hit that outline on
//! its first ring, read [`Interior::Textured`], and was allowed to veto a
//! [`Detected::Bubble`]. Six regions across eighteen probe pages went to review
//! as *"text outside a speech bubble"* that way - 02.png 414,115 177×430 over
//! `Bubble@0.92`; 03.png 126,105 295×254 over `Bubble@0.92 Bubble@0.91` and
//! 779,982 197×402 over `Bubble@0.92 Bubble@0.86`; 04.png 786,923 101×378 over
//! `Bubble@0.69 TextInBubble@0.48`; 10.png 483,1018 262×365 over two
//! `Bubble@0.96`; and 15.png 819,559 157×383 over `Bubble@0.94 Bubble@0.95`.
//! Every one is ordinary dialogue in a white balloon. No veto exists any more,
//! so none of them can go outside that way; what the grade still decides is
//! whether the gate's reader may rescue a failed script verdict, which asks for
//! [`Detected::TextInBubble`] and nothing weaker.
//!
//! So [`detected`] grades sure `bubble` boxes that **cover** the region as
//! [`Detected::TextInBubble`]. Coverage rather than centre-containment is what
//! keeps this narrow: a large `bubble` box reaching across a sound effect holds
//! that effect's centre and not its extent, and `text_free` still wins outright
//! wherever the model drew it. What is left is the model saying *this text is
//! enclosed by a balloon*, which is the balloon question answered as
//! confidently as a `text_bubble` box answers it.
//!
//! Coverage is taken over the **union of the boxes**, and that is not a detail.
//! The detector emits one `bubble` box per lobe, and every one of the six cases
//! above is a two-lobed balloon: on 02.png the region is under two boxes that
//! only meet at y=319, and no single box covers a third of it. A rule reading
//! one box at a time answers *bubble* on all six.
//!
//! The tolerance the walk reads fill against is the band's own spread
//! ([`fill_band`]), floored at [`FILL_TOLERANCE`] and capped at
//! [`MAX_FILL_TOLERANCE`]. A fixed six levels read every white balloon on a
//! noisy scan as picture, and the page's flattest-decile noise could not stand
//! in for it because a scan's flattest tiles are saturated white.

use std::path::Path;

use crate::detect::Segmentation;
use crate::image::Raster;
use crate::mask::{Mask, Rect};

/// The model's fixed input side.
const INPUT: usize = 640;
/// Below this the box is not evidence of anything. RT-DETR emits a fixed 300
/// queries per image and most of them are noise.
const SCORE_THRESHOLD: f32 = 0.35;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BalloonClass {
    /// A balloon shape, whether or not text was found in it.
    Bubble,
    /// Text that is inside a balloon.
    TextInBubble,
    /// Text that is not.
    TextFree,
}

impl BalloonClass {
    pub(crate) fn from_label(label: i64) -> Option<BalloonClass> {
        match label {
            0 => Some(BalloonClass::Bubble),
            1 => Some(BalloonClass::TextInBubble),
            2 => Some(BalloonClass::TextFree),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BalloonBox {
    pub rect: Rect,
    pub class: BalloonClass,
    pub score: f32,
}

#[derive(Debug, thiserror::Error)]
pub enum BalloonError {
    #[error("the balloon detector could not be loaded: {0}")]
    Model(String),
    #[error("balloon detection failed: {0}")]
    Inference(String),
    #[error("the balloon detector's outputs are not the expected shape: {0}")]
    Contract(String),
}

pub struct BalloonDetector {
    session: ort::session::Session,
    selection: crate::accel::Selection,
    /// This session's row in [`crate::registry`]. See
    /// [`crate::detect::Detector`], which carries the same field for the same
    /// reason.
    lease: crate::registry::Lease,
}

impl BalloonDetector {
    /// This is the int8 model CoreML **cannot build a session for**
    /// ([`crate::accel::BALLOON`]), so `Automatic` keeps it on the CPU where it
    /// is fastest anyway. A user who forces CoreML gets the 8-second failure
    /// and then the CPU, reported rather than silent.
    pub fn open(model: &Path, preference: crate::accel::Preference) -> Result<BalloonDetector, BalloonError> {
        let (session, selection) =
            crate::accel::open_session(model, &crate::accel::BALLOON, preference, None)
                .map_err(|e| BalloonError::Model(e.to_string()))?;
        let lease = crate::registry::register_named(
            crate::registry::Kind::BalloonDetector,
            crate::registry::Footprint::weights(model),
            crate::registry::Device::accelerator(selection.accelerator),
            Some("Ogkalu comic text & bubble detector (Small)".into()),
        );
        Ok(BalloonDetector { session, selection, lease })
    }

    pub fn selection(&self) -> &crate::accel::Selection {
        &self.selection
    }

    /// Whether the owner should give this session back at its next safe point.
    /// [`crate::detect::Detector::spent`] states the trade.
    pub fn spent(&self) -> bool {
        self.lease.spent()
    }

    /// Give this session up for good, because it has failed in a way another
    /// run on it cannot survive - a reset adapter reported as device-removed.
    /// [`crate::registry::Lease::poison`] states what that costs and why it is
    /// not the same signal as an unload the user asked for.
    pub fn poison(&self) {
        self.lease.poison();
    }

    pub fn detect(&mut self, page: &Raster) -> Result<Vec<BalloonBox>, BalloonError> {
        self.lease.touch();
        // `preprocessor_config.json`: resize to 640×640, `do_rescale` with
        // 1/255, and `do_normalize: false` - the mean and std listed beside it
        // are not applied, which is easy to miss because they are there.
        let mut tensor = vec![0f32; 3 * INPUT * INPUT];
        let sx = page.width as f32 / INPUT as f32;
        let sy = page.height as f32 / INPUT as f32;
        for y in 0..INPUT {
            let py = (((y as f32 + 0.5) * sy - 0.5).clamp(0.0, (page.height - 1) as f32)) as u32;
            for x in 0..INPUT {
                let px = (((x as f32 + 0.5) * sx - 0.5).clamp(0.0, (page.width - 1) as f32)) as u32;
                let [r, g, b] = page.rgb8_pixel(px, py);
                tensor[y * INPUT + x] = r as f32 / 255.0;
                tensor[INPUT * INPUT + y * INPUT + x] = g as f32 / 255.0;
                tensor[2 * INPUT * INPUT + y * INPUT + x] = b as f32 / 255.0;
            }
        }

        let images = ort::value::Tensor::from_array(([1usize, 3, INPUT, INPUT], tensor))
            .map_err(|e| BalloonError::Inference(e.to_string()))?;
        // The model scales its own boxes back to this size, which is why the
        // page's own dimensions go in rather than the input's.
        //
        // **Width first.** Hugging Face's `RTDetrImageProcessor` documents
        // `orig_target_sizes` as `(height, width)`, and this export wants the
        // other order: with `(height, width)` every box comes back with its x
        // multiplied by `H/W` and its y by `W/H`, which on a 1600×2400 page is
        // 1.5 and 0.667 - a set of boxes that looks like plausible detections
        // in the wrong places rather than like an error.
        let sizes = ort::value::Tensor::from_array((
            [1usize, 2],
            vec![page.width as i64, page.height as i64],
        ))
        .map_err(|e| BalloonError::Inference(e.to_string()))?;

        let outputs = self
            .session
            .run(ort::inputs![images, sizes])
            .map_err(|e| BalloonError::Inference(e.to_string()))?;

        let (_, labels) = outputs["labels"]
            .try_extract_tensor::<i64>()
            .map_err(|e| BalloonError::Contract(e.to_string()))?;
        let (_, boxes) = outputs["boxes"]
            .try_extract_tensor::<f32>()
            .map_err(|e| BalloonError::Contract(e.to_string()))?;
        let (_, scores) = outputs["scores"]
            .try_extract_tensor::<f32>()
            .map_err(|e| BalloonError::Contract(e.to_string()))?;

        let mut found = Vec::new();
        for i in 0..labels.len().min(scores.len()) {
            if scores[i] < SCORE_THRESHOLD {
                continue;
            }
            let Some(class) = BalloonClass::from_label(labels[i]) else {
                continue;
            };
            let (x1, y1, x2, y2) = (boxes[i * 4], boxes[i * 4 + 1], boxes[i * 4 + 2], boxes[i * 4 + 3]);
            let x = x1.round().clamp(0.0, page.width as f32) as i64;
            let y = y1.round().clamp(0.0, page.height as f32) as i64;
            let right = x2.round().clamp(0.0, page.width as f32) as i64;
            let bottom = y2.round().clamp(0.0, page.height as f32) as i64;
            if right <= x || bottom <= y {
                continue;
            }
            found.push(BalloonBox {
                rect: Rect::new(x, y, (right - x) as u32, (bottom - y) as u32),
                class,
                score: scores[i],
            });
        }

        // Sorted so two runs list them the same way.
        found.sort_by(|a, b| {
            a.rect.y.cmp(&b.rect.y).then(a.rect.x.cmp(&b.rect.x)).then(b.score.total_cmp(&a.score))
        });
        Ok(found)
    }
}

/// A `text_bubble` box at or above this score is the model answering the
/// balloon question about *this text*, confidently: [`Detected::TextInBubble`],
/// the grade the gate's reader asks for before it may rescue a failed script
/// verdict. Above [`SCORE_THRESHOLD`] rather than equal to it: a box that only
/// just cleared emission is evidence, not a verdict. Measured on real scans,
/// when the paper reading could still veto the detector: every `text_bubble`
/// box it wrongly vetoed scored 0.88 to 0.94, and the one it rightly left alone
/// scored 0.38 beside a `text_free` at 0.74.
const SURE_SCORE: f32 = 0.5;

/// What the balloon detector said about one region, graded by how much of the
/// question it actually answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detected {
    /// The model placed this text in a balloon, confidently, and no `text_free`
    /// box contains the region's centre. Two boxes say that:
    ///
    /// - a `text_bubble` box at or above [`SURE_SCORE`] containing the centre  - 
    ///   the model asked "is this text in a balloon?" and answering yes;
    /// - `bubble` boxes at or above [`SURE_SCORE`], one of them containing the
    ///   centre, together covering [`ENCLOSED_SHARE_PERCENT`] of the region  - 
    ///   the model saying this text is enclosed by a balloon, which is the same
    ///   answer arrived at from the other side.
    ///
    /// The second is not a softening of the first. Covering the whole rectangle
    /// is a much stronger claim than holding its centre, and it is the claim
    /// `text_free` cannot also be making: a caption over art has no balloon drawn
    /// around all of it. What it buys is the balloon whose text nearly fills it,
    /// where the model emits `bubble` at 0.86–0.96 and no `text_bubble` at all  - 
    /// see the module note.
    TextInBubble,
    /// A `bubble` shape contains the centre but does not cover the region, or is
    /// too weak to be sure of; or a `text_bubble` box too weak to be sure of
    /// contains the centre. A statement about a shape near the text rather than
    /// about the text: still *inside* for [`in_bubble`], and not confident enough
    /// for the gate's reader.
    Bubble,
    /// A `text_free` box contains the centre, or nothing does. The one grade
    /// the paper reading may overrule, and only towards *inside*.
    Outside,
}

impl Detected {
    pub fn inside(self) -> bool {
        !matches!(self, Detected::Outside)
    }
}

/// What the balloon detector said about a text region.
///
/// The test is containment of the region's **centre**, not overlap: a balloon
/// box and a text box that merely touch are two different things a page corner
/// away, and a large `bubble` box overlapping the edge of an out-of-balloon
/// sound effect would otherwise pull it in.
///
/// `text_free` wins over everything else when both contain the centre, because
/// `text_free` is a statement about *this text* and `bubble` is a statement
/// about a shape that happens to be behind it.
///
/// **The one place a `bubble` box is more than that**: when the sure `bubble`
/// boxes cover the region's whole rectangle - one of them holding its centre  - 
/// they grade [`Detected::TextInBubble`] rather than [`Detected::Bubble`].
/// Enclosure is not proximity: a shape the text sits wholly inside is a shape
/// the text is *in*, and the balloons this rescues are the ones the paper
/// reading is worst at.
pub fn detected(region: Rect, balloons: &[BalloonBox]) -> Detected {
    let cx = region.x + region.w as i64 / 2;
    let cy = region.y + region.h as i64 / 2;

    let mut answer = Detected::Outside;
    let mut sure_shape_over_centre = false;
    for balloon in balloons {
        if !balloon.rect.contains(cx, cy) {
            continue;
        }
        match balloon.class {
            BalloonClass::TextFree => return Detected::Outside,
            BalloonClass::TextInBubble if balloon.score >= SURE_SCORE => {
                answer = Detected::TextInBubble;
            }
            BalloonClass::Bubble if balloon.score >= SURE_SCORE => {
                sure_shape_over_centre = true;
                if answer == Detected::Outside {
                    answer = Detected::Bubble;
                }
            }
            BalloonClass::TextInBubble | BalloonClass::Bubble => {
                if answer == Detected::Outside {
                    answer = Detected::Bubble;
                }
            }
        }
    }

    if answer == Detected::Bubble && sure_shape_over_centre {
        // The union is one balloon's lobes, not every sure shape on the page:
        // a shape counts only if it touches or overlaps one that holds the
        // centre. Lobes of one balloon meet (02.png's two boxes share a row);
        // a neighbouring balloon whose box merely reaches across this region
        // does not, and must not lend its area to the share.
        let sure: Vec<Rect> = balloons
            .iter()
            .filter(|b| b.class == BalloonClass::Bubble && b.score >= SURE_SCORE)
            .map(|b| b.rect)
            .collect();
        let holders: Vec<Rect> = sure.iter().copied().filter(|r| r.contains(cx, cy)).collect();
        let shapes: Vec<Rect> = sure
            .into_iter()
            .filter(|r| holders.iter().any(|h| touches(*h, *r)))
            .collect();
        if covered_percent(region, &shapes) >= ENCLOSED_SHARE_PERCENT {
            answer = Detected::TextInBubble;
        }
    }
    answer
}

/// How much of a region the sure `bubble` shapes must cover before the model is
/// read as having enclosed it, in percent.
///
/// Not 100, and the reason is a shape rather than a tolerance: the balloon
/// detector emits one `bubble` box per **lobe**, and the balloons this rule
/// exists for are the two-lobed ones a letterer draws for a long line. Two
/// overlapping boxes over one balloon leave the concave notches where the lobes
/// meet uncovered, and the region's own rectangle - the lettering's box grown by
/// the seven pixels [`crate::detect`] adds - reaches into them. Measured over
/// twelve probe pages: the four balloons wrongly sent to review covered 0.79,
/// 0.87, 0.93 and 0.97 of their regions, and the two regions that must stay
/// outside covered 0.00 and 0.34. Three quarters is the middle of that gap, and
/// it is also below every one-box case, which covers all of it.
const ENCLOSED_SHARE_PERCENT: i64 = 75;

/// What share of `region`, in percent, the union of `rects` covers.
///
/// The union, not the sum: two `bubble` boxes over one two-lobed balloon overlap
/// heavily, and adding their areas would call a third of a region a whole one.
/// Coordinate compression rather than a raster - a page holds tens of balloon
/// boxes, so the grid is small and the answer is exact.
/// Whether two boxes overlap or share an edge. Inclusive on purpose: the two
/// lobes of a balloon drawn as two shapes can meet at exactly one row.
fn touches(a: Rect, b: Rect) -> bool {
    a.x <= b.right() && b.x <= a.right() && a.y <= b.bottom() && b.y <= a.bottom()
}

fn covered_percent(region: Rect, rects: &[Rect]) -> i64 {
    let area = region.w as i64 * region.h as i64;
    if area == 0 {
        return 0;
    }
    let clipped: Vec<Rect> = rects
        .iter()
        .filter_map(|r| {
            let x = r.x.max(region.x);
            let y = r.y.max(region.y);
            let right = r.right().min(region.right());
            let bottom = r.bottom().min(region.bottom());
            (right > x && bottom > y)
                .then(|| Rect { x, y, w: (right - x) as u32, h: (bottom - y) as u32 })
        })
        .collect();
    if clipped.is_empty() {
        return 0;
    }
    let mut xs: Vec<i64> = clipped.iter().flat_map(|r| [r.x, r.right()]).collect();
    let mut ys: Vec<i64> = clipped.iter().flat_map(|r| [r.y, r.bottom()]).collect();
    xs.sort_unstable();
    xs.dedup();
    ys.sort_unstable();
    ys.dedup();

    let mut covered = 0i64;
    for x in xs.windows(2) {
        for y in ys.windows(2) {
            let inside = clipped.iter().any(|r| {
                r.x <= x[0] && r.right() >= x[1] && r.y <= y[0] && r.bottom() >= y[1]
            });
            if inside {
                covered += (x[1] - x[0]) * (y[1] - y[0]);
            }
        }
    }
    covered * 100 / area
}

/// Whether text sits in a bubble: the one in/out answer a run uses, for the
/// engine pick, the gate, the stored detection and a held candidate alike.
///
/// The detector's answer **or** the page's: [`detected`] at `masking` saying
/// anything but [`Detected::Outside`], or [`interior_of`] reading
/// [`Interior::Solid`] around `text_bounds`. Paper can only rescue; it never
/// vetoes the detector's *inside* (see the module note for the measurement),
/// so the walk runs only when the detector said *outside*.
///
/// `text_bounds` is the lettering's own box, which the walk reads around, and
/// `masking` the box the detector's answer is read at - the two a
/// [`crate::detect::Region`] carries as `text_bounds()` and `masking`, and
/// [`crate::detect::Region::text_bounds`] says why the walk must not start from
/// the wider one. `page` and `seg` share `text_bounds`'
/// coordinates; `balloons` share `masking`'s.
pub fn in_bubble(
    page: &Raster,
    seg: &Segmentation,
    text_bounds: Rect,
    masking: Rect,
    balloons: &[BalloonBox],
) -> bool {
    detected(masking, balloons).inside()
        || matches!(interior_of(page, seg, text_bounds), Interior::Solid { .. })
}

/// The smaller luma population inside `bounds` by Otsu's threshold, or `None`
/// when there is no visible ink population: too little contrast, too few
/// pixels, or a population too even to be text. The broad bounds admit dense
/// Hangul and Han while refusing one-pixel noise and near-even texture.
///
/// The bounded ink estimate [`crate::text_groups`] gives a text box no mask
/// pixel covers. The companion detector finds Korean and Chinese lettering the
/// primary one can miss, but has no mask output; without this such a box has
/// nothing for the gate to split into lines or for the fit to seed from.
pub(crate) fn otsu_ink(page: &Raster, bounds: Rect) -> Option<Mask> {
    if bounds.w == 0 || bounds.h == 0 {
        return None;
    }
    let mut histogram = [0u32; 256];
    for y in bounds.y..bounds.bottom() {
        for x in bounds.x..bounds.right() {
            let level = (page.luma16_at(x as u32, y as u32) >> 8) as usize;
            histogram[level] += 1;
        }
    }
    let threshold = otsu_threshold(&histogram)?;
    let low: u32 = histogram[..=threshold].iter().sum();
    let high: u32 = histogram[threshold + 1..].iter().sum();
    let ink_is_low = low <= high;
    let ink = low.min(high);
    let total = low + high;
    // Text must be a visible population, but cannot occupy most of its own
    // detector box.
    if ink < 4 || ink * 100 < total || ink * 100 > total * 45 {
        return None;
    }
    let mut mask = Mask::empty(bounds);
    for y in bounds.y..bounds.bottom() {
        for x in bounds.x..bounds.right() {
            let level = (page.luma16_at(x as u32, y as u32) >> 8) as usize;
            let is_ink = if ink_is_low { level <= threshold } else { level > threshold };
            if is_ink {
                mask.set(x, y, true);
            }
        }
    }
    Some(mask)
}

fn otsu_threshold(histogram: &[u32; 256]) -> Option<usize> {
    let total: u64 = histogram.iter().map(|&n| n as u64).sum();
    let first = histogram.iter().position(|&n| n != 0)?;
    let last = histogram.iter().rposition(|&n| n != 0)?;
    if last.saturating_sub(first) < 12 {
        return None;
    }
    let weighted: u64 = histogram
        .iter()
        .enumerate()
        .map(|(level, &n)| level as u64 * n as u64)
        .sum();
    let mut below = 0u64;
    let mut below_weighted = 0u64;
    let mut best = None;
    let mut best_variance = -1.0f64;
    for (level, &count) in histogram.iter().enumerate().take(255) {
        below += count as u64;
        below_weighted += level as u64 * count as u64;
        let above = total - below;
        if below == 0 || above == 0 {
            continue;
        }
        let delta = below_weighted as f64 / below as f64
            - (weighted - below_weighted) as f64 / above as f64;
        let variance = below as f64 * above as f64 * delta * delta;
        if variance > best_variance {
            best_variance = variance;
            best = Some(level);
        }
    }
    best
}

/// The narrowest tolerance a luma may sit from the band's own fill and still be
/// that fill, in 16-bit luma. Six 8-bit levels: wider than a clean scan's noise
/// floor - the pages [`crate::fit::ring`] is calibrated against sit at a few
/// levels - and far under the contrast of anything that is not the fill. An
/// anti-aliased glyph edge crosses it, which is why the text mask is dilated
/// before it is removed rather than taken as the model drew it.
///
/// **A floor, not the tolerance.** It was the tolerance, and on a noisy scan
/// that read every balloon as picture: white paper with a spread of 7–10
/// levels puts a fifth of a ring past six, in dozens of separate runs, which is
/// exactly what [`RingRead::is_outline`] refuses and [`Interior::Textured`]
/// means. Measured on real pages, the page's own flattest-decile noise
/// ([`crate::fit::page_noise_sigma`]) was **0** on three of the six - their
/// flattest tiles are saturated white - so the floor cannot be scaled from
/// there either. [`fill_band`] measures the spread of the innermost rings
/// instead, and widens the tolerance to it up to [`MAX_FILL_TOLERANCE`].
const FILL_TOLERANCE: u16 = 6 * 257;

/// The widest tolerance [`fill_band`] will grant, in 16-bit luma. Twenty 8-bit
/// levels: above the spread of any scan that is still paper, and below the
/// contrast of ink, a halftone dot, or hatching against the paper they sit on.
/// Without a cap a screentone's own spread would be measured as its tolerance
/// and every ring of tone would read as fill.
const MAX_FILL_TOLERANCE: u16 = 20 * 257;

/// The tolerance is this share of the innermost rings' deviations, in percent,
/// scaled by [`TOLERANCE_MARGIN`]. The same share [`FILL_SHARE_PERCENT`] asks a
/// ring to keep within it, so that the rings the tolerance was read from would
/// pass their own test; the margin is what keeps the rings *beyond* them, with
/// the same noise, from failing it by a coin toss.
const TOLERANCE_PERCENTILE: usize = FILL_SHARE_PERCENT;

/// Numerator over denominator: three halves.
const TOLERANCE_MARGIN: (u32, u32) = (3, 2);

/// The text mask is grown by this before its pixels are dropped from the band.
/// [`crate::fit::ring`]'s reason, one ring further in: an un-grown contour
/// "lands on the anti-aliasing fringe and biases the median 5–20 levels dark",
/// and here a biased pixel is not a shifted statistic but a fill that is not
/// the fill.
const TEXT_HALO: u32 = 2;

/// Below this many readable pixels a ring is not a ring. A 20×20 box's first
/// ring is 84 pixels before anything is dropped from it, so this is reached
/// only by a box at a page edge or one whose surround is nearly all text.
const MIN_RING_SAMPLES: usize = 16;

/// And below this share of the ring's own on-page pixels, in percent, what is
/// left of it is not a sample of it. The rings nearest a masking box are mostly
/// glyph - the box is drawn around the lettering, so its first two rings are
/// the lettering's own halo - and a dozen surviving pixels in a corner will
/// read as whatever that corner happens to be. Measured against the *on-page*
/// perimeter rather than the whole one, because a region at a page edge has a
/// short ring honestly.
const MIN_RING_COVERAGE_PERCENT: usize = 50;

/// A ring's off-fill pixels may form at most this many separate runs and still
/// be read as an outline. A rim crossing one of [`ring_sides`]' four segments
/// enters and leaves it, so a balloon whose rim dips inside the walk on every
/// side is eight runs; an open tail adds one more, and a scan's own speckle adds
/// a few. A halftone puts one run on every dot it crosses, which on any
/// realistic segment is tens.
const MAX_OUTLINE_RUNS: usize = 10;

/// A ring is the fill when this share of it is within [`FILL_TOLERANCE`], in
/// percent. Not 100: a scan speckles, and a ring that crosses a balloon's
/// interior antialiasing is still that interior.
const FILL_SHARE_PERCENT: usize = 92;

/// Of the pixels that are *not* the fill, this share must fall on one side of
/// it - all darker, or all lighter - for the ring to read as an outline rather
/// than as picture. Ink is one-sided; art is not.
const ONE_SIDED_PERCENT: usize = 95;

/// Each of [`ring_sides`]' segments is its side less one of these fractions off
/// each end - a quarter each way, so the middle half. Four rather than three or
/// six because half a side is the largest span whose ends stay inside every
/// balloon whose text box fits it: on an ellipse circumscribing the box, a
/// point a quarter-side off the axis is at 0.87 of the rim's distance where the
/// corner itself is at 1.0.
const SIDE_INSET_QUARTERS: u32 = 4;

/// The shallowest band of fill that means anything at all. Under three pixels
/// the reading is the box's own growth margin rather than a balloon.
const MIN_FILL_DEPTH: u32 = 3;

/// The deepest band worth asking for. A balloon's padding around its text is
/// tens of pixels at most, and asking for more of it only loses the tight ones.
const MAX_FILL_DEPTH: u32 = 8;

/// How the paper immediately outside a text box reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interior {
    /// A band of uniform fill that is a balloon's interior: closed by a thin
    /// outline at least [`MIN_FILL_DEPTH`] out, or running unbroken across the
    /// whole walk. `level` is that fill in 16-bit luma - the *page's* tone,
    /// never snapped to white, for the same reason [`crate::fit::ring`] refuses
    /// to snap one - and `depth` is how many native pixels of it were walked:
    /// up to the outline, or to [`scan_depth`] when nothing ended it.
    Solid { level: u16, depth: u32 },
    /// Picture: screentone, hatching, or art, less than [`MIN_FILL_DEPTH`] out.
    /// Not a balloon's interior.
    Textured,
    /// Not enough paper to say, or a band the walk will not count. A region at
    /// a page edge, a box that abuts ink at once, a band that ran out before it
    /// was deep enough to mean anything, or one that ended in picture rather
    /// than at an outline - which is what a sound effect's own white outline
    /// over art looks like.
    Unreadable,
}

/// How deep a band of fill this box needs before it counts, in native pixels.
///
/// Scaled by the box's short side, because the quantity being measured is a
/// balloon's padding around its own lettering and that padding scales with the
/// lettering. Clamped at both ends: [`MIN_FILL_DEPTH`] because a shallower
/// reading is the box's growth margin rather than a balloon, and
/// [`MAX_FILL_DEPTH`] because past it the only balloons still answering are the
/// roomy ones.
fn required_depth(bbox: Rect) -> u32 {
    (bbox.w.min(bbox.h) / 10).clamp(MIN_FILL_DEPTH, MAX_FILL_DEPTH)
}

/// How far out the walk may go before it gives up. Three times the depth it is
/// looking for, so a band interrupted by an outline still has room to have been
/// deep enough before it.
fn scan_depth(bbox: Rect) -> u32 {
    (required_depth(bbox) * 3).clamp(9, 32)
}

/// The pixels of one ring, as four segments: the paper directly above, below,
/// left of and right of the box, each exactly as long as the side it faces.
///
/// The order within a segment is what makes the run count in [`RingRead`] mean
/// anything: an outline crosses a segment in a contiguous arc and a halftone
/// crosses it once per dot, and the two are told apart by walking the segment
/// rather than by counting its pixels.
///
/// **Nothing near a corner is walked, and that is the whole shape of this
/// function.** A closed rectangular ring around a text box inside a *rounded*
/// balloon leaves the balloon at its four corners long before it leaves it
/// anywhere else: a text block filling two thirds of an ellipse has tens of
/// pixels of clearance on the axes and none at all on the diagonal, so ring 1
/// already reads the art beyond the rim, that art is two-sided screentone, and
/// the walk calls picture at depth 0 - [`Interior::Textured`], about the very
/// balloon a detector miss needed the page to find.
///
/// Every balloon shape this application meets is convex or nearly so, and on a
/// convex shape the clearance from a box's side is largest at that side's
/// midpoint and falls away towards both of its ends. So each segment is the
/// **middle half** of the side it faces, given by [`SIDE_INSET_QUARTERS`]: the
/// part of the paper the balloon actually reserves for the text, and the part a
/// letterer leaves room in. The ends of a full-length segment are still a box
/// corner's own distance off the axis, which on an eccentric ellipse is already
/// outside the rim at offset 1 - half a side is not a softening of the corner
/// rule but the rest of it.
fn ring_sides(bbox: Rect, offset: u32) -> [Vec<(i64, i64)>; 4] {
    let r = offset as i64;
    let inset = |side: u32| (side / SIDE_INSET_QUARTERS) as i64;
    let (ix, iy) = (inset(bbox.w), inset(bbox.h));
    let (x0, x1) = (bbox.x + ix, bbox.right() - ix);
    let (y0, y1) = (bbox.y + iy, bbox.bottom() - iy);
    let top = (x0..x1).map(|x| (x, bbox.y - r)).collect();
    let bottom = (x0..x1).map(|x| (x, bbox.bottom() - 1 + r)).collect();
    let left = (y0..y1).map(|y| (bbox.x - r, y)).collect();
    let right = (y0..y1).map(|y| (bbox.right() - 1 + r, y)).collect();
    [top, bottom, left, right]
}

/// One ring measured against a fill level.
struct RingRead {
    /// On-page pixels of the ring, text included: what the ring would have held
    /// if none of it were lettering.
    on_page: usize,
    samples: usize,
    off: usize,
    darker: usize,
    lighter: usize,
    /// Maximal contiguous runs of off-fill pixels, counted along each of
    /// [`ring_sides`]' four segments and summed. A run that would have spanned
    /// two segments counts once in each, which is the conservative direction:
    /// it can only move a reading away from *outline* and towards *picture*.
    runs: usize,
}

impl RingRead {
    /// Whether enough of the ring survived the text mask to be a sample of it.
    fn is_readable(&self) -> bool {
        self.samples >= MIN_RING_SAMPLES
            && self.samples * 100 >= self.on_page * MIN_RING_COVERAGE_PERCENT
    }

    fn is_fill(&self) -> bool {
        self.off * 100 <= self.samples * (100 - FILL_SHARE_PERCENT)
    }

    /// Whether what is off the fill is all on one side of it - all darker, or
    /// all lighter. Ink is one-sided; art is not. Vacuously true when nothing
    /// is off the fill at all.
    fn is_one_sided(&self) -> bool {
        self.darker.max(self.lighter) * 100 >= self.off * ONE_SIDED_PERCENT
    }

    /// Ink: few runs, and all of it on one side of the fill. A ring lying
    /// wholly on a thick outline is one run of one-sided pixels and reads here,
    /// which is the case a share threshold would have thrown away - the balloon
    /// whose lettering nearly fills it.
    fn is_outline(&self) -> bool {
        if self.runs > MAX_OUTLINE_RUNS {
            return false;
        }
        self.off > 0 && self.is_one_sided()
    }
}

fn read_ring(
    page: &Raster,
    text: &Mask,
    bbox: Rect,
    offset: u32,
    level: u16,
    tolerance: u16,
) -> RingRead {
    let mut read = RingRead { on_page: 0, samples: 0, off: 0, darker: 0, lighter: 0, runs: 0 };
    for side in ring_sides(bbox, offset) {
        // Each segment is walked on its own, so a run does not carry across the
        // gap between two of them.
        let mut previous_off = false;
        for (x, y) in side {
            if x < 0 || y < 0 || x >= page.width as i64 || y >= page.height as i64 {
                continue;
            }
            read.on_page += 1;
            if text.contains(x, y) {
                continue;
            }
            let luma = page.luma16_at(x as u32, y as u32);
            let off = luma.abs_diff(level) > tolerance;
            read.samples += 1;
            if off {
                read.off += 1;
                if luma < level {
                    read.darker += 1;
                } else {
                    read.lighter += 1;
                }
                if !previous_off {
                    read.runs += 1;
                }
            }
            previous_off = off;
        }
    }
    read
}

/// The fill the band is measured against, and how far from it the band may
/// stray: the median of the innermost rings, and their spread.
///
/// Both taken from the page rather than assumed. The level, because "white or
/// black" is not the choice - cream paper, newsprint and a screened grey
/// balloon are all fills, and [`crate::fit::ring`]'s refusal to snap a
/// near-white median to white is the same refusal one ring further in. The
/// tolerance, because a scan's noise is not the choice either: the
/// [`TOLERANCE_PERCENTILE`] of the rings' own deviations from the median, given
/// [`TOLERANCE_MARGIN`], and held between [`FILL_TOLERANCE`] and
/// [`MAX_FILL_TOLERANCE`]. A clean page measures a spread of zero and keeps the
/// floor; a noisy one widens to what its paper actually does; a screentone
/// measures its dots and is stopped at the cap, where the dots are still off
/// the fill.
fn fill_band(page: &Raster, text: &Mask, bbox: Rect) -> Option<(u16, u16)> {
    let mut values: Vec<u16> = Vec::new();
    for offset in 1..=MIN_FILL_DEPTH {
        for (x, y) in ring_sides(bbox, offset).into_iter().flatten() {
            if x < 0 || y < 0 || x >= page.width as i64 || y >= page.height as i64 {
                continue;
            }
            if text.contains(x, y) {
                continue;
            }
            values.push(page.luma16_at(x as u32, y as u32));
        }
    }
    if values.len() < MIN_RING_SAMPLES {
        return None;
    }
    values.sort_unstable();
    let level = values[values.len() / 2];

    let mut deviations: Vec<u16> = values.iter().map(|v| v.abs_diff(level)).collect();
    deviations.sort_unstable();
    let at = (deviations.len() * TOLERANCE_PERCENTILE / 100).min(deviations.len() - 1);
    let spread = deviations[at] as u32 * TOLERANCE_MARGIN.0 / TOLERANCE_MARGIN.1;
    let tolerance = (spread.min(MAX_FILL_TOLERANCE as u32) as u16).max(FILL_TOLERANCE);
    Some((level, tolerance))
}

/// The text pixels around a box, grown by [`TEXT_HALO`], over the area the walk
/// will read.
///
/// **Every** text pixel, not this region's: a neighbouring column of lettering
/// standing in the band is the region's own problem to ignore in exactly the
/// same way, and the segmentation mask does not distinguish them.
/// Grown by scattering rather than by [`Mask::dilated_within`], which is the
/// same disc and the same result: a `flagged_large` region's box is a quarter of
/// a page, and a dilation that visits every pixel of it would pay a
/// twenty-five-fold cost over an interior this walk never reads.
fn text_halo(seg: &Segmentation, area: Rect) -> Mask {
    let radius = TEXT_HALO as i64;
    let mut mask = Mask::empty(area);
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if x < 0 || y < 0 || x >= seg.width as i64 || y >= seg.height as i64 {
                continue;
            }
            if !seg.is_text(x as u32, y as u32) {
                continue;
            }
            for dy in -radius..=radius {
                for dx in -radius..=radius {
                    if dx * dx + dy * dy <= radius * radius {
                        mask.set(x + dx, y + dy, true);
                    }
                }
            }
        }
    }
    mask
}

/// [`interior`], with the text mask taken from the detector's segmentation.
pub fn interior_of(page: &Raster, seg: &Segmentation, bbox: Rect) -> Interior {
    let reach = scan_depth(bbox) + TEXT_HALO + 1;
    let area = bbox.grown(reach, page.width, page.height);
    interior(page, &text_halo(seg, area), bbox)
}

/// Walk outward from `bbox` and say what the paper around it is.
///
/// `text` is a mask of the page's text strokes, already grown - the pixels this
/// walk must not read, because a glyph in the band is not evidence about the
/// band. [`interior_of`] builds it from a [`Segmentation`]; a caller with a
/// mask of its own passes that.
///
/// The walk takes one ring per native pixel of offset, out to [`scan_depth`],
/// and stops at the first one that is not the fill:
///
/// - **Fill** - [`FILL_SHARE_PERCENT`] of the ring within [`FILL_TOLERANCE`] of
///   the level the innermost rings set. Deepens the band.
/// - **Outline** - off-fill pixels in at most [`MAX_OUTLINE_RUNS`] runs, all on
///   one side of the fill. Stops the walk *and is evidence*: a thin ink line
///   with uniform fill behind it is what a balloon's edge is. Nothing beyond it
///   is read at all, which is the point - beyond a balloon's outline is not the
///   balloon.
/// - **Picture** - anything else. Stops the walk.
///
/// **A band counts only when it is closed or never ends.** Closed: an outline
/// stopped the walk at least [`MIN_FILL_DEPTH`] out. Never ends: the walk ran
/// its whole depth without meeting picture, and the band is at least
/// [`required_depth`] deep. A band that ends in picture is not counted however
/// deep it was, and that is the whole of what separates a chat box from a
/// sound effect: an effect drawn with a white outline over art has 3 to 8
/// pixels of white around its strokes and then art, which the walk used to
/// accept as soon as the white was deep enough. The walk does not stop early
/// to accept it now; it reads on until the band is closed or broken.
pub fn interior(page: &Raster, text: &Mask, bbox: Rect) -> Interior {
    if bbox.w == 0 || bbox.h == 0 {
        return Interior::Unreadable;
    }
    let Some((level, tolerance)) = fill_band(page, text, bbox) else {
        return Interior::Unreadable;
    };
    let wanted = required_depth(bbox);

    let mut depth = 0u32;
    let mut stopped_on_outline = false;
    let mut stopped_on_picture = false;
    for offset in 1..=scan_depth(bbox) {
        let read = read_ring(page, text, bbox, offset, level, tolerance);
        if !read.is_readable() {
            // Nothing to read at this offset - a page edge, or a ring the
            // lettering fills. Neither is evidence either way, and the band is
            // not broken by it: the walk carries its depth past it.
            continue;
        }
        if read.is_fill() {
            depth += 1;
            continue;
        }
        stopped_on_outline = read.is_outline();
        stopped_on_picture = !stopped_on_outline;
        break;
    }

    if (stopped_on_outline && depth >= MIN_FILL_DEPTH) || (!stopped_on_picture && depth >= wanted) {
        return Interior::Solid { level, depth };
    }
    if stopped_on_picture && depth < MIN_FILL_DEPTH {
        return Interior::Textured;
    }
    // A band that ran into picture, or ran out before it was deep enough. Not a
    // verdict either way: the detector's answer stands.
    Interior::Unreadable
}

/// A line across the strip between two boxes counts as crossing something when
/// this many of its pixels are off the fill. Three rather than one: a scan
/// speckles, and one pixel is a speckle where three in a row is a stroke.
const MIN_OFF_PER_LINE: usize = 3;

/// And the strip is a boundary when this share of its lines, in percent, cross
/// something. A rim runs the whole length of the strip it stands in, so a
/// balloon's edge is near every line of it; a stray mark on the paper is on a
/// few.
const BOUNDARY_LINE_PERCENT: usize = 60;

/// How much of a line may be sampled off-fill and the strip still be one fill.
/// Read as the complement: a strip whose lines are clean is one balloon's
/// paper.
const CLEAN_LINE_PERCENT: usize = 100 - BOUNDARY_LINE_PERCENT;


/// How thick a slice is taken off the far end of an overhanging box, and the
/// largest share of that box's own extent it may be. A slice is a place to
/// stand, not a measurement of its own.
const FAR_SLICE: i64 = 20;

/// Whether `other`'s far end is on different paper from `near`.
///
/// Only asked of a box that overhangs - one wholly inside the other has no far
/// end to reach anywhere - and answered by the same strip reading as the rest of
/// [`merge_crosses_a_balloon`], between `near` and a slice at that far end.
fn far_edge_crosses(page: &Raster, seg: &Segmentation, near: Rect, other: Rect) -> bool {
    let thickness = |overhang: i64| FAR_SLICE.min(overhang / 2).max(1);
    // The overhang must be worth reading: a box a few pixels longer than its
    // neighbour is the same box. A box wholly inside the other has no far end
    // to reach anywhere and never gets here.
    let slice = if other.right() - near.right() > FAR_SLICE {
        let t = thickness(other.right() - near.right());
        Rect { x: other.right() - t, y: other.y, w: t as u32, h: other.h }
    } else if near.x - other.x > FAR_SLICE {
        let t = thickness(near.x - other.x);
        Rect { x: other.x, y: other.y, w: t as u32, h: other.h }
    } else if other.bottom() - near.bottom() > FAR_SLICE {
        let t = thickness(other.bottom() - near.bottom());
        Rect { x: other.x, y: other.bottom() - t, w: other.w, h: t as u32 }
    } else if near.y - other.y > FAR_SLICE {
        let t = thickness(near.y - other.y);
        Rect { x: other.x, y: other.y, w: other.w, h: t as u32 }
    } else {
        return false;
    };
    // If `near` still meets the slice there is no reach to measure, and that is
    // also what keeps the recursion one level deep.
    let x_gap = near.x.max(slice.x) - near.right().min(slice.right());
    let y_gap = near.y.max(slice.y) - near.bottom().min(slice.bottom());
    if x_gap <= 0 && y_gap <= 0 {
        return false;
    }
    merge_crosses_a_balloon(page, seg, near, slice)
}

/// Whether merging two boxes would cross a balloon.
///
/// **The question [`crate::detect::build_separated`] asks before it merges.**
/// Two text boxes in one balloon have that balloon's own fill between them; two
/// text boxes in *different* balloons have a rim, the art beyond it, and a
/// second rim. The merge rules in [`crate::detect`] cannot tell those apart -
/// they see two rectangles and an overlap fraction - and when they join the
/// second pair the result is one region whose box spans both balloons and the
/// artwork between them. That region then fails this module's other test too:
/// its surround is picture, so the gate sends ordinary dialogue to review under
/// *"text outside a speech bubble"*.
///
/// **Counted line by line, not as a share of the strip.** Two white balloons
/// have the *same* interior level, so a strip spanning both is nine tenths that
/// one level and the rim between them is a rounding error in any area
/// statistic. What separates them is that the rim is on every line of the strip
/// and a balloon's own fill is on none, so the strip is read as a bundle of
/// crossings: each line perpendicular to the gap either meets something or does
/// not, and [`BOUNDARY_LINE_PERCENT`] of them meeting something is a boundary.
///
/// Two boxes sharing no extent in either axis are refused whatever the paper
/// says: they are joined by a diagonal rather than by a strip, their hull covers
/// ground belonging to neither, and there is no reading of that hull which is a
/// reading of the thing being merged.
///
/// Boxes that already overlap have no strip between them and are never refused
/// here. That is not a gap in the test but the reason [`crate::detect`] asks it
/// of **every member** of a group rather than only of the box being absorbed: a
/// detector box bridging two balloons overlaps both, and it is the two text
/// blocks either side of it, not the bridge, that the strip is read between.
pub fn merge_crosses_a_balloon(page: &Raster, seg: &Segmentation, a: Rect, b: Rect) -> bool {
    let x_gap = a.x.max(b.x) - a.right().min(b.right());
    let y_gap = a.y.max(b.y) - a.bottom().min(b.bottom());
    if x_gap <= 0 && y_gap <= 0 {
        // Overlapping. There is no strip between them, but a merge claims more
        // than "these touch": it claims one text block on one piece of paper,
        // and the claim has to hold at the far end of the box that reaches
        // furthest. A detector box bridging two balloons overlaps the lettering
        // in the first one and ends inside the second, and this is where that
        // is caught - the strip between the near box and the far box's own far
        // edge is the rim, the art, and the second rim.
        return far_edge_crosses(page, seg, a, b) || far_edge_crosses(page, seg, b, a);
    }
    // Side by side: the strip is the columns between them over the rows they
    // share, and each of those rows is one crossing. Stacked: the other way
    // about. Diagonal: no strip at all.
    let (strip, across_rows) = if y_gap <= 0 && x_gap > 0 {
        let y = a.y.max(b.y);
        let bottom = a.bottom().min(b.bottom());
        (
            Rect { x: a.right().min(b.right()), y, w: x_gap as u32, h: (bottom - y) as u32 },
            true,
        )
    } else if x_gap <= 0 && y_gap > 0 {
        let x = a.x.max(b.x);
        let right = a.right().min(b.right());
        (
            Rect { x, y: a.bottom().min(b.bottom()), w: (right - x) as u32, h: y_gap as u32 },
            false,
        )
    } else {
        return true;
    };
    if strip.w == 0 || strip.h == 0 {
        return true;
    }

    let text = text_halo(seg, strip.grown(TEXT_HALO, page.width, page.height));
    let readable = |x: i64, y: i64| {
        x >= 0
            && y >= 0
            && x < page.width as i64
            && y < page.height as i64
            && !text.contains(x, y)
    };

    // The fill the crossings are measured against: the strip's own median, the
    // same refusal to assume white that [`fill_band`] makes.
    let mut values: Vec<u16> = Vec::new();
    for y in strip.y..strip.bottom() {
        for x in strip.x..strip.right() {
            if readable(x, y) {
                values.push(page.luma16_at(x as u32, y as u32));
            }
        }
    }
    if values.len() < MIN_RING_SAMPLES {
        // Nothing readable between them. Not evidence of a boundary.
        return false;
    }
    values.sort_unstable();
    let level = values[values.len() / 2];

    let (lines, along): (Vec<i64>, Vec<i64>) = if across_rows {
        ((strip.y..strip.bottom()).collect(), (strip.x..strip.right()).collect())
    } else {
        ((strip.x..strip.right()).collect(), (strip.y..strip.bottom()).collect())
    };
    let mut crossed = 0usize;
    let mut read = 0usize;
    for line in &lines {
        let mut off = 0usize;
        let mut samples = 0usize;
        for step in &along {
            let (x, y) = if across_rows { (*step, *line) } else { (*line, *step) };
            if !readable(x, y) {
                continue;
            }
            samples += 1;
            if page.luma16_at(x as u32, y as u32).abs_diff(level) > FILL_TOLERANCE {
                off += 1;
            }
        }
        if samples == 0 {
            continue;
        }
        read += 1;
        if off >= MIN_OFF_PER_LINE || off * 100 > samples * CLEAN_LINE_PERCENT {
            crossed += 1;
        }
    }
    read > 0 && crossed * 100 >= read * BOUNDARY_LINE_PERCENT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "manual local ONNX runtime and small RT-DETR graph required"]
    fn native_cpu_small_graph_runs_synthetic_page() {
        let runtime = std::env::var("RT_RUNTIME").expect("RT_RUNTIME");
        let graph = std::env::var("RT_GRAPH").expect("RT_GRAPH");
        crate::runtime::load(Path::new(&runtime)).unwrap();
        let mut model = BalloonDetector::open(Path::new(&graph), crate::accel::Preference::CpuOnly).unwrap();
        assert_eq!(model.selection().accelerator, crate::accel::Accelerator::Cpu);
        let page = crate::image::fixtures::by_name("l8").raster;
        let _boxes = model.detect(&page).unwrap();
    }

    fn balloon(x: i64, y: i64, w: u32, h: u32, class: BalloonClass) -> BalloonBox {
        BalloonBox { rect: Rect::new(x, y, w, h), class, score: 0.9 }
    }

    #[test]
    fn a_region_inside_a_bubble_is_in_a_balloon() {
        let balloons = [balloon(0, 0, 200, 200, BalloonClass::Bubble)];
        assert!(detected(Rect::new(50, 50, 40, 40), &balloons).inside());
    }

    #[test]
    fn a_region_with_no_balloon_over_it_is_not() {
        let balloons = [balloon(500, 500, 200, 200, BalloonClass::Bubble)];
        assert!(!detected(Rect::new(50, 50, 40, 40), &balloons).inside());
        assert!(!detected(Rect::new(50, 50, 40, 40), &[]).inside());
    }

    #[test]
    fn text_free_overrides_a_bubble_that_happens_to_overlap() {
        // A sound effect over art, with a large balloon box reaching across it.
        let balloons = [
            balloon(0, 0, 1000, 1000, BalloonClass::Bubble),
            balloon(400, 400, 200, 200, BalloonClass::TextFree),
        ];
        assert!(!detected(Rect::new(450, 450, 100, 100), &balloons).inside());
    }

    #[test]
    fn containment_is_of_the_centre_rather_than_of_any_corner() {
        // A tall sound effect whose top corner clips a balloon.
        let balloons = [balloon(0, 0, 200, 200, BalloonClass::Bubble)];
        assert!(!detected(Rect::new(150, 150, 400, 400), &balloons).inside());
    }

    /// The jagged balloon the paper walk reads as picture. A sure `bubble` box
    /// drawn around the whole of the text is the model answering the balloon
    /// question, and it grades as strongly as a `text_bubble` would.
    #[test]
    fn a_sure_bubble_enclosing_the_whole_region_grades_as_the_model_s_own_answer() {
        let balloons = [balloon(400, 100, 220, 480, BalloonClass::Bubble)];
        let region = Rect::new(414, 115, 177, 430);
        assert_eq!(detected(region, &balloons), Detected::TextInBubble);
        // Was a `settles` veto check; paper can no longer veto, so the grade
        // matters for the gate's reader, which asks for exactly this one.
    }

    /// 02.png's own geometry: one two-lobed balloon, two `bubble` boxes that
    /// meet at a single row, and a region under both. No box covers even half of
    /// it, and their union covers nearly all of it.
    #[test]
    fn two_lobes_of_one_balloon_enclose_a_region_neither_of_them_does() {
        let balloons = [
            BalloonBox {
                rect: Rect::new(429, 53, 201, 266),
                class: BalloonClass::Bubble,
                score: 0.92,
            },
            BalloonBox {
                rect: Rect::new(389, 319, 190, 264),
                class: BalloonClass::Bubble,
                score: 0.92,
            },
        ];
        let region = Rect::new(414, 115, 177, 430);
        for one in &balloons {
            assert!(
                covered_percent(region, &[one.rect]) < ENCLOSED_SHARE_PERCENT,
                "one lobe alone must not carry this"
            );
        }
        assert_eq!(detected(region, &balloons), Detected::TextInBubble);
    }

    /// And the union is a union: two boxes over the same paper do not add up to
    /// a covered region.
    #[test]
    fn overlapping_shapes_are_not_counted_twice() {
        let region = Rect::new(0, 0, 100, 100);
        let half = Rect::new(0, 0, 50, 100);
        assert_eq!(covered_percent(region, &[half, half]), 50);
        assert_eq!(covered_percent(region, &[half, Rect::new(40, 0, 60, 100)]), 100);
        assert_eq!(covered_percent(region, &[Rect::new(500, 500, 10, 10)]), 0);
    }

    /// And the case enclosure is narrow enough to exclude: a large `bubble` box
    /// reaching across a sound effect holds its centre but not its extent.
    #[test]
    fn a_sure_bubble_holding_only_the_centre_is_still_only_a_shape() {
        let balloons = [balloon(0, 0, 400, 400, BalloonClass::Bubble)];
        let region = Rect::new(100, 100, 500, 500);
        assert_eq!(detected(region, &balloons), Detected::Bubble);
        // Was "picture overrules a Bubble"; under `in_bubble` a Bubble is inside
        // whatever the paper says, and only the reader's grade stays lower.
        assert!(detected(region, &balloons).inside());
    }

    /// A `bubble` box that only just cleared emission is not the model being
    /// sure of anything, however much of the region it covers.
    #[test]
    fn an_unsure_bubble_does_not_grade_up_however_much_it_encloses() {
        let weak = BalloonBox {
            rect: Rect::new(0, 0, 400, 400),
            class: BalloonClass::Bubble,
            score: 0.4,
        };
        assert_eq!(detected(Rect::new(100, 100, 60, 60), &[weak]), Detected::Bubble);
    }

    /// Text drawn over art with no balloon behind it stays outside, enclosing
    /// `bubble` box or not: `text_free` is a statement about *this text*.
    /// Two sure shapes that meet, but together cover under three quarters of
    /// the region: the model has not said the text is enclosed.
    #[test]
    fn two_touching_shapes_under_the_share_stay_a_shape() {
        let region = Rect::new(0, 0, 100, 100);
        let balloons = [
            BalloonBox { rect: Rect::new(0, 0, 100, 40), class: BalloonClass::Bubble, score: 0.9 },
            BalloonBox { rect: Rect::new(0, 40, 100, 20), class: BalloonClass::Bubble, score: 0.9 },
        ];
        assert_eq!(detected(region, &balloons), Detected::Bubble);
    }

    /// A neighbouring balloon whose box reaches across the region but never
    /// touches the shape holding the centre lends nothing to the share.
    #[test]
    fn a_detached_neighbour_does_not_lend_its_area() {
        let region = Rect::new(0, 0, 100, 100);
        let balloons = [
            BalloonBox { rect: Rect::new(0, 0, 100, 60), class: BalloonClass::Bubble, score: 0.9 },
            BalloonBox { rect: Rect::new(0, 70, 100, 30), class: BalloonClass::Bubble, score: 0.9 },
        ];
        assert_eq!(detected(region, &balloons), Detected::Bubble);
        // Joined, the same two boxes are one balloon and the share is 100.
        let joined = [
            balloons[0].clone(),
            BalloonBox { rect: Rect::new(0, 60, 100, 40), class: BalloonClass::Bubble, score: 0.9 },
        ];
        assert_eq!(detected(region, &joined), Detected::TextInBubble);
    }

    #[test]
    fn text_free_still_wins_over_an_enclosing_bubble() {
        let balloons = [
            balloon(0, 0, 1000, 1000, BalloonClass::Bubble),
            balloon(400, 400, 200, 200, BalloonClass::TextFree),
        ];
        let region = Rect::new(450, 450, 100, 100);
        assert_eq!(detected(region, &balloons), Detected::Outside);
        assert!(!detected(region, &balloons).inside());
    }

    // The paper reading. Every page below is 8-bit grey, because §4 makes the
    // grayscale path canonical and this walk is one of its measurements.

    use crate::detect::{Letterbox, Segmentation};
    use crate::image::{BitDepth, ColorMode};

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

    /// A segmentation whose text pixels are wherever `f` says.
    fn segmentation(w: u32, h: u32, f: impl Fn(u32, u32) -> bool) -> Segmentation {
        let mut levels = vec![0u8; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                if f(x, y) {
                    levels[(y * w + x) as usize] = 255;
                }
            }
        }
        Segmentation { width: w, height: h, levels, fit: Letterbox::fit(w, h) }
    }

    /// The region every test below reads around: 60×60 at the centre of a
    /// 240×240 page, which asks for six pixels of fill (60 / 10).
    const BOX: Rect = Rect { x: 90, y: 90, w: 60, h: 60 };

    fn glyphs(x: u32, y: u32) -> bool {
        BOX.contains(x as i64, y as i64) && (x % 7 < 4) && (y % 9 < 6)
    }

    /// Diagonal hatching at pitch 7 - the tone that is *not* a balloon.
    fn hatched(x: u32, y: u32) -> bool {
        (x + y) % 7 < 2
    }

    /// A 45° halftone lattice, two families of dots at pitch 8.
    fn halftone(x: u32, y: u32) -> bool {
        let near = |a: u32, b: u32| {
            let (dx, dy) = ((a % 8) as i32 - 2, (b % 8) as i32 - 2);
            dx * dx + dy * dy <= 4
        };
        near(x, y) || near(x + 4, y + 4)
    }

    fn depth_of(interior: Interior) -> u32 {
        match interior {
            Interior::Solid { depth, .. } => depth,
            other => panic!("not solid: {other:?}"),
        }
    }

    #[test]
    fn a_uniform_fill_outside_the_text_is_a_balloon_interior() {
        // Cream paper at 246, not white: the fill is read off the page and
        // never snapped, the same refusal §4 states for a ring median.
        let page = gray_page(240, 240, |x, y| if glyphs(x, y) { 20 } else { 246 });
        let seg = segmentation(240, 240, glyphs);
        let read = interior_of(&page, &seg, BOX);
        // Depth was 6 (the early return at `required_depth`); the strict walk
        // reads on to `scan_depth` when nothing ends the band.
        assert!(matches!(read, Interior::Solid { level, .. } if level == 246 * 257), "{read:?}");
        assert!(depth_of(read) > required_depth(BOX), "the walk stopped early: {read:?}");
        assert!(in_bubble(&page, &seg, BOX, BOX, &[]), "a detector miss is what this exists to overrule");
    }

    /// The other half of the objection to reading balloons off the page:
    /// *white text over black*. A fill is a fill.
    #[test]
    fn a_black_balloon_under_white_lettering_reads_the_same_as_a_white_one() {
        let page = gray_page(240, 240, |x, y| {
            if glyphs(x, y) {
                250
            } else if BOX.grown(30, 240, 240).contains(x as i64, y as i64) {
                18
            } else {
                200
            }
        });
        let seg = segmentation(240, 240, glyphs);
        // Depth was 6: no early return, and the black fill runs past the walk.
        let read = interior_of(&page, &seg, BOX);
        assert!(matches!(read, Interior::Solid { level, .. } if level == 18 * 257), "{read:?}");
        assert!(depth_of(read) > required_depth(BOX), "the walk stopped early: {read:?}");
    }

    #[test]
    fn hatching_around_the_text_is_not_a_balloon_interior() {
        let page = gray_page(240, 240, |x, y| {
            if glyphs(x, y) {
                20
            } else if hatched(x, y) {
                40
            } else {
                250
            }
        });
        let seg = segmentation(240, 240, glyphs);
        let read = interior_of(&page, &seg, BOX);
        assert_eq!(read, Interior::Textured);
        // Was "picture overrules a Bubble but not a TextInBubble"; paper can
        // no longer veto, so picture only means the page rescues nothing.
        assert!(!in_bubble(&page, &seg, BOX, BOX, &[]), "picture rescues nothing");
    }

    /// The detector's *inside* wins even where the paper reads picture: the
    /// page only ever adds an answer, and every grade but `Outside` is one.
    #[test]
    fn the_detector_s_inside_wins_even_when_the_paper_reads_picture() {
        let page = gray_page(240, 240, |x, y| {
            if glyphs(x, y) {
                20
            } else if hatched(x, y) {
                40
            } else {
                250
            }
        });
        let seg = segmentation(240, 240, glyphs);
        assert_eq!(interior_of(&page, &seg, BOX), Interior::Textured);
        let masking = BOX.grown(5, 240, 240);
        let sure = balloon(60, 60, 120, 120, BalloonClass::TextInBubble);
        let weak = BalloonBox { score: 0.4, ..sure.clone() };
        let shape = balloon(110, 110, 120, 120, BalloonClass::Bubble);
        for boxes in [vec![sure.clone()], vec![weak], vec![shape]] {
            assert!(detected(masking, &boxes).inside());
            assert!(in_bubble(&page, &seg, BOX, masking, &boxes), "{boxes:?}");
        }
        // And `text_free` over the centre is the detector saying *outside*,
        // which picture does not rescue.
        let free = balloon(60, 60, 120, 120, BalloonClass::TextFree);
        assert!(!in_bubble(&page, &seg, BOX, masking, &[sure, free]));
    }

    /// The scan this module was measured against after it shipped: white paper
    /// with a spread of eight levels, one-sided because white saturates. Under
    /// a fixed six-level tolerance a fifth of every ring is off the fill in
    /// dozens of runs, which reads as picture; the band's own spread is what
    /// the tolerance has to follow.
    #[test]
    fn a_noisy_white_fill_is_still_a_balloon_interior() {
        // Deterministic speckle: a hash of the position, one-sided below 255,
        // with a long tail past six levels and nothing past twenty.
        let noise = |x: u32, y: u32| -> u8 {
            let h = x.wrapping_mul(2654435761).wrapping_add(y.wrapping_mul(40503)) ^ (x * y);
            let r = (h >> 7) % 100;
            if r < 60 {
                0
            } else if r < 80 {
                (r - 55) as u8 // 5..=24 levels, mostly under twelve
            } else {
                ((r - 80) % 12 + 8) as u8 // 8..=19
            }
        };
        let page = gray_page(240, 240, |x, y| if glyphs(x, y) { 20 } else { 255 - noise(x, y) });
        let seg = segmentation(240, 240, glyphs);
        let read = interior_of(&page, &seg, BOX);
        assert!(matches!(read, Interior::Solid { .. }), "noisy paper read as {read:?}");
        assert!(in_bubble(&page, &seg, BOX, BOX, &[]), "a caption box the detector missed");
    }

    /// The cap: a light grey tone whose dots sit thirty levels under the paper
    /// is not a fill however evenly spread its own deviations are.
    #[test]
    fn a_light_tone_is_not_widened_into_a_fill() {
        let page = gray_page(240, 240, |x, y| {
            if glyphs(x, y) {
                20
            } else if halftone(x, y) {
                220
            } else {
                250
            }
        });
        let seg = segmentation(240, 240, glyphs);
        assert_eq!(interior_of(&page, &seg, BOX), Interior::Textured);
    }

    #[test]
    fn the_detector_grade_follows_the_class_and_the_score() {
        let sure = balloon(0, 0, 200, 200, BalloonClass::TextInBubble);
        let weak = BalloonBox { rect: Rect::new(0, 0, 200, 200), class: BalloonClass::TextInBubble, score: 0.4 };
        // A shape near the text rather than around it: it holds the region's
        // centre and not its whole extent, which is what keeps it a `Bubble`.
        let shape = balloon(60, 60, 200, 200, BalloonClass::Bubble);
        let free = balloon(0, 0, 200, 200, BalloonClass::TextFree);
        let region = Rect::new(50, 50, 40, 40);
        assert_eq!(detected(region, std::slice::from_ref(&sure)), Detected::TextInBubble);
        assert_eq!(detected(region, &[sure.clone(), shape.clone()]), Detected::TextInBubble);
        assert_eq!(detected(region, &[weak]), Detected::Bubble);
        assert_eq!(detected(region, &[shape]), Detected::Bubble);
        assert_eq!(detected(region, &[sure, free]), Detected::Outside);
        assert_eq!(detected(region, &[]), Detected::Outside);
    }

    #[test]
    fn a_halftone_lattice_is_not_a_balloon_interior() {
        let page = gray_page(240, 240, |x, y| {
            if glyphs(x, y) {
                20
            } else if halftone(x, y) {
                60
            } else {
                250
            }
        });
        let seg = segmentation(240, 240, glyphs);
        assert_eq!(interior_of(&page, &seg, BOX), Interior::Textured);
    }

    /// Lettering on `fill` paper that runs `band` pixels out from the box, then
    /// a three-pixel ink outline, then art that is none of this region's
    /// business.
    fn boxed(band: i64) -> Raster {
        gray_page(240, 240, |x, y| {
            let (x, y) = (x as i64, y as i64);
            let outline = BOX.grown(band as u32 + 3, 240, 240);
            let inside_outline = BOX.grown(band as u32, 240, 240);
            if glyphs(x as u32, y as u32) {
                20
            } else if !outline.contains(x, y) {
                // Art beyond the balloon, and plenty of it.
                if (x * 3 + y * 5) % 11 < 5 { 30 } else { 240 }
            } else if !inside_outline.contains(x, y) {
                25 // the outline: three pixels of ink
            } else {
                246
            }
        })
    }

    /// The case the ring walk exists to get right, and the one a plain variance
    /// test gets wrong: four pixels of fill, then the balloon's own outline,
    /// then art. The walk stops **at** the outline, so what is beyond it is
    /// never read.
    #[test]
    fn a_thin_outline_ends_the_band_and_is_itself_the_evidence() {
        let page = boxed(4);
        let seg = segmentation(240, 240, glyphs);
        let read = interior_of(&page, &seg, BOX);
        let depth = depth_of(read);
        assert!(
            (MIN_FILL_DEPTH..required_depth(BOX)).contains(&depth),
            "the band was {depth} deep: shallower than this box asks for, which is the case \
             the outline has to carry"
        );
        assert!(in_bubble(&page, &seg, BOX, BOX, &[]), "boxed text the detector missed is inside");
    }

    /// Boxed text with room to spare: a band deeper than the box asks for,
    /// closed by the frame. The walk no longer stops once the band is deep
    /// enough; it reads on to the frame, and the frame is what makes it count.
    #[test]
    fn boxed_text_whose_band_ends_at_a_thin_line_is_inside() {
        let page = boxed(9);
        let seg = segmentation(240, 240, glyphs);
        let read = interior_of(&page, &seg, BOX);
        assert!(matches!(read, Interior::Solid { level, .. } if level == 246 * 257), "{read:?}");
        // Ring 1 is lettering halo and is skipped, so nine pixels of paper walk
        // as eight rings: past `required_depth`, where the walk used to stop.
        assert!(depth_of(read) >= required_depth(BOX), "{read:?}");
        assert!(in_bubble(&page, &seg, BOX, BOX, &[]));
    }

    /// The control for the tests above. Same geometry, no outline: art starts
    /// where the fill stops. Four pixels of fill is not six, and there is no
    /// outline behind it to say the fill was a balloon's - so the page declines
    /// to answer rather than inventing one, and the detector keeps its verdict.
    #[test]
    fn a_band_that_runs_into_art_with_no_outline_leaves_the_detector_alone() {
        let page = gray_page(240, 240, |x, y| {
            let (ix, iy) = (x as i64, y as i64);
            if glyphs(x, y) {
                20
            } else if !BOX.grown(4, 240, 240).contains(ix, iy) {
                if (ix * 3 + iy * 5) % 11 < 5 { 30 } else { 240 }
            } else {
                246
            }
        });
        let seg = segmentation(240, 240, glyphs);
        let read = interior_of(&page, &seg, BOX);
        assert_eq!(read, Interior::Unreadable);
        // Was a pair of `settles` checks; the same two answers through `in_bubble`.
        let shape = balloon(60, 60, 120, 120, BalloonClass::Bubble);
        assert!(in_bubble(&page, &seg, BOX, BOX, &[shape]), "an unreadable band changes nothing");
        assert!(!in_bubble(&page, &seg, BOX, BOX, &[]));
    }

    /// A sound effect drawn with a white outline over art: seven pixels of
    /// white around the strokes, as deep as this box asks for, and then
    /// picture. The walk used to accept the band the moment it was six deep;
    /// it now reads on, meets the art, and does not count a band picture ended.
    #[test]
    fn white_outlined_lettering_over_picture_is_not_inside() {
        let page = gray_page(240, 240, |x, y| {
            let (ix, iy) = (x as i64, y as i64);
            if glyphs(x, y) {
                20
            } else if !BOX.grown(7, 240, 240).contains(ix, iy) {
                if (ix * 3 + iy * 5) % 11 < 5 { 30 } else { 240 }
            } else {
                246 // the effect's own outline
            }
        });
        let seg = segmentation(240, 240, glyphs);
        assert!(7 > required_depth(BOX), "the premise: the white is deep enough to have passed");
        assert_eq!(interior_of(&page, &seg, BOX), Interior::Unreadable);
        assert!(!in_bubble(&page, &seg, BOX, BOX, &[]));
        let free = balloon(60, 60, 120, 120, BalloonClass::TextFree);
        assert!(!in_bubble(&page, &seg, BOX, BOX.grown(5, 240, 240), &[free]));
    }

    /// A glyph that spills past the masking box is a glyph, not a wall. Without
    /// the text mask the first ring reads as ink and the walk stops on it.
    #[test]
    fn the_text_s_own_strokes_are_not_read_as_the_band() {
        let spill = |x: u32, y: u32| glyphs(x, y) || ((x == 89 || x == 150) && (100..140).contains(&y));
        let page = gray_page(240, 240, |x, y| if spill(x, y) { 20 } else { 246 });
        let seg = segmentation(240, 240, spill);

        // Was an exact `depth: 6` in both; the strict walk reads past it.
        let blind = interior(&page, &Mask::empty(BOX.grown(30, 240, 240)), BOX);
        assert!(!matches!(blind, Interior::Solid { .. }), "the spill was read as ink: {blind:?}");

        let seeing = interior_of(&page, &seg, BOX);
        assert!(matches!(seeing, Interior::Solid { level, .. } if level == 246 * 257), "{seeing:?}");
        assert!(depth_of(seeing) > required_depth(BOX));
    }

    /// Text on wide flat paper: the band runs the whole walk and nothing ends
    /// it, which is a chat box, a sign or interface text on flat white. The
    /// page rescues it even where the detector drew `text_free` over it.
    #[test]
    fn text_on_wide_flat_paper_is_inside() {
        let page = gray_page(240, 240, |x, y| if glyphs(x, y) { 20 } else { 246 });
        let seg = segmentation(240, 240, glyphs);
        // Was `depth: 6` under the early return; now the whole walk.
        let read = interior_of(&page, &seg, BOX);
        assert!(matches!(read, Interior::Solid { level, .. } if level == 246 * 257), "{read:?}");
        assert!(depth_of(read) > required_depth(BOX), "the walk stopped early: {read:?}");
        let masking = BOX.grown(5, 240, 240);
        assert!(in_bubble(&page, &seg, BOX, masking, &[]));
        let free = balloon(60, 60, 120, 120, BalloonClass::TextFree);
        assert!(in_bubble(&page, &seg, BOX, masking, &[free]), "text_free is the detector's miss here");
    }

    /* ---- nothing is read between the strokes ---- */

    /// Lettering sparse enough to leave paper between its strokes: after
    /// `TEXT_HALO` about the 50 to 62% the real narration boxes leave, where
    /// `glyphs` is a lattice that leaves under a tenth.
    fn letters(x: u32, y: u32) -> bool {
        BOX.contains(x as i64, y as i64) && (x % 12 < 3) && (y % 14 < 5)
    }

    /// A page whose box is lettering on `fill`, with `inside` painting whatever
    /// else is between the strokes, and art everywhere outside the box, so the
    /// walk meets picture at offset 1.
    fn plate(fill: u8, inside: impl Fn(u32, u32) -> Option<u8>) -> Raster {
        gray_page(240, 240, |x, y| {
            if letters(x, y) {
                20
            } else if BOX.contains(x as i64, y as i64) {
                inside(x, y).unwrap_or(fill)
            } else if hatched(x, y) {
                40
            } else {
                250
            }
        })
    }

    /// These fixtures pinned a second reading, of the paper *between* the
    /// strokes, that rescued a narration box framed tight against its
    /// lettering over art. It is gone: on real pages it read the white outline
    /// between a sound effect's strokes as paper (`Solid` at depth 0), which
    /// put outlined effects inside. Every plate below is now the walk's answer
    /// alone - picture at offset 1 - including the three that used to read as
    /// paper (the framed narration box, the plain plate and the one-sided
    /// speckle); a tight narration box is left to the detector.
    #[test]
    fn nothing_between_the_strokes_is_read_as_paper() {
        let dark = |x: u32, y: u32| (x as i64 * 3 + y as i64 * 5) % 37 < 2;
        let light = |x: u32, y: u32| (x as i64 * 7 + y as i64 * 2) % 41 < 2;

        // The narration box: a one-pixel frame inside the box, tight against
        // sparse lettering, and art everywhere outside it.
        let framed = Rect::new(BOX.x + 1, BOX.y + 1, BOX.w - 2, BOX.h - 2);
        let framed_letters =
            move |x: u32, y: u32| framed.contains(x as i64, y as i64) && (x % 12 < 3) && (y % 14 < 5);
        let narration = gray_page(240, 240, |x, y| {
            let (ix, iy) = (x as i64, y as i64);
            if framed_letters(x, y) {
                20
            } else if framed.contains(ix, iy) {
                246
            } else if BOX.contains(ix, iy) {
                20
            } else if hatched(x, y) {
                40
            } else {
                250
            }
        });
        // A box that is almost all lettering.
        let dense =
            |x: u32, y: u32| BOX.contains(x as i64, y as i64) && !(x.is_multiple_of(5) && y.is_multiple_of(5));
        let crowded = gray_page(240, 240, |x, y| {
            if dense(x, y) {
                20
            } else if BOX.contains(x as i64, y as i64) {
                246
            } else if hatched(x, y) {
                40
            } else {
                250
            }
        });

        let lettered = segmentation(240, 240, letters);
        let cases = [
            ("narration box, framed tight", narration, segmentation(240, 240, framed_letters)),
            ("plain paper between the strokes", plate(250, |_, _| None), lettered.clone()),
            ("screentone between the strokes", plate(250, |x, y| halftone(x, y).then_some(60)), lettered.clone()),
            ("finer lighter tone", plate(250, |x, y| ((x * 7 + y * 11) % 17 < 4).then_some(205)), lettered.clone()),
            (
                "mostly frame",
                plate(250, |x, y| {
                    let (x, y) = (x as i64, y as i64);
                    (x < BOX.x + 5 || x >= BOX.right() - 5 || y < BOX.y + 5 || y >= BOX.bottom() - 5)
                        .then_some(20)
                }),
                lettered.clone(),
            ),
            (
                "two-sided speckle",
                plate(200, |x, y| if dark(x, y) { Some(40) } else { light(x, y).then_some(255) }),
                lettered.clone(),
            ),
            ("one-sided speckle", plate(200, |x, y| (dark(x, y) || light(x, y)).then_some(40)), lettered),
            ("almost all lettering", crowded, segmentation(240, 240, dense)),
        ];
        for (name, page, seg) in cases {
            assert_eq!(interior_of(&page, &seg, BOX), Interior::Textured, "{name}");
            assert!(!in_bubble(&page, &seg, BOX, BOX, &[]), "{name}");
        }
    }

    /// And too few pixels outright. A box smaller than
    /// [`MIN_RING_SAMPLES`] worth of paper has no reading either way.
    #[test]
    fn a_box_with_almost_no_pixels_in_it_is_not_read_from_the_inside() {
        let tiny = Rect::new(100, 100, 5, 5);
        let inked = |x: u32, y: u32| tiny.contains(x as i64, y as i64) && x.is_multiple_of(2);
        let page = gray_page(240, 240, |x, y| {
            if inked(x, y) {
                20
            } else if tiny.contains(x as i64, y as i64) {
                246
            } else if hatched(x, y) {
                40
            } else {
                250
            }
        });
        let seg = segmentation(240, 240, inked);
        // 25 pixels, of which the halo leaves nothing like 16 of paper.
        assert!(!matches!(interior_of(&page, &seg, tiny), Interior::Solid { .. }));
    }

    #[test]
    fn a_box_with_no_readable_paper_around_it_is_not_a_verdict() {
        let page = gray_page(8, 8, |_, _| 200);
        let seg = segmentation(8, 8, |_, _| false);
        let bbox = Rect::new(0, 0, 4, 4);
        let read = interior_of(&page, &seg, bbox);
        assert_eq!(read, Interior::Unreadable);
        // Was a pair of `settles` checks: unreadable paper leaves the detector's answer.
        assert!(in_bubble(&page, &seg, bbox, bbox, &[balloon(0, 0, 8, 8, BalloonClass::Bubble)]));
        assert!(!in_bubble(&page, &seg, bbox, bbox, &[]));
        // And a box with no area at all is not a question.
        assert_eq!(interior_of(&page, &seg, Rect::new(0, 0, 0, 0)), Interior::Unreadable);
    }

    /// The depth asked for scales with the lettering, and is clamped at both
    /// ends: a small box must not need a balloon's whole width of fill, and a
    /// large one must not be able to ask for more than any balloon has.
    #[test]
    fn the_band_asked_for_scales_with_the_text_and_stops_at_both_ends() {
        assert_eq!(required_depth(Rect::new(0, 0, 12, 12)), MIN_FILL_DEPTH);
        assert_eq!(required_depth(Rect::new(0, 0, 60, 200)), 6);
        assert_eq!(required_depth(Rect::new(0, 0, 400, 400)), MAX_FILL_DEPTH);
        // Every walk reaches past what it asks for, so an interrupted band has
        // room to have been deep enough first.
        for side in [8u32, 60, 400] {
            let bbox = Rect::new(0, 0, side, side);
            assert!(scan_depth(bbox) > required_depth(bbox));
        }
    }

    /// Two columns of one balloon have that balloon's fill between them.
    #[test]
    fn two_columns_of_one_balloon_may_be_merged() {
        let cols = |x: u32, y: u32| {
            (100..140).contains(&x) && (100..300).contains(&y)
                || (180..220).contains(&x) && (100..300).contains(&y)
        };
        let page = gray_page(400, 400, |x, y| if cols(x, y) { 20 } else { 250 });
        let seg = segmentation(400, 400, cols);
        assert!(!merge_crosses_a_balloon(
            &page,
            &seg,
            Rect::new(95, 95, 50, 210),
            Rect::new(175, 95, 50, 210)
        ));
    }

    /// Anti-aliased text overhang bordering a strip is covered by the text halo.
    #[test]
    fn text_halo_scans_outside_strip_to_cover_text_overhang() {
        let cols = |x: u32, y: u32| {
            (50..100).contains(&x) && (100..300).contains(&y)
                || (104..154).contains(&x) && (100..300).contains(&y)
        };
        let page = gray_page(400, 400, |x, y| {
            if cols(x, y) {
                20
            } else if (x == 100 || x == 101) && (100..300).contains(&y) {
                100
            } else {
                250
            }
        });
        let seg = segmentation(400, 400, cols);
        assert!(!merge_crosses_a_balloon(
            &page,
            &seg,
            Rect::new(50, 100, 50, 200),
            Rect::new(104, 100, 50, 200)
        ));
    }


    /// Two balloons have a rim, art, and a second rim between them. This is the
    /// merge that produces one region spanning two bubbles.
    #[test]
    fn a_merge_across_two_balloons_is_refused() {
        let cols = |x: u32, y: u32| {
            (100..140).contains(&x) && (100..300).contains(&y)
                || (260..300).contains(&x) && (100..300).contains(&y)
        };
        let tone = |x: u32, y: u32| (x + y) % 7 < 3;
        let page = gray_page(400, 400, |x, y| {
            if cols(x, y) {
                20
            } else if !(160..240).contains(&x) {
                250 // the two balloons' interiors
            } else if !(166..234).contains(&x) {
                25 // their rims
            } else if tone(x, y) {
                60
            } else {
                246
            }
        });
        let seg = segmentation(400, 400, cols);
        assert!(merge_crosses_a_balloon(
            &page,
            &seg,
            Rect::new(95, 95, 50, 210),
            Rect::new(255, 95, 50, 210)
        ));
    }

    /// Boxes that already meet have no gap, and boxes sharing no extent at all
    /// are joined by a diagonal rather than by a strip of paper.
    #[test]
    fn overlapping_boxes_merge_and_diagonal_ones_do_not() {
        let page = gray_page(400, 400, |_, _| 250);
        let seg = segmentation(400, 400, |_, _| false);
        assert!(!merge_crosses_a_balloon(
            &page,
            &seg,
            Rect::new(100, 100, 80, 80),
            Rect::new(150, 120, 80, 80)
        ));
        assert!(merge_crosses_a_balloon(
            &page,
            &seg,
            Rect::new(100, 100, 60, 60),
            Rect::new(220, 220, 60, 60)
        ));
    }

    /// One ring is four segments, each walked in order, which is the whole
    /// reason an outline and a halftone can be told apart: both put off-tone
    /// pixels on the ring, and only one of them puts them next to each other.
    #[test]
    fn a_ring_is_four_segments_walked_in_order_and_never_reaches_a_corner() {
        let sides = ring_sides(Rect::new(10, 10, 4, 4), 1);
        let flat: Vec<(i64, i64)> = sides.iter().flatten().copied().collect();
        assert_eq!(flat.len(), 4 * 2, "four segments, each the middle half of its side");
        assert_eq!(sides[0], vec![(11, 9), (12, 9)], "the paper above");
        assert_eq!(sides[3], vec![(14, 11), (14, 12)], "the paper to the right");
        // Nothing within a quarter-side of a corner is ever read, and no corner
        // of the enclosing 6×6 square is.
        for near_corner in [(9, 9), (14, 9), (9, 14), (14, 14), (10, 9), (13, 9), (14, 10)] {
            assert!(!flat.contains(&near_corner), "a corner was walked: {near_corner:?}");
        }
        let mut sorted = flat.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), flat.len(), "a pixel was walked twice");
    }

    /// The case the corner-free walk exists for, and the one a closed
    /// rectangular ring gets wrong: an elliptical balloon with lettering that
    /// nearly fills it, over screentone. The box's corners are inside the rim
    /// and its sides have room; a ring that turns the corner leaves the balloon
    /// at once, reads the tone beyond it, and calls picture.
    #[test]
    fn an_elliptical_balloon_full_of_lettering_is_still_a_balloon_interior() {
        let (cx, cy, rx, ry) = (400.0f64, 400.0f64, 180.0f64, 140.0f64);
        // Corners at 0.96 of the way to the rim: a letterer's own margin.
        let text = Rect::new(400 - 122, 400 - 95, 244, 190);
        let t = |x: u32, y: u32| {
            let (dx, dy) = ((x as f64 - cx) / rx, (y as f64 - cy) / ry);
            (dx * dx + dy * dy).sqrt()
        };
        // A 45° screentone lattice outside the balloon, at pitch 9.
        let tone = |x: u32, y: u32| {
            let near = |a: u32, b: u32| {
                let (dx, dy) = ((a % 9) as i32 - 3, (b % 9) as i32 - 3);
                dx * dx + dy * dy <= 5
            };
            near(x, y) || near(x + 4, y + 4)
        };
        let letters = |x: u32, y: u32| {
            text.contains(x as i64, y as i64) && (x % 11 < 7) && (y % 13 < 9)
        };
        let page = gray_page(800, 800, |x, y| {
            if letters(x, y) {
                20
            } else if t(x, y) <= 1.0 {
                250
            } else if t(x, y) <= 1.0 + 3.0 / rx {
                25 // the rim
            } else if tone(x, y) {
                60
            } else {
                246
            }
        });
        let seg = segmentation(800, 800, letters);
        // The box the gate actually asks about: `detect::boxes` grows the
        // detector's box by +2 all sides and +1 more on the right, then +5.
        let masking = Rect::new(text.x - 7, text.y - 7, text.w + 15, text.h + 14);
        let read = interior_of(&page, &seg, masking);
        assert!(
            matches!(read, Interior::Solid { .. }),
            "an ordinary balloon read as {read:?}, which sends its dialogue to review",
        );
        assert!(in_bubble(&page, &seg, masking, masking, &[]), "and the page alone carries it");
    }
}
