//! The pass that turns rules 1–3's geometry into something a run can hold.
//!
//! Rules 1-3's geometry was built and had no caller. This is the caller.
//! One walk over the pages, in
//! order, keeping **one page of samples and two row-strips**, and what comes
//! back is everything the clean pass needs to know about the strip before it
//! reads a single decode window:
//!
//! - which joins may be read across ([`check_join`], [`Joins`]) - and, for the
//!   ones that may not, what was wrong with them, so they can be reported;
//! - where the strip is split ([`plan_splits`]), recorded in `strip.splits`;
//! - what the detection segments are ([`detection_segments`]).
//!
//! ## The shape is rule 1's, deliberately
//!
//! Nothing here holds two pages. The join check reads [`JOIN_PROBE_ROWS`] rows
//! either side of one boundary, so the previous page survives this walk as
//! sixteen rows of luma; the row profile is one `f32` per strip row, which is
//! 3.2 MB for a 200-page webtoon. A survey of a 200-page chapter costs one page
//! of pixels at a time, and that is the whole of the memory argument rule 1
//! exists to make.
//!
//! ## Two things this does not do, named rather than left to be found
//!
//! **The mask term of rule 2's row score is not added here.** Rule 2 asks for
//! "coverage by the detector's mask from a cheap low-resolution pre-pass", and
//! there is no cheap pre-pass: the only thing in this build that can say where
//! text is, is the detector, and running it here would be a second inference
//! over every page. [`RowProfile::add_mask_coverage`] is written, tested, and
//! still has no caller. What that costs is real and bounded - a balloon's
//! interior is flat paper, so the gradient term alone can pick a row inside a
//! bubble - and it is bounded because rule 3's detection overlap is what
//! recovers a box a split cut.
//!
//! **The detection overlap stops at a page boundary.** [`detection_segments`]
//! extends a segment's detection range 900 rows into the next segment's work;
//! [`Survey`] then caps that range at the end of the page the segment starts
//! in, because the reader this build has is a page-at-a-time reader and a
//! detection range that crossed a join would need two pages resident. Inside a
//! tall page - the webtoon case, where a source is 800×12000 and rule 2 cuts it
//! into six segments - the overlap is unaffected and does its whole job. Across
//! a **page join** it does not fire, so a bubble split across one is still two
//! boxes to the detector even where [`Strip::clamp`] would let the read cross.

use crate::image::{BitDepth, ColorMode, Raster};

use super::{
    EdgeRows, JOIN_PROBE_ROWS, JoinAnomaly, Joins, RowProfile, Segment, Split, SplitKind, Strip,
    check_join, detection_segments, plan_splits,
};

/// What one walk over the pages learned.
#[derive(Debug, Clone, PartialEq)]
pub struct Survey {
    /// Rule 1's state for every join, indexed by the page above it.
    pub joins: Joins,
    /// Rule 2's plan, in strip coordinates. Empty where the trigger did not
    /// fire - a page-shaped document is not split, and its pages are its
    /// segments.
    pub splits: Vec<Split>,
    /// Rule 3's segments, cut at rule 2's splits **and** at every page
    /// boundary, with each segment's detection range capped at the end of its
    /// own page. See the module note.
    pub segments: Vec<Segment>,
}

impl Survey {
    /// Every join that failed, with the index of the page above it.
    ///
    /// "Report them", finally readable from somewhere that is not a test.
    pub fn anomalies(&self) -> impl Iterator<Item = (usize, JoinAnomaly)> + '_ {
        self.joins.anomalies()
    }

    /// The segments that begin inside the page at `placement`.
    ///
    /// The unit a page-at-a-time clean pass detects over. Segments are cut at
    /// page boundaries, so "begins inside" and "lies inside" are the same
    /// question and this is the whole of the page's detection work.
    pub fn segments_of(&self, strip: &Strip, placement: usize) -> Vec<Segment> {
        let Some(page) = strip.pages().get(placement) else { return Vec::new() };
        let (top, bottom) = (page.y_offset, page.rect().bottom());
        self.segments
            .iter()
            .copied()
            .filter(|s| (s.start as i64) >= top && (s.start as i64) < bottom)
            .collect()
    }
}

