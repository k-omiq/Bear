//! The four box tiers, and the size rules.
//!
//! The detector's raw boxes become four progressively grown and merged
//! sets, each with one job:
//!
//! | Tier | Built by | Used for |
//! |---|---|---|
//! | `tight` | detector output → centre-in-box merge → +2 all sides, +3 right | crops, tight reference |
//! | `extended` | after filtering → +5 all sides | drawn into the box mask |
//! | `masking` | merged at >20% of the smaller box's area | the masking box |
//! | `reference` | merged, +20 px | the base cutout rings are sampled from, and the paste origin |
//!
//! Everything here is **sorted before it merges**. Upstream's merge is
//! single-pass and non-transitive, and one implementation iterates a `set`, so
//! the answer depends on iteration order; it must not.
//! Sorting by `(y1, x1)` is what makes two runs agree.

use crate::mask::Rect;

use super::{DetBox, DetectedLanguage};

/// One detector box after the first merge and the first growth.
#[derive(Debug, Clone, PartialEq)]
pub struct TightBox {
    pub tight: Rect,
    pub extended: Rect,
    pub language: DetectedLanguage,
    pub confidence: f32,
}

/// A masking box and the boxes that went into it. This is the unit the rest of
/// the pipeline works on: one mask, one fill tone, one review row.
#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    pub masking: Rect,
    pub reference: Rect,
    pub members: Vec<TightBox>,
    /// Bigger than 40× the page's median box, or taller than a quarter of the
    /// page. Still cleaned - §2 makes this a flag where upstream makes it a
    /// drop - but it goes to the inpaint ladder with wider context and it is
    /// surfaced in review.
    pub flagged_large: bool,
    /// The text group this region is, when it came from
    /// [`crate::text_groups`]. Its exact lettering seeds the fit instead of
    /// the segmentation under `masking`, so a neighbouring group's glyphs never
    /// leak into this region's mask.
    pub group: Option<std::sync::Arc<crate::text_groups::RegionGroup>>,
}

impl Region {
    /// The lettering's own bounds: the hull of the members' `tight` boxes.
    ///
    /// **Not `masking`, and the difference is evidence.** `masking` is `tight`
    /// grown by five more pixels on every side, and that growth belongs to the
    /// edit - it is the margin the mask is allowed to reach into. A caller
    /// asking what the *paper around the text* looks like
    /// ([`crate::balloon::in_bubble`], through [`crate::balloon::interior_of`])
    /// must not spend that margin before it starts looking: in a balloon whose
    /// lettering nearly fills it, five pixels on each side is most of what is
    /// left, and the walk then reads the rim where it should have read the
    /// fill.
    pub fn text_bounds(&self) -> Rect {
        self.members
            .iter()
            .map(|m| m.tight)
            .reduce(|a, b| hull(&a, &b))
            .unwrap_or(self.masking)
    }

    /// The strongest member's language. A merged region can hold boxes the
    /// detector called different things; the confident one wins rather than a
    /// vote, because the class is a weak signal to begin with.
    pub fn language(&self) -> DetectedLanguage {
        self.members
            .iter()
            .max_by(|a, b| a.confidence.total_cmp(&b.confidence))
            .map(|m| m.language)
            .unwrap_or(DetectedLanguage::Japanese)
    }

    /// One region from one box, grown through the same four tiers as a
    /// detector box, for a caller holding a box the text detector never
    /// emitted.
    ///
    /// [`crate::text_groups::Grouping::region`] and
    /// [`crate::text_groups::Grouping::detector_only_region`] are that caller:
    /// a text group's bounds and the balloon detector's `text_free` and
    /// `text_bubble` boxes are a second opinion
    /// about where text is, and a page's narration boxes and sound effects are
    /// sometimes the only opinion there is. The growth is `build_separated`'s
    /// growth to the pixel - `grown` here, not a fresh set of numbers - so an
    /// adopted region's mask reaches exactly as far as its neighbours' do.
    ///
    /// **The size rules apply by half.** The large-box flag is applied, since
    /// a chapter-title strip down the side of a page is exactly the thing
    /// review should be told about. The small-box `Drop` is not applied at
    /// all: this box exists *because* the text detector did not emit it, so
    /// dropping it against a median computed from the boxes it did emit would
    /// discard the only evidence there is. A merge that follows would be the
    /// same mistake - an adopted box is only built when nothing else covers
    /// it.
    pub fn from_box(
        rect: Rect,
        confidence: f32,
        language: DetectedLanguage,
        page_w: u32,
        page_h: u32,
        median_area: i64,
    ) -> Region {
        let tight = tight_rect(rect, page_w, page_h);
        let extended = grown(tight, 5, 0, page_w, page_h);
        let flagged_large =
            size_verdict(area(&rect), median_area, rect.h, page_h, confidence)
                == SizeVerdict::FlagLarge;
        Region {
            masking: extended,
            reference: extended.grown(20, page_w, page_h),
            members: vec![TightBox { tight, extended, language, confidence }],
            flagged_large,
            group: None,
        }
    }
}

