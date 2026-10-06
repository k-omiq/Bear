//! Longstrip geometry: the virtual stitch, the split plan, and the global box
//! list.
//!
//! Longstrip rules 1, 2 and 3. One module rather than three, because the
//! three rules share one coordinate system and separating them is how a box
//! ends up measured in one space and owned in another.
//!
//! The rule that shapes everything here is rule 1's first sentence:
//!
//! > A `Strip` is an ordered list of `SourceRef`s with cumulative offsets - a
//! > coordinate system, not a buffer.
//!
//! So [`Strip`] holds page **placements** and no pixels at all. Nothing in this
//! module takes a whole strip's worth of samples: the join check reads a few
//! rows either side of one boundary ([`join`]), the split plan accumulates one
//! `f32` per strip row from one page at a time ([`split`]), and the box merge
//! works on rectangles ([`segment`]).
//!
//! ## Rules 4 to 6, and what they call
//!
//! [`window`] is rule 4 - the per-box decode window, every term native.
//! [`stream`] is rule 6 - one window in flight, results streamed out.
//! [`survey`] is the caller rules 1-3's geometry needed: one
//! walk over the pages that probes every join with [`check_join`], accumulates
//! rule 2's row profile a page at a time, and answers with the [`Joins`], the
//! [`plan_splits`] plan and the [`detection_segments`] segments a clean pass
//! needs before it reads anything.
//!
//! `src-tauri/src/run.rs` calls [`survey`] once per job, emits the one join
//! anomaly the catalogue has a sentence for, and writes the plan into the
//! manifest's `strip.splits`; the run's pipeline detects per segment, decides
//! ownership on [`merge_global`]'s global list, and bounds every read with
//! [`window::decode_window`], which ends in [`Strip::clamp`].
//!
//! **What still has no caller**, so that no "is" here is read as more than it
//! says: [`RowProfile::add_mask_coverage`], because rule 2's "cheap
//! low-resolution pre-pass" does not exist and the detector is the only thing
//! that knows where text is. The detection overlap is capped at a page boundary
//! by [`Survey`] - see that module's note for what that costs.
//!
//! [`Strip::split_at_joins`] is rule 8's half and belongs to the streaming
//! export, which calls it through `cleaner_core::export::split`.
//!
//! ## The three constants that are not in [`crate::constants`]
//!
//! These are not the table [`crate::constants`] holds. They are kept beside
//! the code that reads them because every one of them is a *geometry* number
//! whose justification is a sentence in rule 2 or rule 3 rather than a
//! measurement on a page.
//!
//! No tile dimension is defined here. Rule 7's preview tile size is fixed by
//! **WebView2 on Windows**, which has never been executed in this repository,
//! and the macOS figure does not transfer. A constant named here would be
//! that figure wearing a general name.

use crate::ingest::SourceRef;
use crate::mask::Rect;

pub mod join;
pub mod raster;
pub mod segment;
pub mod split;
pub mod stream;
pub mod survey;
pub mod window;

pub use join::{JOIN_PROBE_ROWS, EdgeRows, JoinAnomaly, JoinState, Joins, check_join};
pub use raster::{WindowRaster, read_window, read_window_borrowing};
pub use segment::{DetectedInSegment, GlobalBox, Segment, detection_segments, merge_global};
pub use split::{RowProfile, Split, SplitKind, plan_splits};
pub use stream::{stream_windows, windows_in_flight};
pub use survey::{Survey, crop_rows, survey};
pub use window::{DecodeWindow, EdgePad, EngineContext, decode_window, decode_window_padded};

/// Rule 2's target segment height, in strip pixels.
pub const SPLIT_TARGET: u32 = 2000;

/// Rule 2's tolerance band, widening on each failed attempt. The last entry is
/// the last chance before the hard fallback.
pub const SPLIT_TOLERANCES: [u32; 3] = [500, 1000, 1500];

/// Rule 2's acceptance floor: the band's argmin is taken only if its score
/// falls below this fraction of the owning page's median row score.
///
/// **Minimum is not zero.** A band that contains nothing but a bubble has an
/// argmin, and without the floor the splitter cuts the least-bad row of that
/// bubble.
pub const SPLIT_ACCEPT_FRACTION: f32 = 0.10;

/// Rule 2's trigger: `width / height` below this is a strip worth splitting.
pub const LONGSTRIP_ASPECT: f32 = 0.33;