/// The rows of the page at `placement` that a segment's **detector** is run
/// over, in that page's own coordinates.
///
/// `start..detect_end`, not `start..end`: rule 3's overlap is the difference
/// between what a segment processes and what it looks at, and it is the whole
/// reason a box clipped by a split arrives whole from somewhere. Clamped to the
/// page, because [`Survey`] has already capped the detection range at the
/// page's end and this is what makes that cap arithmetic rather than a promise.
///
/// `None` where the segment and the page do not meet at all.
pub fn crop_rows(strip: &Strip, placement: usize, segment: &Segment) -> Option<(u32, u32)> {
    let page = strip.pages().get(placement)?;
    let top = (segment.start as i64 - page.y_offset).max(0);
    let bottom = (segment.detect_end as i64 - page.y_offset).min(page.height as i64);
    (bottom > top).then_some((top as u32, bottom as u32))
}

/// Walk the strip once and answer everything rules 1–3 can be asked before a
/// window is read.
///
/// `page(placement)` is the caller's decoder, by position in `strip.pages()`.
/// `None` is a page that could not be read: it leaves the joins either side of
/// it [`JoinState::Unchecked`] - which is a page edge, the conservative
/// reading - and its rows at zero in the profile, which rule 2's acceptance
/// floor already treats as "nothing has measured this" rather than as "this is
/// quiet".
///
/// The decoder is called **once per page**, in order, and the returned raster
/// is dropped before the next call.
pub fn survey(strip: &Strip, mut page: impl FnMut(usize) -> Option<Raster>) -> Survey {
    let mut joins = Joins::unchecked(strip.joins());
    let mut profile = RowProfile::zeroed(strip.height());
    let mut previous: Option<(EdgeRows, Encoding)> = None;

    for (index, placement) in strip.pages().iter().enumerate() {
        let Some(raster) = page(index) else {
            previous = None;
            continue;
        };
        profile.add_page(placement.y_offset, &raster);

        if index > 0 {
            if let Some((above, encoding)) = previous.take() {
                if encoding == Encoding::of(&raster) {
                    joins.set(index - 1, check_join(&above, &EdgeRows::top_of(&raster, JOIN_PROBE_ROWS)));
                }
            }
        }
        previous = Some((EdgeRows::bottom_of(&raster, JOIN_PROBE_ROWS), Encoding::of(&raster)));
        // Explicit, because the whole claim of this pass is that it holds one
        // page at a time and the next iteration's decode must not overlap this
        // one's samples.
        drop(raster);
    }

    let splits = plan_splits(strip, &profile);
    let segments = segments_for(strip, &splits);
    Survey { joins, splits, segments }
}

#[derive(PartialEq, Eq)]
struct Encoding {
    mode: ColorMode,
    depth: BitDepth,
    icc: Option<Vec<u8>>,
    palette: Option<Vec<u8>>,
    trns: Option<Vec<u8>>,
    srgb_intent: Option<u8>,
    color: crate::image::ColorDescription,
}

impl Encoding {
    fn of(raster: &Raster) -> Self {
        Self { mode: raster.mode, depth: raster.depth, icc: raster.icc.clone(),
            palette: raster.palette.clone(), trns: raster.trns.clone(), srgb_intent: raster.srgb_intent, color: raster.color.clone() }
    }
}