/// A detector box's tight tier: the box grown by 2, and 1 more on the right.
pub fn tight_rect(rect: Rect, page_w: u32, page_h: u32) -> Rect {
    grown(rect, 2, 1, page_w, page_h)
}

/// Grow on every side, and by a little more on the right.
///
/// The asymmetry is upstream's and it is not arbitrary: vertical Japanese runs
/// right-to-left, so the right edge of a text block is where the next column
/// would start and where anti-aliasing spills.
fn grown(rect: Rect, all: u32, extra_right: u32, page_w: u32, page_h: u32) -> Rect {
    let base = rect.grown(all, page_w, page_h);
    let right = (base.right() + extra_right as i64).min(page_w as i64);
    Rect { x: base.x, y: base.y, w: (right - base.x).max(0) as u32, h: base.h }
}

fn centre_of(rect: &Rect) -> (i64, i64) {
    (rect.x + rect.w as i64 / 2, rect.y + rect.h as i64 / 2)
}

fn hull(a: &Rect, b: &Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = a.right().max(b.right());
    let bottom = a.bottom().max(b.bottom());
    Rect { x, y, w: (right - x) as u32, h: (bottom - y) as u32 }
}

fn intersection_area(a: &Rect, b: &Rect) -> i64 {
    let w = (a.right().min(b.right()) - a.x.max(b.x)).max(0);
    let h = (a.bottom().min(b.bottom()) - a.y.max(b.y)).max(0);
    w * h
}

fn area(rect: &Rect) -> i64 {
    rect.w as i64 * rect.h as i64
}

/// Two extended boxes become one region above this share of the smaller box's
/// area. §2's "merged at >20% of the smaller box's area", as a number one
/// other place can also ask for.
pub const MERGE_OVERLAP_SHARE: f64 = 0.2;

/// Whether two rectangles overlap by more than [`MERGE_OVERLAP_SHARE`] of the
/// smaller one's area.
///
/// The test the extended tier merges on, lifted out so that a caller asking
/// *is this box already covered?* asks it the same way the merge does rather
/// than picking its own fraction. A predicate rather than a share, and
/// multiplication rather than division, so that a box sitting exactly on the
/// threshold falls the same side of it here as it did before this was a
/// function. False when either rectangle is empty.
pub fn overlaps_enough(a: &Rect, b: &Rect) -> bool {
    let smaller = area(a).min(area(b));
    smaller > 0 && intersection_area(a, b) as f64 > MERGE_OVERLAP_SHARE * smaller as f64
}

/// Sort regions by `(masking.y, masking.x)`.
///
/// Reproducible order again, and this time for the ids rather than the merge:
/// a run names each region by its index in this list, so a list assembled from
/// two sources - a grouping's cleaning groups and its detector-only
/// groups - has to be put back in reading
/// order before anything counts it.
pub fn sort_regions(regions: &mut [Region]) {
    in_reading_order(regions, |r| r.masking);
}

/// Sorted by `(y1, x1)`. The single fact that makes every merge below
/// reproducible.
fn in_reading_order<T>(items: &mut [T], rect_of: impl Fn(&T) -> Rect) {
    items.sort_by(|a, b| {
        let (ra, rb) = (rect_of(a), rect_of(b));
        ra.y.cmp(&rb.y).then(ra.x.cmp(&rb.x)).then(ra.h.cmp(&rb.h)).then(ra.w.cmp(&rb.w))
    });
}