/// Rule 3's detection overlap, in strip pixels - "≥ the largest expected box
/// height".
///
/// It is for **detection only**; no pixels are processed twice. It exists
/// because the IoU dedupe below cannot fire without it: with disjoint segments
/// a clipped bubble is a top half in one segment and a bottom half in the next,
/// two *disjoint* boxes with IoU = 0, and both produce a half-evidence patch.
pub const DETECTION_OVERLAP: u32 = 900;

/// Rule 3 step 1: two boxes whose facing edges both lie within this of the same
/// split line are the same box, cut.
pub const SPLIT_EDGE_TOLERANCE: i64 = 8;

/// Rule 3 step 2.
pub const DEDUPE_IOU: f32 = 0.5;

/// Rule 3 step 3, and the same fraction [`crate::detect`] merges extended boxes
/// at on a single page.
pub const MERGE_AREA_FRACTION: f32 = 0.2;

/// One page's place in the strip. Rule 1's `(source_ref, y_offset, x_offset)`,
/// with the page's own dimensions alongside so the owning rectangle can be
/// answered without opening anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// Index into the caller's source list - `sources` in the manifest, not
    /// a position in `strip.order`.
    pub source_idx: usize,
    pub x_offset: i64,
    pub y_offset: i64,
    pub width: u32,
    pub height: u32,
}

impl Placement {
    /// The owning page rectangle, in strip coordinates. This is what rule 1
    /// clamps to.
    pub fn rect(&self) -> Rect {
        Rect::new(self.x_offset, self.y_offset, self.width, self.height)
    }
}

/// The virtual stitch.
///
/// **Not a buffer, at any point, including export** (rule 1). The type holds
/// `Vec<Placement>` and three numbers; the largest thing it can be asked for is
/// a rectangle.
///
/// Pages are **centred**, so a 700 px page in an 800 px strip has 50 px of
/// gutter on each side. Which side the gutter falls on is arbitrary; that there
/// *is* one, and that no ring, mask or engine crop may sample it, is rule 1.
///
/// This is a different type from [`crate::project::Strip`], which is the
/// manifest record - `mode`, `order` and `splits`. That one is what is written
/// to disk; this one is the coordinate system it describes, and it is rebuilt
/// from `sources` and `order` on open rather than persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Strip {
    pages: Vec<Placement>,
    width: u32,
    height: u32,
}

impl Strip {
    /// From page dimensions in reading order. `source_idx` is the position in
    /// the list, which is the identity mapping a caller with no reordering
    /// wants.
    pub fn of_sizes(sizes: &[(u32, u32)]) -> Strip {
        Strip::place(sizes.iter().copied().enumerate())
    }

    /// From the manifest's `sources` and `strip.order`. An index in `order`
    /// that is not a source is skipped rather than panicking: `order` is
    /// persisted and a manifest is an input.
    pub fn of(sources: &[SourceRef], order: &[usize]) -> Strip {
        Strip::place(
            order
                .iter()
                .filter_map(|i| sources.get(*i).map(|s| (*i, (s.width, s.height)))),
        )
    }

    fn place(pages: impl Iterator<Item = (usize, (u32, u32))>) -> Strip {
        let pages: Vec<(usize, (u32, u32))> = pages.collect();
        let width = pages.iter().map(|(_, (w, _))| *w).max().unwrap_or(0);
        let mut y = 0i64;
        let mut placed = Vec::with_capacity(pages.len());
        for (source_idx, (w, h)) in pages {
            placed.push(Placement {
                source_idx,
                x_offset: (width as i64 - w as i64) / 2,
                y_offset: y,
                width: w,
                height: h,
            });
            y += h as i64;
        }
        Strip { pages: placed, width, height: y.max(0) as u32 }
    }

