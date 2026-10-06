//! Rule 8's other half: a patch that spans a join, written into two files.
//!
//! Rule 1 settles the pair of decisions this module implements one of:
//!
//! > Resolution: read across verified joins, split the **patch** at export, and
//! > require the two halves to share a fill tone with no visible seam.
//!
//! and rule 8 states the consequence for the passthrough test:
//!
//! > A patch spanning a join is split at the page boundary, and the passthrough
//! > test uses per-page **intersection** with the global patch set, not patch
//! > ownership.
//!
//! The fidelity contract says the same thing from its own side, and it is
//! the sentence that explains why ownership is not merely a worse answer
//! but a wrong one: a patch
//! that spans a join **belongs to no single page**. Ask "which page owns this
//! patch" and one of the two pages it writes into answers "none of mine", is
//! declared untouched, and is copied byte-for-byte - while the other page's
//! file carries half a bubble. The page's file is then bit-identical to its
//! source and *wrong*, which is the one failure the fidelity contract cannot
//! see, because the contract is an upper bound on what may change and a
//! passthrough changes nothing.
//!
//! ## How the two sides are made to agree
//!
//! By never computing anything. [`StripPatch::split_at_joins`] is a **copy**:
//! every sample of every part is one sample of the strip-coordinate patch,
//! moved. There is no resampling, no re-fit, no per-page ring statistic and no
//! second call to an engine, so there is no arithmetic that could round one way
//! above the join and the other way below it. The fill tone is shared because
//! it is the *same tone*, written twice.
//!
//! The copy is per **sample** rather than per row, and that is load-bearing
//! rather than cautious. Pages are centred (rule 1), so a page narrower than
//! the strip sits at an x offset - and at 1, 2 or 4 bits per sample an odd
//! offset puts the page's first sample in the middle of a byte. A row-wise
//! `copy_from_slice` would shift every sample on the narrower page by a
//! fraction of a byte, which is a corruption that looks like a colour shift.

use crate::image::Raster;
use crate::mask::{Mask, Rect};
use crate::patch::Patch;
use crate::strip::Strip;

/// A patch whose mask and pixels are in **strip** coordinates.
///
/// A separate type rather than a convention, because a rectangle that
/// changes coordinate systems and nothing in the type system saying so cost
/// every region on every page its position, silently. A [`Patch`] is in page
/// coordinates everywhere else in this crate; this wrapper is the only thing
/// that is not, and it can only be unwrapped by splitting it back onto pages.
#[derive(Debug, Clone)]
pub struct StripPatch {
    patch: Patch,
}

impl StripPatch {
    /// Take a patch already measured in strip coordinates - what rule 3's
    /// global box list produces, and what a window that read across a verified
    /// join yields.
    pub fn of(patch: Patch) -> StripPatch {
        StripPatch { patch }
    }

    /// Lift a patch anchored on one page into strip coordinates.
    ///
    /// `position` is the page's index in [`Strip::pages`], not its index in the
    /// project's source list - the two differ whenever `strip.order` reorders
    /// pages. `None` when the strip has no page there.
    ///
    /// A patch that lies wholly inside its anchor page lifts and splits back to
    /// itself, which is why a single-page project can be routed through this
    /// path without changing what it produces.
    pub fn lift(strip: &Strip, position: usize, patch: &Patch) -> Option<StripPatch> {
        let bounds = patch.mask.bounds;
        let (x, y) = strip.to_strip(position, bounds.x, bounds.y)?;
        let mut lifted = patch.clone();
        lifted.mask.bounds.x = x;
        lifted.mask.bounds.y = y;
        // The ink has a rectangle of its own, moved by the same offset.
        lifted.ink.bounds.x += x - bounds.x;
        lifted.ink.bounds.y += y - bounds.y;
        Some(StripPatch { patch: lifted })
    }