/// Merge boxes whose **centre** falls inside another box.
///
/// Single-pass and non-transitive, like upstream: A absorbing B does not then
/// let B's absorbed C reach A. Reproduced rather than improved because the size
/// statistics below are calibrated against the box population this produces.
///
/// Each merged box is returned with the **original** boxes that went into it.
/// Those are what the veto is asked about further down: once a group has been
/// hulled, the rectangle that comes out overlaps everything the group reached,
/// and a strip of paper between two overlapping rectangles does not exist. The
/// two text blocks a bridging box brought together are still two boxes here,
/// and they are still the pair the page has an answer about.
fn merge_centre_in_box(
    mut boxes: Vec<DetBox>,
    separated: &mut dyn FnMut(Rect, Rect) -> bool,
) -> Vec<(DetBox, Vec<Rect>)> {
    in_reading_order(&mut boxes, |b| b.rect);
    // A box that overlaps two boxes the page puts in *different* balloons is a
    // bridge across them and not a block of text. Left in, it is absorbed into
    // whichever it meets first and drags that region's box across the gap, or -
    // refused by both - stands as a third region overlapping the other two. The
    // text it covers is already covered by the two boxes it spans, which is what
    // makes dropping it safe rather than a loss.
    let overlaps = |a: &Rect, b: &Rect| {
        a.x.max(b.x) < a.right().min(b.right()) && a.y.max(b.y) < a.bottom().min(b.bottom())
    };
    let mut bridge = vec![false; boxes.len()];
    for i in 0..boxes.len() {
        let touching: Vec<Rect> = boxes
            .iter()
            .enumerate()
            .filter(|(j, o)| *j != i && overlaps(&boxes[i].rect, &o.rect))
            .map(|(_, o)| o.rect)
            .collect();
        bridge[i] = touching.iter().enumerate().any(|(m, a)| {
            touching.iter().skip(m + 1).any(|b| separated(*a, *b))
        });
    }
    let boxes: Vec<DetBox> = boxes
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !bridge[*i])
        .map(|(_, b)| b)
        .collect();

    let mut merged: Vec<(DetBox, Vec<Rect>)> = Vec::new();
    let mut absorbed = vec![false; boxes.len()];

    for i in 0..boxes.len() {
        if absorbed[i] {
            continue;
        }
        let mut current = boxes[i].clone();
        // Every box that went into `current`, not just the hull of them. The
        // veto is asked of each: a box bridging two balloons overlaps both, so
        // it is separated from neither, and the pair the strip has to be read
        // between is the two it brought together.
        let mut group = vec![current.rect];
        for (j, other) in boxes.iter().enumerate().skip(i + 1) {
            if absorbed[j] {
                continue;
            }
            let (cx, cy) = centre_of(&other.rect);
            let (ox, oy) = centre_of(&current.rect);
            if !(current.rect.contains(cx, cy) || other.rect.contains(ox, oy)) {
                continue;
            }
            if group.iter().any(|member| separated(*member, other.rect)) {
                continue;
            }
            current.rect = hull(&current.rect, &other.rect);
            current.confidence = current.confidence.max(other.confidence);
            group.push(other.rect);
            absorbed[j] = true;
        }
        merged.push((current, group));
    }
    merged
}

/// What the size rules did to a box. Kept as an outcome rather than applied
/// silently, because a dropped box is not nothing - §2 makes an undersized box
/// a drop and an oversized one a flag, and the difference matters in review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeVerdict {
    Keep,
    Drop,
    FlagLarge,
}

/// A box the detector is at least this sure of is never dropped for being
/// small.
///
/// The small-box rule is there for speckle - a mark the detector half-saw and
/// half-believed - and it was catching short single-column dialogue instead. On
/// the user's own scans (1080×1536, twelve pages) seven boxes the detector was
/// 0.84 to 0.94 sure of were dropped for sitting under 0.15 of the page median:
/// 07.png 146,388 26×75 @0.94 and 896,505 32×63 @0.84, 08.png 138,411 22×100
/// @0.94 and 423,1140 26×48 @0.90, 11.png 692,1165 26×127 @0.94, 12.png
/// 465,519 24×51 @0.94, 18.png 930,1145 24×50 @0.94. Each is a line of dialogue
/// inside a balloon the balloon detector scored 0.74 to 0.90, and each vanished
/// from the page - not into review, where a wrong call can be undone, but out of
/// the run entirely. The one small box on those pages that *is* speckle,
/// 05.png 466,829 35×64, scored 0.41, and that is the gap this threshold sits
/// in: half way between the noise and the dialogue, and well above
/// [`crate::detect`]'s own emission floor.
pub const SIZE_DROP_MAX_CONFIDENCE: f32 = 0.7;

