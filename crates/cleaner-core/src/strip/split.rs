//! Rule 2: split at content minima, with an acceptance floor.
//!
//! Rule 2, in four sentences:
//!
//! > target 2000 px, tolerance ±500 widening to ±1000 then ±1500, trigger at a
//! > strip aspect below 0.33.
//!
//! > **Score rows by edge/gradient energy, not ink density.** Ink density has
//! > the wrong sign on inverted content - white text on a black panel is a
//! > *low*-ink row, so an ink-minimising splitter actively prefers to cut
//! > through it - and on a solid black panel every row saturates and the argmin
//! > is noise.
//!
//! > **Minimum is not zero.** The band's argmin is accepted only if its score
//! > falls below 10% of the page median; otherwise widen, then fall back to a
//! > hard split at exactly 2000 px, recorded in `strip.splits` and surfaced in
//! > review. The record is `kind: "fallback"`.
//!
//! > **Page joins win unconditionally.**
//!
//! The profile is one `f32` per strip row, accumulated **one page at a time**
//! ([`RowProfile::add_page`]). A 200-page webtoon is 800 000 rows, which is
//! 3.2 MB of profile and one page of pixels at a time - the rule 1 shape, not a
//! full-strip buffer.

use crate::detect::Segmentation;
use crate::image::Raster;

use super::{SPLIT_ACCEPT_FRACTION, SPLIT_TARGET, SPLIT_TOLERANCES, Strip};

/// How heavily a row known to cross text is penalised.
///
/// Rule 2: "Add coverage by the detector's mask from a cheap low-resolution
/// pre-pass, so a row **crossing known text is never chosen**." Never, not
/// rarely - so the penalty is not a tuned weight but a number large against the
/// quantity it is added to. A row score is a mean absolute gradient in 0–255
/// levels, so it cannot exceed 510; a row 1% covered by text already scores
/// above any row that is not.
pub const TEXT_ROW_PENALTY: f32 = 1000.0;

/// Why a split is where it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SplitKind {
    /// A page boundary inside the tolerance band. Rule 2 takes it and skips the
    /// search: two ~2000 px pages give a guaranteed text-free split for free.
    Join,
    /// The band's argmin, having cleared the acceptance floor.
    Minimum,
    /// No row in any band cleared the floor, so the split is a hard cut at
    /// exactly [`SPLIT_TARGET`] past the previous one.
    ///
    /// This is the honest outcome rule 2 requires instead of taking the
    /// least-bad row silently. Boxes it cuts are recovered by rule 3's
    /// detection overlap, and it is surfaced in review.
    Fallback,
}

/// One split row, in **strip** coordinates.
///
/// Rule 2 records a fallback as `kind: "fallback"`, which is what this
/// serialises to. Earlier revisions of the rule named a `split_fallback: true`
/// boolean; the enum carries the same bit and two more, so a reader of a
/// manifest can tell a join from a found minimum, which the bool cannot say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Split {
    pub y: u32,
    pub kind: SplitKind,
}

impl Split {
    pub fn is_fallback(&self) -> bool {
        self.kind == SplitKind::Fallback
    }
}

/// One score per strip row: rule 2's edge/gradient energy, plus the detector's
/// mask coverage.
#[derive(Debug, Clone, PartialEq)]
pub struct RowProfile {
    scores: Vec<f32>,
}

impl RowProfile {
    pub fn zeroed(height: u32) -> RowProfile {
        RowProfile { scores: vec![0.0; height as usize] }
    }

    /// For tests and for a caller that has the numbers already.
    pub fn from_scores(scores: Vec<f32>) -> RowProfile {
        RowProfile { scores }
    }

    pub fn scores(&self) -> &[f32] {
        &self.scores
    }

    /// Score one page's rows into the profile at its strip offset.
    ///
    /// **Gradient magnitude, not ink.** The quantity is the mean over the row
    /// of `|∂L/∂y| + |∂L/∂x|` in 0–255 levels, which is sign-invariant: white
    /// text on black scores exactly as high as black text on white, and a solid
    /// panel of either scores zero on both.
    /// One row of luma is kept from the previous iteration rather than the page
    /// being sampled twice per pixel, so a 200-page strip costs one pass and
    /// two rows.
    pub fn add_page(&mut self, y_offset: i64, page: &Raster) {
        let mut previous: Option<Vec<f32>> = None;
        for y in 0..page.height {
            let row: Vec<f32> =
                (0..page.width).map(|x| page.luma16_at(x, y) as f32 / 257.0).collect();
            let strip_row = y_offset + y as i64;
            if strip_row >= 0 && (strip_row as usize) < self.scores.len() {
                let mut total: f32 = row.windows(2).map(|w| (w[1] - w[0]).abs()).sum();
                if let Some(above) = &previous {
                    total += row.iter().zip(above).map(|(a, b)| (a - b).abs()).sum::<f32>();
                }
                self.scores[strip_row as usize] += total / page.width.max(1) as f32;
            }
            previous = Some(row);
        }
    }