    /// The patch as it stands, in strip coordinates. Named so that a caller
    /// reaching for the mask has to say which space it wanted.
    pub fn in_strip_coordinates(&self) -> &Patch {
        &self.patch
    }

    /// Rule 8's split: the parts of this patch that fall on each page it
    /// covers, in **page** coordinates, paired with the page's position in the
    /// strip.
    ///
    /// The parts are returned in strip order, and a patch that touches one page
    /// returns one part. Every returned part is well-formed in the sense
    /// [`Patch::is_well_formed`] means - its buffer covers its mask - so the
    /// compositor accepts it without a second thought.
    ///
    /// The part keeps the whole patch's `id`. Two files carrying two halves of
    /// one edit are one edit, and its provenance - the engine, the parameters,
    /// the `mask_sha256` of the applied mask - describes the whole of it. A
    /// derived per-part id would make the two halves look like two independent
    /// patches that happen to abut, which is the reading this module exists to
    /// prevent.
    pub fn split_at_joins(&self, strip: &Strip) -> Vec<(usize, Patch)> {
        strip
            .split_at_joins(self.patch.mask.bounds)
            .into_iter()
            .map(|(position, part)| (position, self.copy_into(strip, position, part)))
            .collect()
    }

    /// The part of this patch on one page, or `None` if it does not reach that
    /// page.
    ///
    /// Separate from [`StripPatch::split_at_joins`] so that
    /// [`patches_on_page`] can ask about one page without building - and then
    /// throwing away - the parts for every other page. The rectangles come from
    /// the same [`Strip::split_at_joins`] call either way, so the two cannot
    /// disagree about where the boundary is.
    pub fn part_on(&self, strip: &Strip, position: usize) -> Option<Patch> {
        strip
            .split_at_joins(self.patch.mask.bounds)
            .into_iter()
            .find(|(page, _)| *page == position)
            .map(|(page, part)| self.copy_into(strip, page, part))
    }

    /// The copy itself. `part` is in page coordinates; every sample it takes is
    /// addressed back through the strip, so a page's own x offset is applied
    /// exactly once and in one place.
    fn copy_into(&self, strip: &Strip, position: usize, part: Rect) -> Patch {
        let source = &self.patch;
        let strip_bounds = source.mask.bounds;
        let samples = source.pixels.mode.samples();

        let mut mask = Mask::empty(part);
        let mut pixels = Raster {
            width: part.w,
            height: part.h,
            mode: source.pixels.mode,
            depth: source.pixels.depth,
            icc: None,
            palette: source.pixels.palette.clone(),
            trns: source.pixels.trns.clone(),
            srgb_intent: None,
            color: Default::default(),
            data: vec![0; {
                let bits = part.w as usize * samples * source.pixels.depth.bits() as usize;
                bits.div_ceil(8) * part.h as usize
            }],
        };

        for y in part.y..part.bottom() {
            for x in part.x..part.right() {
                // Page coordinates out, strip coordinates in. The two differ in
                // x as well as y whenever this page is narrower than the strip.
                let Some((sx, sy)) = strip.to_strip(position, x, y) else { continue };
                mask.set_coverage(x, y, source.mask.coverage(sx, sy));
                let (from_x, from_y) = ((sx - strip_bounds.x) as u32, (sy - strip_bounds.y) as u32);
                let (to_x, to_y) = ((x - part.x) as u32, (y - part.y) as u32);
                for channel in 0..samples {
                    pixels.set_sample(
                        to_x,
                        to_y,
                        channel,
                        source.pixels.sample(from_x, from_y, channel),
                    );
                }
            }
        }

        // The ink is cut by the same rule from its own rectangle, which is
        // usually inside the mask's but is not required to be. A part the ink
        // does not reach carries an empty one.
        let ink = strip
            .split_at_joins(source.ink.bounds)
            .into_iter()
            .find(|(page, _)| *page == position)
            .map(|(_, ink_part)| {
                let mut ink = Mask::empty(ink_part);
                for y in ink_part.y..ink_part.bottom() {
                    for x in ink_part.x..ink_part.right() {
                        let Some((sx, sy)) = strip.to_strip(position, x, y) else { continue };
                        ink.bits[(y-ink_part.y) as usize*ink_part.w as usize+(x-ink_part.x) as usize] = source.ink.bits[(sy-source.ink.bounds.y) as usize*source.ink.bounds.w as usize+(sx-source.ink.bounds.x) as usize];
                    }
                }
                ink
            })
            .unwrap_or_else(|| Mask::empty(Rect::new(part.x, part.y, 0, 0)));

        Patch {
            id: source.id.clone(),
            mask,
            ink,
            pixels,
            order: source.order,
            visible: source.visible,
            provenance: source.provenance.clone(),
        }
    }
}