/// §2's thresholds, scaled by **text size** rather than page size.
///
/// Revision 2 used fractions of page area, which was worse than the absolute
/// values it replaced: `8e-3 × page_area` is 7 680 px² on an 800×1200 webtoon,
/// 31× smaller than the dialogue box used to condemn the absolute version, so
/// every box would flag.
///
/// `confidence` is the detector's own, and it only ever spares a box: at or
/// above [`SIZE_DROP_MAX_CONFIDENCE`] the small-box `Drop` does not apply, for
/// the reason stated there. The large-box flag is unaffected - a confident
/// enormous box is still a box worth surfacing, and flagging costs nothing.
pub fn size_verdict(
    box_area: i64,
    median_area: i64,
    box_height: u32,
    page_height: u32,
    confidence: f32,
) -> SizeVerdict {
    if median_area > 0
        && (box_area as f64) < 0.15 * median_area as f64
        && confidence < SIZE_DROP_MAX_CONFIDENCE
    {
        return SizeVerdict::Drop;
    }
    if median_area > 0 && (box_area as f64) > 40.0 * median_area as f64 {
        return SizeVerdict::FlagLarge;
    }
    if (box_height as f64) > 0.25 * page_height as f64 {
        return SizeVerdict::FlagLarge;
    }
    SizeVerdict::Keep
}

/// The median box area on a page, computed **after detection and before
/// filtering**, so the statistic is not skewed by the filtering it feeds.
///
/// Meaningful only over boxes of one kind. CTD's boxes are text blocks; a
/// list of per-glyph mask components is not, and a median over it is a glyph's
/// area, against which every real block is "40x the median". Grouped text
/// uses [`median_rect_area`] over its blocks and [`block_size_verdict`].
pub fn median_box_area(boxes: &[DetBox]) -> i64 {
    median_rect_area(&boxes.iter().map(|b| b.rect).collect::<Vec<_>>())
}

/// The median area of `rects`; 0 for none.
pub fn median_rect_area(rects: &[Rect]) -> i64 {
    if rects.is_empty() {
        return 0;
    }
    let mut areas: Vec<i64> = rects.iter().map(area).collect();
    areas.sort_unstable();
    areas[areas.len() / 2]
}

/// A text block is unusually large above this multiple of the page's median
/// block area. Calibrated on grouped blocks (see [`block_size_verdict`]).
pub const BLOCK_LARGE_FACTOR: f64 = 12.0;
/// ...or taller than this share of the page or segment.
pub const BLOCK_LARGE_HEIGHT_SHARE: f64 = 0.5;
/// ...or wider than this share of it.
pub const BLOCK_LARGE_WIDTH_SHARE: f64 = 0.75;

/// The unusually-large flag for a **grouped text block**.
///
/// [`size_verdict`]'s 40x-the-median rule was calibrated on CTD's block boxes;
/// fed per-glyph SAM components its median is one glyph, which is why a demo
/// chapter flagged 90 of 1,353 records. A block's reference is the median of
/// the page's other blocks, and a block legitimately spans a quarter of a page
/// far more often than a glyph box does, so the height rule moves to half the
/// page. There is no small-block drop: a grouped block has lettering pixels
/// under it by construction.
pub fn block_size_verdict(rect: &Rect, median_block_area: i64, page_w: u32, page_h: u32) -> SizeVerdict {
    if median_block_area > 0 && area(rect) as f64 > BLOCK_LARGE_FACTOR * median_block_area as f64 {
        return SizeVerdict::FlagLarge;
    }
    if rect.h as f64 > BLOCK_LARGE_HEIGHT_SHARE * page_h as f64
        || rect.w as f64 > BLOCK_LARGE_WIDTH_SHARE * page_w as f64
    {
        return SizeVerdict::FlagLarge;
    }
    SizeVerdict::Keep
}

/// Build the tiers. `page_w`/`page_h` clamp every growth.
///
/// Merges on geometry alone. [`build_separated`] is the same builder with the
/// page's own evidence in it, and is what a caller holding a raster should use.
pub fn build(raw: Vec<DetBox>, page_w: u32, page_h: u32) -> Vec<Region> {
    build_separated(raw, page_w, page_h, |_, _| false)
}