    /// Add the detector's mask coverage, so a row crossing known text is never
    /// chosen.
    ///
    /// The segmentation is whatever the caller has - rule 2 asks for "a cheap
    /// low-resolution pre-pass", and [`Segmentation`] is already resampled to
    /// the page it came from, so this needs no scale factor.
    pub fn add_mask_coverage(&mut self, y_offset: i64, seg: &Segmentation) {
        for y in 0..seg.height {
            let row = y_offset + y as i64;
            if row < 0 || row as usize >= self.scores.len() {
                continue;
            }
            let covered = (0..seg.width).filter(|x| seg.is_text(*x, y)).count();
            self.scores[row as usize] +=
                TEXT_ROW_PENALTY * covered as f32 / seg.width.max(1) as f32;
        }
    }

    fn median_over(&self, from: u32, to: u32) -> f32 {
        let from = from as usize;
        let to = (to as usize).min(self.scores.len());
        if to <= from {
            return 0.0;
        }
        let mut window: Vec<f32> = self.scores[from..to].to_vec();
        window.sort_by(f32::total_cmp);
        window[window.len() / 2]
    }

    fn argmin_over(&self, from: u32, to: u32) -> Option<(u32, f32)> {
        let to = (to as usize).min(self.scores.len()) as u32;
        if to <= from {
            return None;
        }
        (from..to)
            .map(|y| (y, self.scores[y as usize]))
            // `<` rather than `<=`, so the **first** row of a tied run wins and
            // two runs over the same profile agree.
            .reduce(|best, next| if next.1 < best.1 { next } else { best })
    }
}

/// Rule 2's whole plan, in strip coordinates.
///
/// Returns nothing at all for a strip the trigger does not fire on: a
/// page-shaped document is not split, its pages are its segments.
pub fn plan_splits(strip: &Strip, profile: &RowProfile) -> Vec<Split> {
    if !strip.is_longstrip_shaped() {
        return Vec::new();
    }

    let height = strip.height();
    let mut out: Vec<Split> = Vec::new();
    let mut last = 0u32;

    while last + SPLIT_TARGET < height {
        let target = last + SPLIT_TARGET;
        let mut chosen: Option<Split> = None;

        for tolerance in SPLIT_TOLERANCES {
            let low = target.saturating_sub(tolerance).max(last + 1);
            // The band is `target ± tolerance` **inclusive**, expressed as an
            // exclusive upper bound. Both ends, because a page join lands on
            // `target + tolerance` exactly whenever the pages are a round
            // multiple of the target - which is the common case for a webtoon
            // sliced at 2000, and the case rule 2's "joins win" exists for.
            let high = (target + tolerance).saturating_add(1).min(height);
            if low >= high {
                continue;
            }

            // Joins win unconditionally, and are looked for at every widening
            // rather than only the first: a join two thousand rows away is
            // still a guaranteed text-free split, and rule 2's reason for
            // preferring it does not weaken with distance.
            if let Some(join) = join_in(strip, low, high, target) {
                chosen = Some(Split { y: join, kind: SplitKind::Join });
                break;
            }

            let Some((y, score)) = profile.argmin_over(low, high) else { continue };
            // A median of zero is not a quiet page, it is a profile that was
            // never filled: `score <= 0.10 × 0` is then satisfied by the 0.0
            // argmin, and the plan comes back as a full set of minima at
            // `target − tolerance` with nothing behind them. The floor is a
            // *relative* test and it has nothing to be relative to, so there is
            // no acceptance to be had - the same conservatism as
            // [`crate::strip::Joins::unchecked`], which refuses to call a join
            // verified until something has looked at it.
            let median = page_median(strip, profile, y);
            if median > 0.0 && score <= SPLIT_ACCEPT_FRACTION * median {
                chosen = Some(Split { y, kind: SplitKind::Minimum });
                break;
            }
        }

        let split = chosen.unwrap_or(Split { y: target, kind: SplitKind::Fallback });
        out.push(split);
        last = split.y;
    }

    out
}

/// The median row score of the page that owns `y`. Rule 2's floor is relative
/// to "the page median", and on a strip the page is the unit that has one - the
/// strip's own median mixes a title page with a dense action sequence.
fn page_median(strip: &Strip, profile: &RowProfile, y: u32) -> f32 {
    match strip.page_at(y as i64) {
        Some(index) => {
            let page = strip.pages()[index];
            profile.median_over(page.y_offset as u32, (page.y_offset + page.height as i64) as u32)
        }
        None => profile.median_over(0, profile.scores.len() as u32),
    }
}