/// Rule 3's segments over rule 2's plan **and** the page boundaries.
///
/// Two things happen here and neither belongs inside
/// [`detection_segments`], which is rule 3 as the document states it, over a
/// strip:
///
/// - **Page boundaries are added to the cut list.** Rule 2's own sentence for
///   the case its trigger does not fire is "a strip that is not long is not
///   split; the pages are their own segments". Adding the joins says that once,
///   for both modes, instead of a branch: in a paginated project the plan is
///   empty and the joins are the whole cut list, and in a longstrip project the
///   joins that rule 2 did not already choose still bound a segment.
/// - **Each detection range is capped at its page's end**, because this build's
///   reader is a page at a time. See the module note.
fn segments_for(strip: &Strip, splits: &[Split]) -> Vec<Segment> {
    let mut cuts: Vec<Split> = splits.to_vec();
    cuts.extend(
        (0..strip.joins())
            .filter_map(|j| strip.join_row(j))
            .map(|y| Split { y, kind: SplitKind::Join }),
    );

    let mut segments = detection_segments(strip.height(), &cuts);
    for segment in segments.iter_mut() {
        if let Some(index) = strip.page_at(segment.start as i64) {
            let page_end = strip.pages()[index].rect().bottom() as u32;
            segment.detect_end = segment.detect_end.min(page_end);
        }
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{BitDepth, ColorMode, Raster};
    use crate::strip::{DETECTION_OVERLAP, JoinState};

    fn gray(width: u32, height: u32, data: Vec<u8>) -> Raster {
        Raster {
            width,
            height,
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

    /// Rows with structure, so the join check's content floor is cleared. The
    /// same hash `join`'s own tests use, and for the same reason: a lattice is
    /// periodic in x and every join comes back misregistered.
    fn textured(seed: u32, width: u32, height: u32) -> Vec<u8> {
        (0..height)
            .flat_map(|y| {
                (0..width).map(move |x| {
                    let mut h = x.wrapping_mul(0x9E37_79B1)
                        ^ (y + seed).wrapping_mul(0x85EB_CA6B)
                        ^ 0xC2B2_AE35;
                    h ^= h >> 15;
                    h = h.wrapping_mul(0x2545_F491);
                    h ^= h >> 13;
                    (60 + h % 160) as u8
                })
            })
            .collect()
    }

    /// A page that is quiet except for one band of texture at `loud`, so the
    /// row profile has a real median and a real minimum.
    fn quiet_with_text(width: u32, height: u32, loud: std::ops::Range<u32>) -> Raster {
        let mut data = vec![246u8; (width * height) as usize];
        for y in 0..height {
            if loud.contains(&y) {
                for x in 0..width {
                    data[(y * width + x) as usize] = if (x / 3) % 2 == 0 { 20 } else { 246 };
                }
            }
        }
        gray(width, height, data)
    }

    /// **The caller's outer half.** The check runs, the states come back, and
    /// an anomaly is readable from something that is not a test of `join.rs`.
    #[test]
    fn every_join_is_probed_and_the_broken_one_is_reported() {
        // Page 1's last eight rows are page 2's first eight - a webtoon sliced
        // with overlap, which is the one anomaly the catalogue can say.
        let shared = textured(11, 40, 8);
        let mut first = textured(0, 40, 40);
        first.truncate(40 * 32);
        first.extend_from_slice(&shared);
        let mut second = shared.clone();
        second.extend_from_slice(&textured(90, 40, 32));
        let third = textured(700, 40, 40);

        let pages = [gray(40, 40, first), gray(40, 40, second), gray(40, 40, third)];
        let strip = Strip::of_sizes(&[(40, 40), (40, 40), (40, 40)]);
        let survey = survey(&strip, |i| pages.get(i).cloned());

        assert_eq!(survey.joins.state(0), JoinState::Anomalous(JoinAnomaly::DuplicatedRows { rows: 8 }));
        assert_eq!(survey.joins.state(1), JoinState::Verified);
        let reported: Vec<(usize, JoinAnomaly)> = survey.anomalies().collect();
        assert_eq!(reported, vec![(0, JoinAnomaly::DuplicatedRows { rows: 8 })]);
        assert_eq!(reported[0].1.reason_key(), Some("notice.input.joinAnomaly"));

        // Rule 1: an anomalous join is a page edge, and a verified one is not.
        assert!(!survey.joins.is_verified(0));
        assert!(survey.joins.is_verified(1));
    }

    /// The memory claim, as a count: one decode per page, and the raster does
    /// not outlive the iteration.
    #[test]
    fn a_survey_decodes_each_page_once_and_holds_one_at_a_time() {
        let strip = Strip::of_sizes(&[(40, 40); 6]);
        let mut calls: Vec<usize> = Vec::new();
        survey(&strip, |i| {
            calls.push(i);
            Some(gray(40, 40, textured(i as u32 * 13, 40, 40)))
        });
        assert_eq!(calls, vec![0, 1, 2, 3, 4, 5], "in order, once each");
    }

    /// A page the run cannot read leaves both its joins unprobed, which rule 1
    /// treats as a page edge, and does not take the survey down with it.
    #[test]
    fn an_unreadable_page_leaves_its_joins_unchecked() {
        let strip = Strip::of_sizes(&[(40, 40); 3]);
        let survey = survey(&strip, |i| {
            (i != 1).then(|| gray(40, 40, textured(i as u32 * 13, 40, 40)))
        });
        assert_eq!(survey.joins.state(0), JoinState::Unchecked);
        assert_eq!(survey.joins.state(1), JoinState::Unchecked);
        assert_eq!(survey.anomalies().count(), 0, "unchecked is not an anomaly");
    }

    /// **Rule 2, called.** A tall source is split, and the split lands on the
    /// quiet band rather than on the target.
    #[test]
    fn a_longstrip_source_is_split_at_a_content_minimum() {
        // One 400×9000 page: longstrip-shaped, and busy everywhere except a
        // 400-row quiet band starting at 2200.
        let width = 400u32;
        let height = 9000u32;
        let mut data = textured(3, width, height);
        for y in 2200..2600 {
            for x in 0..width {
                data[(y * width + x) as usize] = 246;
            }
        }
        let page = gray(width, height, data);
        let strip = Strip::of_sizes(&[(width, height)]);
        assert!(strip.is_longstrip_shaped());

        let survey = survey(&strip, |i| (i == 0).then(|| page.clone()));
        assert!(!survey.splits.is_empty(), "rule 2 planned nothing");
        let first = survey.splits[0];
        assert_eq!(first.kind, SplitKind::Minimum, "{:?}", survey.splits);
        assert!(
            (2201..2599).contains(&first.y),
            "the split missed the quiet band: {first:?}"
        );
    }

    /// Rule 2's trigger, and rule 2's own sentence for the case it does not
    /// fire: the pages are their own segments.
    ///
    /// Two 1600×2400 pages, because the trigger is a property of the **strip**
    /// and three of them are already 0.22 - a tankoubon long enough to read is
    /// longstrip-*shaped* by rule 2's own arithmetic, and what saves it is the
    /// next test's rule rather than this one's.
    #[test]
    fn a_paginated_project_plans_no_splits_and_gets_one_segment_per_page() {
        let strip = Strip::of_sizes(&[(1600, 2400), (1600, 2400)]);
        assert!(!strip.is_longstrip_shaped());
        let survey = survey(&strip, |_| Some(quiet_with_text(1600, 2400, 100..140)));

        assert!(survey.splits.is_empty(), "a page-shaped document is not split");
        assert_eq!(survey.segments.len(), 2);
        assert_eq!(survey.segments[0].start, 0);
        assert_eq!(survey.segments[0].end, 2400);
        assert_eq!(
            survey.segments[0].detect_end, 2400,
            "the overlap does not reach into the next page"
        );
        assert_eq!(survey.segments_of(&strip, 1).len(), 1);
        assert_eq!(survey.segments_of(&strip, 1)[0].start, 2400);
    }

    /// **What keeps a tankoubon from being cut through its own pages.** Rule
    /// 2's trigger is the strip's aspect ratio, so any chapter of more than two
    /// ordinary pages fires it - and the rule that then matters is "page joins
    /// win unconditionally": the target is 2000 and a 2400 px page puts a join
    /// inside every band, so every split is a join and every segment is a page.
    ///
    /// Worth its own test because the trigger reads like a mode switch and is
    /// not one. Nothing branches on "is this a webtoon"; the plan comes out the
    /// same shape either way, for a reason rule 2 states.
    #[test]
    fn a_chapter_of_ordinary_pages_is_split_at_its_joins_and_nowhere_else() {
        let strip = Strip::of_sizes(&[(1600, 2400); 6]);
        assert!(strip.is_longstrip_shaped(), "0.11 - rule 2's trigger is the strip's shape");
        let survey = survey(&strip, |_| Some(quiet_with_text(1600, 2400, 100..140)));

        let joins: Vec<u32> = (0..strip.joins()).filter_map(|j| strip.join_row(j)).collect();
        let taken: Vec<u32> = survey.splits.iter().map(|s| s.y).collect();
        for row in &joins {
            assert!(taken.contains(row), "join at {row} was not taken: {taken:?}");
        }
        // Every split before the last join is a join. The one after it is the
        // tail of the last page, which is 2400 rows long and therefore over the
        // target with no join left to prefer - rule 2 cuts it, and that is the
        // rule rather than an exception to it.
        let last_join = *joins.last().unwrap();
        for split in survey.splits.iter().filter(|s| s.y <= last_join) {
            assert_eq!(split.kind, SplitKind::Join, "{split:?}");
        }
        for page in 0..5 {
            assert_eq!(survey.segments_of(&strip, page).len(), 1, "page {page}");
        }
    }

    /// **Rule 3's overlap, where this build does reach it.** Inside one tall
    /// source, a segment looks 900 rows past its own work.
    #[test]
    fn inside_a_tall_page_the_detection_overlap_is_rule_threes_own() {
        let strip = Strip::of_sizes(&[(800, 12_000)]);
        let survey = survey(&strip, |_| Some(quiet_with_text(800, 12_000, 30..70)));
        assert!(survey.segments.len() > 1, "{:?}", survey.segments);

        let first = survey.segments[0];
        assert_eq!(first.detect_end - first.end, DETECTION_OVERLAP);
        assert_eq!(first.detect_end, survey.segments[1].start + DETECTION_OVERLAP);
        // …and the last segment has nowhere to reach.
        let last = survey.segments[survey.segments.len() - 1];
        assert_eq!(last.end, 12_000);
        assert_eq!(last.detect_end, 12_000);
    }

    /// The cap, stated as the thing it costs. Two 2000 px pages in a longstrip
    /// project: the segment above the join looks at its own page and no further,
    /// so a box clipped by the join is not recovered by the overlap.
    #[test]
    fn the_detection_overlap_stops_at_a_page_boundary() {
        let strip = Strip::of_sizes(&[(400, 2000), (400, 2000), (400, 2000), (400, 2000)]);
        assert!(strip.is_longstrip_shaped());
        let survey = survey(&strip, |_| Some(quiet_with_text(400, 2000, 30..70)));

        for segment in &survey.segments {
            let page = strip.page_at(segment.start as i64).expect("a segment starts on a page");
            let page_end = strip.pages()[page].rect().bottom() as u32;
            assert!(
                segment.detect_end <= page_end,
                "{segment:?} looks past the end of page {page}"
            );
        }
        // Every page boundary is a segment boundary, whatever rule 2 chose.
        let starts: Vec<u32> = survey.segments.iter().map(|s| s.start).collect();
        for join in 0..strip.joins() {
            let row = strip.join_row(join).unwrap();
            assert!(starts.contains(&row), "join at {row} is not a segment start: {starts:?}");
        }
    }

    /// The crop the detector is actually handed, which is what makes rule 3's
    /// two ranges a fact about pixels rather than about a struct.
    #[test]
    fn a_segments_crop_is_its_detection_range_in_the_pages_own_rows() {
        let strip = Strip::of_sizes(&[(800, 4_000), (800, 12_000)]);
        let survey = survey(&strip, |_| None);
        // Page 0 is 4000 rows and one page-join split, so it is 4000/2000 = two
        // segments even on an empty profile - the fallback, which is the honest
        // outcome for a profile nothing filled.
        let first = survey.segments_of(&strip, 0);
        assert!(!first.is_empty());
        let (top, bottom) = crop_rows(&strip, 0, &first[0]).unwrap();
        assert_eq!(top, 0);
        assert_eq!(bottom, (first[0].detect_end).min(4_000), "capped at the page");

        // Page 1 begins at strip row 4000; its segments' crops are page-local.
        let second = survey.segments_of(&strip, 1);
        assert!(second.len() > 1, "{second:?}");
        let (top, bottom) = crop_rows(&strip, 1, &second[1]).unwrap();
        assert_eq!(top as i64, second[1].start as i64 - 4_000);
        assert_eq!(bottom as i64, second[1].detect_end as i64 - 4_000);
        assert!(bottom <= 12_000);

        // A segment of another page does not crop this one.
        assert_eq!(crop_rows(&strip, 0, &second[1]), None);
        assert_eq!(crop_rows(&strip, 9, &second[0]), None);
    }

    /// An empty strip answers rather than panicking, like everything else in
    /// this module's neighbours.
    #[test]
    fn an_empty_strip_surveys_to_nothing() {
        let strip = Strip::of_sizes(&[]);
        let survey = survey(&strip, |_| None);
        assert!(survey.splits.is_empty());
        assert!(survey.joins.is_empty());
        assert_eq!(survey.segments_of(&strip, 0).len(), 0);
    }
}