/// [`build`], with a veto on any merge that would cross a balloon.
///
/// `separated(a, b)` answers *these two must not become one region*, and
/// [`crate::balloon::gap_is_solid`] is what answers it from the page:
///
/// ```ignore
/// let regions = build_separated(boxes, page.width, page.height, |a, b| {
///     !cleaner_core::balloon::gap_is_solid(page, &detection.segmentation, a, b)
/// });
/// ```
///
/// **Geometry cannot answer this and the veto is not a tuning knob.** Both
/// merges below are greedy over a **growing hull** - a box absorbed into the
/// current one widens the rectangle that the next box is tested against - so a
/// single detector box bridging two balloons pulls both of their text blocks
/// into one region, and no threshold on overlap fraction distinguishes that
/// from two columns of one balloon. What distinguishes them is the paper in the
/// gap: one balloon's fill, or a rim and the art beyond it.
pub fn build_separated(
    raw: Vec<DetBox>,
    page_w: u32,
    page_h: u32,
    mut separated: impl FnMut(Rect, Rect) -> bool,
) -> Vec<Region> {
    let median = median_box_area(&raw);
    let merged = merge_centre_in_box(raw, &mut separated);

    let mut tight: Vec<(TightBox, SizeVerdict, Vec<Rect>)> = merged
        .into_iter()
        .map(|(b, origins)| {
            let verdict = size_verdict(area(&b.rect), median, b.rect.h, page_h, b.confidence);
            let tight_rect = grown(b.rect, 2, 1, page_w, page_h); // +2 all sides, +3 right
            (
                TightBox {
                    tight: tight_rect,
                    extended: grown(tight_rect, 5, 0, page_w, page_h),
                    language: b.language,
                    confidence: b.confidence,
                },
                verdict,
                origins,
            )
        })
        .collect();

    // Undersized boxes go before the extended tier is built: §2 says the tiers
    // are "copy **after filtering**".
    tight.retain(|(_, verdict, _)| *verdict != SizeVerdict::Drop);
    in_reading_order(&mut tight, |(t, _, _)| t.extended);

    // Merge the extended boxes at >20% of the smaller box's area.
    let mut regions: Vec<Region> = Vec::new();
    let mut taken = vec![false; tight.len()];
    for i in 0..tight.len() {
        if taken[i] {
            continue;
        }
        let mut masking = tight[i].0.extended;
        let mut members = vec![tight[i].0.clone()];
        let mut origins = tight[i].2.clone();
        let mut flagged = tight[i].1 == SizeVerdict::FlagLarge;

        for j in (i + 1)..tight.len() {
            if taken[j] {
                continue;
            }
            let other = tight[j].0.extended;
            if !overlaps_enough(&masking, &other) {
                continue;
            }
            // Asked between the original boxes on either side, never between
            // the hulls: see `merge_centre_in_box`.
            let crosses = origins
                .iter()
                .any(|a| tight[j].2.iter().any(|b| separated(*a, *b)));
            if crosses {
                continue;
            }
            masking = hull(&masking, &other);
            members.push(tight[j].0.clone());
            origins.extend(tight[j].2.iter().copied());
            flagged |= tight[j].1 == SizeVerdict::FlagLarge;
            taken[j] = true;
        }

        regions.push(Region {
            masking,
            reference: masking.grown(20, page_w, page_h),
            members,
            flagged_large: flagged,
            group: None,
        });
    }

    in_reading_order(&mut regions, |r| r.masking);
    regions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn det(x: i64, y: i64, w: u32, h: u32) -> DetBox {
        DetBox {
            rect: Rect::new(x, y, w, h),
            confidence: 0.9,
            language: DetectedLanguage::Japanese,
        }
    }

    #[test]
    fn the_tight_box_grows_further_on_the_right() {
        // Vertical Japanese runs right-to-left, so the right edge is where the
        // next column starts. +2 all sides, +3 right.
        let regions = build(vec![det(100, 100, 40, 60)], 1000, 1000);
        let tight = regions[0].members[0].tight;
        assert_eq!(tight.x, 98);
        assert_eq!(tight.y, 98);
        assert_eq!(tight.right(), 143, "expected 140 + 2 + 1");
        assert_eq!(tight.bottom(), 162);
    }

    #[test]
    fn a_box_whose_centre_sits_in_another_is_absorbed() {
        let regions = build(vec![det(100, 100, 100, 100), det(140, 140, 40, 40)], 1000, 1000);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].members.len(), 1, "the absorb happens before the tiers");
    }

    fn det_sure(x: i64, y: i64, w: u32, h: u32, confidence: f32) -> DetBox {
        DetBox { confidence, ..det(x, y, w, h) }
    }

    #[test]
    fn a_tiny_box_beside_normal_ones_is_dropped() {
        // Median area is 10 000; 0.15 × that is 1 500, so a 20×20 box goes  - 
        // when the detector is not sure of it.
        let regions = build(
            vec![det(0, 0, 100, 100), det(300, 0, 100, 100), det_sure(600, 0, 20, 20, 0.41)],
            1000,
            1000,
        );
        assert_eq!(regions.len(), 2);
    }

    /// The seven lines of dialogue this rule was losing. A short single column
    /// (24×50 is 12.png's own box: 1 200 px² against a 1 500 px² floor, so it
    /// is under the drop line and only the confidence keeps it)
    /// in a balloon is a small box on a page of larger ones, and the detector is
    /// as sure of it as of anything else on the page.
    #[test]
    fn a_tiny_box_the_detector_is_sure_of_survives() {
        let regions = build(
            vec![det(0, 0, 100, 100), det(300, 0, 100, 100), det_sure(600, 0, 24, 50, 0.94)],
            1000,
            1000,
        );
        assert_eq!(regions.len(), 3, "a confident short column is dialogue, not speckle");
    }

    /// And the box on the same pages that really is speckle: same size, and the
    /// detector barely believed it.
    #[test]
    fn a_tiny_box_the_detector_is_unsure_of_still_drops() {
        let median = 10_000i64;
        assert_eq!(size_verdict(1_000, median, 20, 1000, 0.41), SizeVerdict::Drop);
        assert_eq!(size_verdict(1_000, median, 20, 1000, 0.94), SizeVerdict::Keep);
        // The exemption is the small-box drop's alone: a confident box that is
        // enormous is still flagged.
        assert_eq!(size_verdict(500_000, median, 20, 1000, 0.99), SizeVerdict::FlagLarge);
        assert_eq!(size_verdict(20_000, median, 300, 1000, 0.99), SizeVerdict::FlagLarge);
    }

    #[test]
    fn a_box_taller_than_a_quarter_of_the_page_is_flagged_not_dropped() {
        let regions = build(vec![det(10, 10, 60, 300), det(300, 10, 60, 80)], 1000, 1000);
        let tall = regions.iter().find(|r| r.masking.h > 200).expect("the tall box survived");
        assert!(tall.flagged_large);
    }

    #[test]
    fn overlapping_extended_boxes_become_one_region_with_both_members() {
        let regions = build(vec![det(100, 100, 60, 60), det(150, 100, 60, 60)], 1000, 1000);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].members.len(), 2);
        assert_eq!(regions[0].reference, regions[0].masking.grown(20, 1000, 1000));
    }

    #[test]
    fn the_result_does_not_depend_on_the_order_the_boxes_arrived_in() {
        let boxes = vec![det(300, 40, 60, 60), det(100, 100, 60, 60), det(150, 100, 60, 60)];
        let mut reversed = boxes.clone();
        reversed.reverse();
        assert_eq!(build(boxes, 1000, 1000), build(reversed, 1000, 1000));
    }

    /// An adopted box is grown by the builder's own margins, not by a second
    /// set that happens to match today. Built from the same rectangle, the two
    /// paths agree on every tier.
    #[test]
    fn a_box_adopted_on_its_own_is_grown_exactly_as_the_builder_grows_one() {
        let built = build(vec![det(100, 100, 40, 60)], 1000, 1000);
        let made = Region::from_box(
            Rect::new(100, 100, 40, 60),
            0.9,
            DetectedLanguage::Japanese,
            1000,
            1000,
            0,
        );
        assert_eq!(made, built[0]);
    }

    /// The share the extended tier merges on, as the predicate the adoption
    /// asks. Exactly a fifth of the smaller box is not "more than".
    #[test]
    fn the_overlap_test_is_a_share_of_the_smaller_box() {
        let big = Rect::new(0, 0, 100, 100);
        let small = Rect::new(90, 0, 10, 100); // wholly inside: 100% of itself
        assert!(overlaps_enough(&big, &small));
        let fifth = Rect::new(98, 0, 10, 100); // 2×100 of 10×100 is exactly 0.2
        assert!(!overlaps_enough(&big, &fifth));
        let more = Rect::new(97, 0, 10, 100);
        assert!(overlaps_enough(&big, &more));
        assert!(!overlaps_enough(&big, &Rect::new(200, 200, 10, 10)));
        assert!(!overlaps_enough(&big, &Rect::new(0, 0, 0, 100)));
    }

    #[test]
    fn growth_is_clamped_to_the_page() {
        let regions = build(vec![det(0, 0, 40, 40)], 100, 100);
        assert_eq!(regions[0].reference.x, 0);
        assert_eq!(regions[0].reference.y, 0);
        assert!(regions[0].reference.right() <= 100);
    }
}