    pub fn pages(&self) -> &[Placement] {
        &self.pages
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// How many joins there are: one fewer than the pages, and zero for an
    /// empty strip.
    pub fn joins(&self) -> usize {
        self.pages.len().saturating_sub(1)
    }

    /// The strip row a join sits on - the first row of the page below it.
    pub fn join_row(&self, join: usize) -> Option<u32> {
        self.pages.get(join + 1).map(|p| p.y_offset as u32)
    }

    /// Rule 2's trigger. A strip that is not long is not split; the pages are
    /// their own segments.
    pub fn is_longstrip_shaped(&self) -> bool {
        self.height > 0 && (self.width as f32 / self.height as f32) < LONGSTRIP_ASPECT
    }

    /// Which page owns a strip row.
    pub fn page_at(&self, y: i64) -> Option<usize> {
        if y < 0 {
            return None;
        }
        self.pages
            .iter()
            .position(|p| y >= p.y_offset && y < p.y_offset + p.height as i64)
    }

    /// Strip coordinates to page coordinates, for the page at `index`.
    pub fn to_page(&self, index: usize, x: i64, y: i64) -> Option<(i64, i64)> {
        let p = self.pages.get(index)?;
        Some((x - p.x_offset, y - p.y_offset))
    }

    /// Page coordinates to strip coordinates.
    pub fn to_strip(&self, index: usize, x: i64, y: i64) -> Option<(i64, i64)> {
        let p = self.pages.get(index)?;
        Some((x + p.x_offset, y + p.y_offset))
    }

    /// **Rule 1's clamp**, as a function. Rule 1 says every ring, mask and
    /// engine crop is clamped this way, and this is the arithmetic that does
    /// it: every one of them is cut from a [`window::decode_window`], which
    /// ends here.
    ///
    /// Two halves, and they are not the same rule:
    ///
    /// - **The gutter is never touched.** Horizontally the result is clamped to
    ///   the *intersection* of the x-ranges of every page the rectangle spans,
    ///   so a window crossing from an 800 px page onto a 700 px one keeps out of
    ///   the narrower page's gutter as well as the wider one's. Without it a box
    ///   near the edge of a 700 px page in an 800 px strip samples gutter: a
    ///   white gutter drags the median light, a black one makes the ring bimodal
    ///   so every edge box routes to the inpainter.
    /// - **Reads may cross a verified join.** Vertically the result is clamped
    ///   to the run of pages reachable from the anchor page through *verified*
    ///   joins, because "a bubble split across a join must be read as one
    ///   region". An unverified join is treated as a page edge, which is rule
    ///   1's own sentence and is why this takes [`Joins`] rather than a bool.
    ///
    /// A rectangle anchored outside the strip comes back empty rather than
    /// clamped to the nearest page: there is no owning page, so there is no
    /// rectangle to clamp to.
    pub fn clamp(&self, rect: Rect, joins: &Joins) -> Rect {
        let Some(anchor) = self.page_at(rect.y).or_else(|| self.page_at(rect.bottom() - 1)) else {
            return Rect::new(rect.x, rect.y, 0, 0);
        };

        let mut lo = anchor;
        let mut hi = anchor;
        while lo > 0 && rect.y < self.pages[lo].y_offset && joins.is_verified(lo - 1) {
            lo -= 1;
        }
        while hi + 1 < self.pages.len()
            && rect.bottom() > self.pages[hi].y_offset + self.pages[hi].height as i64
            && joins.is_verified(hi)
        {
            hi += 1;
        }

        let run = &self.pages[lo..=hi];
        let left = run.iter().map(|p| p.x_offset).max().unwrap_or(0);
        let right = run.iter().map(|p| p.rect().right()).min().unwrap_or(0);
        let top = run[0].y_offset;
        let bottom = run[run.len() - 1].rect().bottom();

        let x0 = rect.x.max(left);
        let y0 = rect.y.max(top);
        let x1 = rect.right().min(right);
        let y1 = rect.bottom().min(bottom);
        Rect::new(x0, y0, (x1 - x0).max(0) as u32, (y1 - y0).max(0) as u32)
    }

    /// Rule 8's per-page export split: the parts of `rect` that belong to each
    /// page it covers, in **page** coordinates.
    ///
    /// > A patch spanning a join is split at the page boundary.
    ///
    /// Which is the other half of the same decision as [`Strip::clamp`]: the
    /// read crosses the join and the write does not.
    pub fn split_at_joins(&self, rect: Rect) -> Vec<(usize, Rect)> {
        let mut out = Vec::new();
        for (index, page) in self.pages.iter().enumerate() {
            let page_rect = page.rect();
            let x0 = rect.x.max(page_rect.x);
            let y0 = rect.y.max(page_rect.y);
            let x1 = rect.right().min(page_rect.right());
            let y1 = rect.bottom().min(page_rect.bottom());
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            out.push((
                index,
                Rect::new(x0 - page.x_offset, y0 - page.y_offset, (x1 - x0) as u32, (y1 - y0) as u32),
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip() -> Strip {
        // A 700 px page between two 800 px ones: the gutter case rule 1 names.
        Strip::of_sizes(&[(800, 1000), (700, 1000), (800, 1000)])
    }

    #[test]
    fn a_strip_is_offsets_and_holds_no_pixels() {
        let strip = strip();
        assert_eq!(strip.width(), 800);
        assert_eq!(strip.height(), 3000);
        assert_eq!(strip.pages()[1].y_offset, 1000);
        assert_eq!(strip.pages()[1].x_offset, 50, "the narrow page is centred");
        assert_eq!(strip.page_at(1500), Some(1));
        assert_eq!(strip.page_at(3000), None, "one past the end is off the strip");
        assert_eq!(strip.to_page(1, 60, 1200), Some((10, 200)));
        assert_eq!(strip.to_strip(1, 10, 200), Some((60, 1200)));
    }

    /// Rule 1: "The gutter beside a narrow page is never touched."
    ///
    /// Without the clamp this window reaches x = 0..800 on a page that occupies
    /// 50..750, and 100 px of the ring is gutter.
    #[test]
    fn a_window_on_a_narrow_page_never_reaches_the_gutter() {
        let strip = strip();
        let window = Rect::new(0, 1100, 800, 200);
        let clamped = strip.clamp(window, &Joins::unchecked(strip.joins()));
        assert_eq!(clamped.x, 50);
        assert_eq!(clamped.right(), 750);
    }

    /// Rule 1's second half: clamping is about the gutter, not about page
    /// boundaries. A bubble split across a **verified** join is read as one
    /// region.
    #[test]
    fn a_read_crosses_a_verified_join_and_stops_at_an_unverified_one() {
        let strip = strip();
        let window = Rect::new(100, 900, 400, 300); // 900..1200, across join 0

        let mut joins = Joins::unchecked(strip.joins());
        joins.set(0, JoinState::Verified);
        let crossed = strip.clamp(window, &joins);
        assert_eq!(crossed.bottom(), 1200, "the read stopped at the page edge");
        // …and the narrower page below still bounds the gutter.
        assert_eq!(crossed.right(), 500);

        let stopped = strip.clamp(window, &Joins::unchecked(strip.joins()));
        assert_eq!(stopped.bottom(), 1000, "an unverified join is a page edge");
    }

    /// Rule 8: the read crosses the join, the write does not.
    #[test]
    fn a_patch_spanning_a_join_is_split_at_the_page_boundary() {
        let strip = strip();
        let parts = strip.split_at_joins(Rect::new(100, 900, 400, 300));
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], (0, Rect::new(100, 900, 400, 100)));
        // Page 1 is inset by 50, so the same strip x is 50 on the page.
        assert_eq!(parts[1], (1, Rect::new(50, 0, 400, 200)));
    }

    #[test]
    fn the_trigger_is_the_strips_aspect_ratio() {
        assert!(Strip::of_sizes(&[(800, 4000), (800, 4000)]).is_longstrip_shaped());
        assert!(!Strip::of_sizes(&[(1600, 2400)]).is_longstrip_shaped());
        // 800 / 2400 = 0.333…, just over the trigger.
        assert!(!Strip::of_sizes(&[(800, 2400)]).is_longstrip_shaped());
    }

    /// `strip.order` is a list of positions into `sources`, and it is held
    /// separately precisely because the two can differ - a user reorders pages
    /// without the sources moving. The placement must carry the *source* index,
    /// not the position.
    #[test]
    fn the_order_decides_the_stacking_and_the_source_index_survives_it() {
        let source = |width, height| SourceRef {
            orientation: Default::default(),
            path: std::path::PathBuf::from("p.png"),
            width,
            height,
            mode: crate::image::ColorMode::Gray,
            bit_depth: crate::image::BitDepth::Eight,
            icc_bytes: None,
            sha256: String::new(),
            converted_from: None,
            conversion: None,
        };
        let sources = [source(800, 1000), source(700, 500), source(800, 200)];
        let strip = Strip::of(&sources, &[2, 0]);
        assert_eq!(strip.pages().len(), 2);
        assert_eq!(strip.pages()[0].source_idx, 2);
        assert_eq!(strip.pages()[1].source_idx, 0);
        assert_eq!(strip.height(), 1200);
        assert_eq!(strip.pages()[1].y_offset, 200);
        // An index no source answers to is dropped rather than panicking: the
        // order is persisted, and a manifest is an input.
        assert_eq!(Strip::of(&sources, &[9]).pages().len(), 0);
    }

    #[test]
    fn an_empty_strip_answers_rather_than_panicking() {
        let empty = Strip::of_sizes(&[]);
        assert_eq!(empty.height(), 0);
        assert_eq!(empty.joins(), 0);
        assert!(!empty.is_longstrip_shaped());
        assert_eq!(empty.clamp(Rect::new(0, 0, 10, 10), &Joins::unchecked(0)).w, 0);
    }
}