/// **The per-page intersection**, which is the test the fidelity contract
/// requires and the reason this function exists at all:
///
/// > In longstrip, "zero applied patches" is decided by per-page
/// > **intersection** with the global patch set, not by patch ownership, since
/// > a patch may span a join.
///
/// So the answer for a page is computed from *every* patch in the global set,
/// and no patch is ever consulted about which page it belongs to. An empty
/// result is a page nothing writes into, and that page - and only that page -
/// is a byte-for-byte passthrough.
///
/// A part whose mask has no set pixel is kept rather than filtered here.
/// Whether an all-zero mask counts as an applied patch is already decided, once,
/// in [`crate::export::export_page`], which treats it as untouched; deciding it
/// a second time here is how the two answers start to differ.
pub fn patches_on_page(strip: &Strip, position: usize, global: &[StripPatch]) -> Vec<Patch> {
    global.iter().filter_map(|patch| patch.part_on(strip, position)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{BitDepth, ColorMode};
    use crate::mask::Rect;
    use crate::patch::{Engine, Provenance};

    fn provenance() -> Provenance {
        Provenance {
            engine: Engine::Fill,
            engine_version: "test".into(),
            model_sha256: None,
            execution_provider: "cpu".into(),
            params_snapshot: serde_json::json!({}),
            mask_sha256: "0".repeat(64),
            source_sha256: "0".repeat(64),
            cloud: None,
            created: 0,
        }
    }

    /// A patch in strip coordinates whose every sample is a distinct, position-
    /// derived value, so a part that came from the wrong place is visible.
    fn strip_patch(bounds: Rect, mode: ColorMode, depth: BitDepth) -> StripPatch {
        let samples = mode.samples();
        let mut pixels = Raster {
            width: bounds.w,
            height: bounds.h,
            mode,
            depth,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None,
            color: Default::default(),
            data: vec![0; {
                let bits = bounds.w as usize * samples * depth.bits() as usize;
                bits.div_ceil(8) * bounds.h as usize
            }],
        };
        let ceiling = (1u32 << depth.bits()) - 1;
        for y in 0..bounds.h {
            for x in 0..bounds.w {
                for channel in 0..samples {
                    let value = (x * 31 + y * 7 + channel as u32 * 3) % ceiling;
                    pixels.set_sample(x, y, channel, value as u16);
                }
            }
        }
        StripPatch::of(Patch {
            id: "p1".into(),
            mask: Mask::filled(bounds),
            ink: Mask::filled(bounds),
            pixels,
            order: 0,
            visible: true,
            provenance: provenance(),
        })
    }

    fn strip() -> Strip {
        // A 700 px page between two 800 px ones: rule 1's gutter case, and the
        // reason a part's x differs from the strip's x on the middle page.
        Strip::of_sizes(&[(800, 1000), (700, 1000), (800, 1000)])
    }

    /// Every sample of every part is a sample of the whole, moved - which is
    /// what makes the two sides of a join agree without anything reconciling
    /// them.
    #[test]
    fn each_part_carries_the_samples_the_whole_patch_had_there() {
        let strip = strip();
        let whole = strip_patch(Rect::new(100, 900, 400, 300), ColorMode::Rgb, BitDepth::Eight);
        let parts = whole.split_at_joins(&strip);

        assert_eq!(parts.len(), 2, "a patch across one join has two parts");
        assert_eq!(parts[0].0, 0);
        assert_eq!(parts[1].0, 1);

        let source = whole.in_strip_coordinates();
        for (position, part) in &parts {
            assert!(part.is_well_formed());
            for y in part.mask.bounds.y..part.mask.bounds.bottom() {
                for x in part.mask.bounds.x..part.mask.bounds.right() {
                    let (sx, sy) = strip.to_strip(*position, x, y).unwrap();
                    let from = (
                        (sx - source.mask.bounds.x) as u32,
                        (sy - source.mask.bounds.y) as u32,
                    );
                    let to = ((x - part.mask.bounds.x) as u32, (y - part.mask.bounds.y) as u32);
                    for channel in 0..3 {
                        assert_eq!(
                            part.pixels.sample(to.0, to.1, channel),
                            source.pixels.sample(from.0, from.1, channel),
                            "page {position} at ({x}, {y}) channel {channel}"
                        );
                    }
                    assert_eq!(part.mask.contains(x, y), source.mask.contains(sx, sy));
                }
            }
        }
    }

    /// The seam itself: the last row page 0 gets and the first row page 1 gets
    /// are adjacent rows of one patch, and they are still adjacent rows of one
    /// patch after the split.
    #[test]
    fn the_two_sides_of_a_join_meet_with_no_step() {
        let strip = strip();
        let whole = strip_patch(Rect::new(100, 900, 400, 300), ColorMode::Gray, BitDepth::Eight);
        let parts = whole.split_at_joins(&strip);
        let (_, above) = &parts[0];
        let (_, below) = &parts[1];

        assert_eq!(above.mask.bounds.bottom(), 1000, "page 0 ends at its own last row");
        assert_eq!(below.mask.bounds.y, 0, "page 1 starts at its own first row");

        let source = whole.in_strip_coordinates();
        for column in 0..above.mask.bounds.w {
            let last = above.pixels.sample(column, above.mask.bounds.h - 1, 0);
            let first = below.pixels.sample(column, 0, 0);
            // Strip rows 999 and 1000, which is what the two are.
            assert_eq!(last, source.pixels.sample(column, 999 - 900, 0));
            assert_eq!(first, source.pixels.sample(column, 1000 - 900, 0));
        }
    }

    /// A page narrower than the strip is centred, so the same strip column is a
    /// different page column - and at 4 bits per sample an odd offset starts
    /// mid-byte. A row-wise copy shifts every sample here; a per-sample one
    /// does not.
    #[test]
    fn a_sub_byte_page_at_an_odd_offset_keeps_its_samples() {
        // 800 - 794 = 6, centred: an x offset of 3.
        let strip = Strip::of_sizes(&[(800, 100), (794, 100)]);
        assert_eq!(strip.pages()[1].x_offset, 3, "the offset this test is about");

        let whole = strip_patch(Rect::new(10, 60, 40, 80), ColorMode::Indexed, BitDepth::Four);
        let parts = whole.split_at_joins(&strip);
        assert_eq!(parts.len(), 2);

        let source = whole.in_strip_coordinates();
        let (_, below) = &parts[1];
        assert_eq!(below.mask.bounds.x, 7, "strip x 10 is page x 7 on the inset page");
        for y in 0..below.mask.bounds.h {
            for x in 0..below.mask.bounds.w {
                assert_eq!(
                    below.pixels.sample(x, y, 0),
                    source.pixels.sample(x, y + 40, 0),
                    "({x}, {y}) moved by a fraction of a byte"
                );
            }
        }
    }

    /// The ink has a rectangle of its own and crosses the join by the same
    /// rule: lifted by the page's offset, cut at the boundary, and absent from
    /// a part it does not reach.
    #[test]
    fn the_ink_is_lifted_and_split_by_its_own_rectangle() {
        let strip = strip();
        // Anchored on page 1, which is inset 50 px and starts at strip y 1000.
        let bounds = Rect::new(10, 900, 200, 200);
        let mut patch = strip_patch(bounds, ColorMode::Gray, BitDepth::Eight).patch;
        // Lettering in the top-left corner of the mask, entirely on the anchor.
        patch.ink = Mask::filled(Rect::new(20, 910, 30, 40));

        let lifted = StripPatch::lift(&strip, 1, &patch).unwrap();
        let in_strip = lifted.in_strip_coordinates();
        assert_eq!(in_strip.mask.bounds, Rect::new(60, 1900, 200, 200));
        assert_eq!(in_strip.ink.bounds, Rect::new(70, 1910, 30, 40), "moved by the same offset");

        let parts = lifted.split_at_joins(&strip);
        assert_eq!(parts.len(), 2, "the mask spans the join into page 2");
        let (on_page_1, on_page_2) = (&parts[0].1, &parts[1].1);
        assert_eq!(on_page_1.ink, patch.ink, "back on its own page, the ink is unchanged");
        assert!(on_page_2.ink.is_empty(), "the ink never reached page 2");
        assert_eq!(on_page_2.ink.bounds.w, 0);
    }

    /// The rule this module is named for. Ownership says page 1 has no patch;
    /// intersection says it has part of one, and the part writes pixels.
    #[test]
    fn a_page_a_patch_does_not_belong_to_still_intersects_it() {
        let strip = strip();
        // Anchored on page 0 and running 200 px past its bottom edge.
        let whole = strip_patch(Rect::new(100, 900, 400, 300), ColorMode::Gray, BitDepth::Eight);
        let global = [whole];

        let on_page_0 = patches_on_page(&strip, 0, &global);
        let on_page_1 = patches_on_page(&strip, 1, &global);
        let on_page_2 = patches_on_page(&strip, 2, &global);

        assert_eq!(on_page_0.len(), 1);
        assert_eq!(on_page_1.len(), 1, "the page the patch does not belong to");
        assert!(on_page_2.is_empty(), "a page the patch never reaches");
        assert!(!on_page_1[0].mask.is_empty(), "the part writes nothing");
        assert_eq!(on_page_1[0].mask.bounds.h, 200);
    }

    /// A patch inside one page lifts and splits back to itself, byte for byte.
    /// This is what makes it safe to route a project that has no joins through
    /// the same path.
    #[test]
    fn a_patch_inside_one_page_is_unchanged_by_the_round_trip() {
        let strip = strip();
        let original = strip_patch(Rect::new(10, 20, 40, 30), ColorMode::Rgb, BitDepth::Eight)
            .in_strip_coordinates()
            .clone();

        let lifted = StripPatch::lift(&strip, 0, &original).expect("page 0 exists");
        let parts = lifted.split_at_joins(&strip);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].0, 0);
        assert_eq!(parts[0].1.mask, original.mask);
        assert_eq!(parts[0].1.pixels.data, original.pixels.data);
        assert_eq!(parts[0].1.id, original.id);
    }

    /// Lifting is by the page's position in the strip, and the position moves
    /// the rectangle by that page's own offsets - both of them.
    #[test]
    fn lifting_uses_the_pages_own_offsets_and_refuses_a_page_that_is_not_there() {
        let strip = strip();
        let patch = strip_patch(Rect::new(10, 20, 40, 30), ColorMode::Gray, BitDepth::Eight)
            .in_strip_coordinates()
            .clone();

        let lifted = StripPatch::lift(&strip, 1, &patch).unwrap();
        assert_eq!(lifted.in_strip_coordinates().mask.bounds.x, 60, "10 + the 50 px inset");
        assert_eq!(lifted.in_strip_coordinates().mask.bounds.y, 1020);
        assert!(StripPatch::lift(&strip, 9, &patch).is_none());
    }
}