/// The join in the band, nearest to `target`.
///
/// Rule 2 says a join inside the band wins and does not say which join wins
/// when two are inside it. Nearest-to-target is the reading the rest of the
/// rule supports: the target exists to keep segments near 2000 px, and the
/// tolerance is how far the search may stray from it, not a preference for
/// straying. Taking the first join in ascending order is a preference for
/// `target − tolerance`, which is the furthest point in the band that is still
/// allowed.
///
/// Ties go to the lower row, so two joins equidistant either side of the target
/// resolve the same way on every run.
fn join_in(strip: &Strip, low: u32, high: u32, target: u32) -> Option<u32> {
    (0..strip.joins())
        .filter_map(|j| strip.join_row(j))
        .filter(|row| *row >= low && *row < high)
        .min_by_key(|row| ((*row as i64 - target as i64).abs(), *row))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{BitDepth, ColorMode, Raster};

    fn strip_of(pages: usize, height: u32) -> Strip {
        Strip::of_sizes(&vec![(800, height); pages])
    }

    /// A profile that is quiet everywhere except where told otherwise, so the
    /// page median is a real number rather than zero.
    fn busy(height: u32) -> RowProfile {
        RowProfile::from_scores((0..height).map(|y| 40.0 + (y % 7) as f32).collect())
    }

    /// Rule 2's headline: the split lands on a content minimum, not on the
    /// midpoint.
    #[test]
    fn a_split_lands_on_the_quiet_row_and_not_on_the_target() {
        let strip = strip_of(1, 12_000);
        let mut profile = busy(12_000);
        profile.scores[2300] = 1.0; // well under 10% of a median near 43
        let splits = plan_splits(&strip, &profile);
        assert_eq!(splits[0], Split { y: 2300, kind: SplitKind::Minimum });
    }

    /// **Minimum is not zero.** A band whose flattest row is still busy has no
    /// acceptable split, and rule 2 requires the honest fallback rather than the
    /// least-bad row.
    #[test]
    fn a_band_with_no_quiet_row_falls_back_rather_than_taking_the_argmin() {
        let strip = strip_of(1, 12_000);
        let mut profile = busy(12_000);
        // Lower than everything around it, and nowhere near a tenth of the
        // median: exactly the "least-bad bubble row" rule 2 refuses.
        profile.scores[2100] = 20.0;
        let splits = plan_splits(&strip, &profile);
        assert_eq!(splits[0], Split { y: 2000, kind: SplitKind::Fallback });
        assert!(splits[0].is_fallback());
    }

    /// The widening band. A quiet row 900 rows from the target is outside ±500
    /// and inside ±1000.
    #[test]
    fn the_band_widens_before_it_gives_up() {
        let strip = strip_of(1, 12_000);
        let mut profile = busy(12_000);
        profile.scores[2900] = 1.0;
        assert_eq!(plan_splits(&strip, &profile)[0], Split { y: 2900, kind: SplitKind::Minimum });
    }

    /// Page joins win unconditionally, even against a row that would have been
    /// accepted.
    #[test]
    fn a_join_inside_the_band_wins_over_a_quiet_row() {
        let strip = strip_of(6, 2100); // joins at 2100, 4200, …
        let mut profile = busy(12_600);
        profile.scores[1900] = 0.0;
        let splits = plan_splits(&strip, &profile);
        assert_eq!(splits[0], Split { y: 2100, kind: SplitKind::Join });
    }

    /// Two joins inside one band. Rule 2 does not say which wins, and the
    /// target is what says: it exists to keep segments near 2000 px, so the
    /// join nearest it is the one taken. Ascending order takes the join nearest
    /// `target − tolerance` instead, which is the furthest row the band allows.
    #[test]
    fn the_join_taken_is_the_one_nearest_the_target_and_not_the_first_in_the_band() {
        // Joins at 1600 and 2100, both inside 1500..=2500.
        let strip = Strip::of_sizes(&[(800, 1_600), (800, 500), (800, 10_000)]);
        assert_eq!((strip.join_row(0), strip.join_row(1)), (Some(1_600), Some(2_100)));
        let splits = plan_splits(&strip, &busy(12_100));
        assert_eq!(splits[0], Split { y: 2_100, kind: SplitKind::Join });
    }

    /// **An empty profile is not a quiet strip.** Every median is zero, so the
    /// acceptance floor `score <= 0.10 × 0` is met by the 0.0 argmin and every
    /// band accepts its first row - a full plan of minima at `target − 500`,
    /// each one asserting that a row nothing has measured is content-free.
    #[test]
    fn a_profile_that_was_never_filled_produces_no_minima_at_all() {
        let strip = strip_of(1, 12_000);
        let splits = plan_splits(&strip, &RowProfile::zeroed(12_000));
        let rows: Vec<u32> = splits.iter().map(|s| s.y).collect();
        assert_eq!(rows, vec![2_000, 4_000, 6_000, 8_000, 10_000]);
        assert!(splits.iter().all(|s| s.kind == SplitKind::Fallback), "{splits:?}");
    }

    /// The other side of the same test: a page that really is blank scores zero
    /// everywhere too, and is treated the same way. Nothing distinguishes the
    /// two from the profile alone, and the fallback is the honest outcome for
    /// both - a hard cut at exactly the target, surfaced in review, rather than
    /// a minimum claiming evidence it does not have.
    #[test]
    fn a_join_still_wins_inside_a_band_of_an_empty_profile() {
        let strip = strip_of(6, 2_100);
        let splits = plan_splits(&strip, &RowProfile::zeroed(12_600));
        assert_eq!(splits[0], Split { y: 2_100, kind: SplitKind::Join });
    }

    /// Rule 2's trigger. A page-shaped document is not split at all.
    #[test]
    fn a_page_shaped_document_is_not_split() {
        let page = Strip::of_sizes(&[(1600, 2400), (1600, 2400)]);
        assert!(plan_splits(&page, &busy(4800)).is_empty());
        assert!((crate::strip::LONGSTRIP_ASPECT - 0.33).abs() < 1e-6);
    }

    /// The plan covers the whole strip and never runs backwards.
    #[test]
    fn splits_are_ascending_and_stop_before_the_end() {
        let strip = strip_of(1, 9_500);
        let splits = plan_splits(&strip, &busy(9_500));
        assert_eq!(splits.iter().map(|s| s.y).collect::<Vec<_>>(), vec![2000, 4000, 6000, 8000]);
        assert!(splits.iter().all(|s| s.y < 9_500));
    }

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

    /// Rule 2's reason for scoring energy rather than ink: **white text on a
    /// black panel is a low-ink row**, so an ink-minimising splitter prefers to
    /// cut through it. Energy gives both polarities the same score.
    #[test]
    fn the_score_is_sign_invariant_between_white_on_black_and_black_on_white() {
        let (w, h) = (32u32, 5u32);
        let mut light = vec![240u8; (w * h) as usize];
        let mut dark = vec![15u8; (w * h) as usize];
        for x in 8..24 {
            light[(2 * w + x) as usize] = 15; // dark text on light paper
            dark[(2 * w + x) as usize] = 240; // light text on dark paper
        }

        let mut a = RowProfile::zeroed(h);
        a.add_page(0, &gray(w, h, light));
        let mut b = RowProfile::zeroed(h);
        b.add_page(0, &gray(w, h, dark));
        assert!((a.scores()[2] - b.scores()[2]).abs() < 1e-3);
        assert!(a.scores()[2] > a.scores()[0] * 10.0, "the text row is the loud one");

        // …and a solid panel of either polarity scores zero, where an
        // ink-density argmin would be pure noise.
        let mut flat = RowProfile::zeroed(h);
        flat.add_page(0, &gray(w, h, vec![15u8; (w * h) as usize]));
        assert!(flat.scores().iter().all(|s| *s < 1e-6));
    }

    /// The mask term. A row the detector says carries text is never chosen,
    /// however flat it looks.
    #[test]
    fn a_row_the_detector_calls_text_is_never_the_minimum() {
        let strip = strip_of(1, 12_000);
        let mut profile = busy(12_000);
        profile.scores[2100] = 0.0;

        let plain = plan_splits(&strip, &profile);
        assert_eq!(plain[0].y, 2100, "without the mask term the flat row wins");

        // A balloon's interior is flat paper: zero energy, and full of text.
        profile.scores[2100] += TEXT_ROW_PENALTY * 0.4;
        let guarded = plan_splits(&strip, &profile);
        assert_ne!(guarded[0].y, 2100);
        assert_eq!(guarded[0], Split { y: 2000, kind: SplitKind::Fallback });
    }

    /// The mask coverage arithmetic itself, against a real `Segmentation`.
    #[test]
    fn mask_coverage_is_added_in_proportion_to_the_row_it_covers() {
        let fit = crate::detect::Letterbox {
            scale: 1.0,
            fitted_w: 10,
            fitted_h: 4,
            page_w: 10,
            page_h: 4,
        };
        let mut levels = vec![0u8; 40];
        for x in 0..5 {
            levels[10 + x] = 255; // half of row 1 is text
        }
        let seg = Segmentation { width: 10, height: 4, levels, fit };
        let mut profile = RowProfile::zeroed(4);
        profile.add_mask_coverage(0, &seg);
        assert!((profile.scores()[1] - TEXT_ROW_PENALTY * 0.5).abs() < 1e-3);
        assert_eq!(profile.scores()[0], 0.0);
    }
}
