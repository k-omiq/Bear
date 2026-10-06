//! Rule 4: decode windows are per-box, in native units.
//!
//! Rule 4:
//!
//! ```text
//! window = box
//!        ∪ reference_padding(20 px native)
//!        ∪ growth(26 px proxy × k → native)
//!        ∪ inpaint_radius(≤20 px native)
//!        ∪ isolation(5 px native)
//!        ∪ engine_context(512² for LaMa; tier size for cloud)
//!        clamped to the owning page rectangle, joins excepted
//! ```
//!
//! > **Every term is native.** Revision 2 wrote `growth(≤26)` - a *proxy*
//! > number. At k=3 that is 78 native px and at k=4 it is 104, so any window
//! > sized at 26 underran the actual grown mask on exactly the large scans the
//! > proxy exists for.
//!
//! > The segment is a **scheduling** unit; the decode window is a
//! > **correctness** unit. They differ, and that is intended.
//!
//! ## The terms are concentric, and rule 4 listed them as alternatives
//!
//! Read as a set union of six rectangles all anchored on the box, the formula
//! answers `max(20, 26k, 20, 5)`, which is `26k` for every k ≥ 1. That is
//! short of what the pipeline reads and writes, because **three of the terms
//! are measured from the grown mask rather than from the box**:
//!
//! | term | anchored on | reaches |
//! |---|---|---|
//! | `growth` | the seed mask, which is inside the box | `26k` |
//! | the ring [`crate::fit::ring`] samples | the grown mask | `26k + 4` |
//! | `isolation` ([`ISOLATION_RADIUS`]) | the grown mask | `26k + 5` |
//! | the edit margin ([`EDIT_MARGIN`]) | the grown mask | `26k + 5` |
//! | `inpaint_radius` | the grown mask | `26k + 20` |
//!
//! So the halo this module computes is `max(20, 26k + 20)`, and the union
//! reading is short by 20 px on all four sides at every scale the pipeline ever
//! runs at. A window short of the ring is not a cosmetic defect: the annulus is
//! clipped on the side that ran out, the median moves with whatever the clip
//! removed, and rung 0 fills the region with a tone measured from three sides.
//! The fix was the same shape as an earlier correction - a margin derived
//! from terms whose *composition* was not the one the code performs - one
//! rule further out.
//!
//! ## What is native and what is proxy, in one place
//!
//! [`GROWTH_PROXY`] is the **only** proxy-unit quantity here, and it is
//! multiplied by `k` before it meets anything else. `k` is
//! [`crate::detect::Segmentation::proxy_scale`] - how many native pixels one
//! proxy pixel covers - so `k` is above 1 for every page larger than 1024 on
//! its long edge, which is every page the pipeline is written for.

use crate::constants::{ANNULUS_WIDTH, EDIT_MARGIN, ISOLATION_RADIUS, MASK_GROWTH_STEP, MASK_GROWTH_STEPS, MIN_MASK_THICKNESS};
use crate::mask::Rect;

use super::{Joins, Strip};

/// Rule 4's reference padding, in native pixels. The same 20 px the
/// `reference_boxes` tier is grown by: the base cutout rings are sampled
/// from, and the paste origin.
pub const REFERENCE_PADDING: u32 = 20;

/// Rule 4's growth term, in **proxy** pixels - the whole candidate ladder,
/// which is cumulative: the first step is `min_mask_thickness` and eleven more
/// add `mask_growth_step` each.
///
/// This is the one number in this module that is not native, and it is never
/// used without being multiplied by `k`.
pub const GROWTH_PROXY: u32 = MIN_MASK_THICKNESS + MASK_GROWTH_STEP * MASK_GROWTH_STEPS;

/// Rule 4's `inpaint_radius(<=20 px native)`, and the
/// `min(20, 5 + int(0.1 × std))` ceiling. The formula reaches it only at
/// std ≈ 150, so it is near-dead in
/// practice - but a decode window is a bound and a bound is taken at the
/// ceiling.
pub const INPAINT_RADIUS_MAX: u32 = 20;

/// The fixed spatial input LaMa takes: the model's spatial input is **fixed at
/// 512²**.
pub const ENGINE_INPUT: u32 = 512;

/// Rule 4's `engine_context` term: how much context the engine that will run
/// this region needs around it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineContext {
    /// Rungs 0 and 1. Arithmetic over the page's own pixels, with no model and
    /// therefore no fixed input: the context term contributes nothing and the
    /// window is the halo alone.
    None,
    /// LaMa: a fixed 512² crop, whatever the box's size.
    Local,
    /// Cloud, at the tier's own pixel dimensions.
    Tier { w: u32, h: u32 },
}

impl EngineContext {
    fn size(self) -> Option<(u32, u32)> {
        match self {
            EngineContext::None => None,
            EngineContext::Local => Some((ENGINE_INPUT, ENGINE_INPUT)),
            EngineContext::Tier { w, h } => Some((w, h)),
        }
    }
}

/// How a window that ran out of page was filled.
///
/// > **Page-edge windows** are asymmetric and undersized for a fixed-512 model.
/// > Pad with **edge-replicate**, or a constant at the ring median. Never
/// > reflection - revision 2 permitted it at page edges while its own reversal
/// > table condemned it. Record which was used in `params_snapshot`.
///
/// There is deliberately no `Reflect`. A variant that cannot be constructed is
/// a variant nobody can select by accident, and the rule is absolute rather
/// than a default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgePad {
    /// The window fitted inside its page run. Nothing was padded.
    None,
    /// Edge-replicate, rule 4's first choice.
    Replicate,
    /// A constant at the ring median, rule 4's second.
    Constant,
}

impl EdgePad {
    /// The word that goes into `params_snapshot`. Not an i18n key - the
    /// snapshot is "the thresholds, dilations and radii actually used",
    /// read by a developer and by a re-run, not shown to a user.
    pub fn as_str(self) -> &'static str {
        match self {
            EdgePad::None => "none",
            EdgePad::Replicate => "edge-replicate",
            EdgePad::Constant => "ring-median",
        }
    }
}

/// The mask growth term, in **native** pixels, at scale `k`.
///
/// Rounded up rather than to nearest: the window is a bound on a read, and a
/// bound rounded down is a bound that is sometimes wrong.
pub fn growth(proxy_scale: f32) -> u32 {
    (GROWTH_PROXY as f32 * proxy_scale.max(0.0)).ceil() as u32
}

/// How far, in native pixels, anything the pipeline does reaches **past the
/// grown mask**.
///
/// The largest of the three terms rule 4 lists after `growth` plus the ring the
/// fit samples, because every one of them is measured from the grown mask and
/// not from the box. See the module note.
pub fn beyond_the_mask() -> u32 {
    INPAINT_RADIUS_MAX.max(ISOLATION_RADIUS).max(EDIT_MARGIN).max(ANNULUS_WIDTH)
}

/// Rule 4's halo around the box, in **native** pixels, at scale `k`.
///
/// `max(reference_padding, growth(k) + beyond_the_mask())`. The `max` is the
/// union rule 4 writes; the `+` inside it is the composition the union reading
/// misses.
pub fn halo(proxy_scale: f32) -> u32 {
    REFERENCE_PADDING.max(growth(proxy_scale) + beyond_the_mask())
}

/// One region's decode window: what to read, what was asked for, and how the
/// difference is filled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeWindow {
    /// What may actually be read, in **strip** coordinates: rule 4's formula
    /// after [`Strip::clamp`].
    pub rect: Rect,
    /// What the formula asked for before the clamp. Kept because the two differ
    /// exactly where rule 4's page-edge padding applies, and a caller that only
    /// has the clamped rectangle cannot tell an edge window from an interior
    /// one.
    pub requested: Rect,
    /// [`EdgePad::None`] when the two above are equal.
    pub pad: EdgePad,
}

impl DecodeWindow {
    /// Whether the clamp took anything - a page-edge or gutter window.
    pub fn is_padded(&self) -> bool {
        self.pad != EdgePad::None
    }
}

/// Rule 4, whole.
///
/// `boxr` is the region's masking box in **strip** coordinates; `proxy_scale`
/// is `k`. The result is clamped through [`Strip::clamp`], which is rule 1's
/// two halves: x to the intersection of the x-ranges of every page the window
/// spans, so the gutter beside a narrow page is never touched, and y to the run
/// of pages reachable through **verified** joins, so a bubble split across a
/// verified join is read as one region and an unverified one is a page edge.
pub fn decode_window(
    strip: &Strip,
    joins: &Joins,
    boxr: Rect,
    proxy_scale: f32,
    context: EngineContext,
) -> DecodeWindow {
    decode_window_padded(strip, joins, boxr, proxy_scale, context, EdgePad::Replicate)
}

/// [`decode_window`], naming the padding to record when the clamp takes
/// something. Rule 4 permits edge-replicate or a constant at the ring median
/// and nothing else; the choice belongs to the caller that knows whether it has
/// a ring median yet.
pub fn decode_window_padded(
    strip: &Strip,
    joins: &Joins,
    boxr: Rect,
    proxy_scale: f32,
    context: EngineContext,
    pad: EdgePad,
) -> DecodeWindow {
    let mut requested = expanded(boxr, halo(proxy_scale));
    if let Some((w, h)) = context.size() {
        requested = union(requested, centred_on(boxr, w, h));
    }
    let rect = strip.clamp(requested, joins);
    let pad = if rect == requested { EdgePad::None } else { pad };
    DecodeWindow { rect, requested, pad }
}

/// The box grown by `by` on every side, with no page clamp - the clamp is
/// [`Strip::clamp`]'s job and it is not the same rectangle.
fn expanded(rect: Rect, by: u32) -> Rect {
    let by = by as i64;
    Rect::new(rect.x - by, rect.y - by, rect.w + 2 * by as u32, rect.h + 2 * by as u32)
}

/// A `w × h` rectangle centred on `rect`, never smaller than `rect` itself: a
/// sound effect wider than 512 is tiled, not shrunk to fit the model.
fn centred_on(rect: Rect, w: u32, h: u32) -> Rect {
    let w = w.max(rect.w);
    let h = h.max(rect.h);
    let x = rect.x + rect.w as i64 / 2 - w as i64 / 2;
    let y = rect.y + rect.h as i64 / 2 - h as i64 / 2;
    Rect::new(x, y, w, h)
}

fn union(a: Rect, b: Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = a.right().max(b.right());
    let bottom = a.bottom().max(b.bottom());
    Rect::new(x, y, (right - x) as u32, (bottom - y) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Letterbox, Segmentation};
    use crate::strip::JoinState;

    fn strip() -> Strip {
        // The gutter case rule 1 names: a 700 px page between two 800 px ones.
        Strip::of_sizes(&[(800, 4000), (700, 4000), (800, 4000)])
    }

    /// **Rule 4's headline, and §8's revision-2 row.** `growth(≤26)` is a proxy
    /// number in a native formula: at k=3 the mask reaches 78 native pixels and
    /// at k=4 it reaches 104, so a window sized at 26 underran the grown mask on
    /// exactly the large scans the proxy exists for.
    #[test]
    fn every_term_is_native_and_the_growth_term_scales_with_k() {
        assert_eq!(GROWTH_PROXY, 26, "the whole cumulative ladder, in proxy px");
        assert_eq!(growth(1.0), 26);
        assert_eq!(growth(3.0), 78);
        assert_eq!(growth(4.0), 104);
        // A fractional k is rounded **up**: a window is a bound on a read.
        assert_eq!(growth(2.34), 61, "26 × 2.34 = 60.84");

        // The proxy-unit reading, stated as the number it would produce, so the
        // test fails rather than drifts if the units are mixed again.
        for k in [2.0f32, 3.0, 4.0] {
            assert!(
                halo(k) > GROWTH_PROXY + beyond_the_mask(),
                "k={k}: the window was sized in proxy pixels"
            );
        }
    }

    /// **The correction this test pins.** Rule 4 lists `growth`, `inpaint_radius`,
    /// `isolation` and the ring as terms of one union, and three of them are
    /// measured from the *grown mask* rather than from the box. A union of six
    /// rectangles all anchored on the box answers `max(20, 26k, 20, 5)`, and
    /// every one of the reads and writes below lands outside it.
    #[test]
    fn the_window_covers_what_the_pipeline_actually_reaches_past_the_grown_mask() {
        for k in [1.0f32, 2.34, 4.0] {
            let union_reading =
                REFERENCE_PADDING.max(growth(k)).max(INPAINT_RADIUS_MAX).max(ISOLATION_RADIUS);
            assert!(halo(k) > union_reading, "k={k}: the union reading is not a bound");

            // Each of the four, from the far side of the grown mask.
            assert!(halo(k) >= growth(k) + ANNULUS_WIDTH, "the ring the fit samples");
            assert!(halo(k) >= growth(k) + ISOLATION_RADIUS, "the isolation cut");
            assert!(halo(k) >= growth(k) + EDIT_MARGIN, "the edit margin");
            assert!(halo(k) >= growth(k) + INPAINT_RADIUS_MAX, "the inpaint radius");
        }
        // And the reference padding is what governs where there is no growth at
        // all, which is the manual-mask case.
        assert_eq!(halo(0.0), REFERENCE_PADDING);
    }

    /// **Rule 5, from the window's side.** Detection is small - the box below
    /// came off the 1024² letterbox - and everything after it is large: the
    /// segmentation is native-sized, `k` comes off the letterbox, and the window
    /// is measured in the page's own pixels.
    ///
    /// This fails if the mask is ever put back at proxy resolution.
    #[test]
    fn a_box_detected_on_the_letterbox_is_measured_in_native_pixels() {
        let side = crate::constants::DETECTOR_INPUT;
        let fit = Letterbox {
            scale: side as f32 / 2400.0,
            fitted_w: (1600.0 * side as f32 / 2400.0) as u32,
            fitted_h: side,
            page_w: 1600,
            page_h: 2400,
        };
        let seg = Segmentation { width: 1600, height: 2400, levels: vec![0; 1600 * 2400], fit };
        assert_eq!((seg.width, seg.height), (1600, 2400), "measured large");
        let k = seg.proxy_scale();
        assert!((k - 2.34375).abs() < 1e-4, "k = max(w, h) / 1024");

        let strip = Strip::of_sizes(&[(1600, 2400)]);
        let joins = Joins::unchecked(0);
        let window = decode_window(&strip, &joins, Rect::new(600, 900, 300, 200), k, EngineContext::None);
        assert_eq!(window.pad, EdgePad::None);
        // 61 + 20 on each side, in the page's own pixels - not 26, and not 26
        // proxy pixels wearing a native name.
        assert_eq!(window.rect, Rect::new(600 - 81, 900 - 81, 300 + 162, 200 + 162));
    }

    /// Rule 4's clamp is rule 1's clamp, both halves. The gutter beside the
    /// narrow page is never touched.
    #[test]
    fn a_window_on_the_narrow_page_stops_at_that_pages_own_edges() {
        let strip = strip();
        let joins = Joins::unchecked(strip.joins());
        // Hard against the narrow page's left edge, which is at x = 50.
        let window = decode_window(&strip, &joins, Rect::new(55, 6000, 200, 200), 2.0, EngineContext::None);
        assert_eq!(window.rect.x, 50, "the gutter is 0..50 and was not read");
        assert!(window.requested.x < 50, "…and the formula did ask for it");
        assert_eq!(window.pad, EdgePad::Replicate);
        assert!(window.is_padded());
    }

    /// The other half: a window may cross a **verified** join and may not cross
    /// an unverified one.
    #[test]
    fn a_window_crosses_a_verified_join_and_stops_at_an_unverified_one() {
        let strip = strip();
        let boxr = Rect::new(200, 3900, 200, 60); // 3900..3960, join 0 is at 4000

        let stopped = decode_window(&strip, &Joins::unchecked(strip.joins()), boxr, 2.0, EngineContext::Local);
        assert_eq!(stopped.rect.bottom(), 4000, "an unverified join is a page edge");

        let mut joins = Joins::unchecked(strip.joins());
        joins.set(0, JoinState::Verified);
        let crossed = decode_window(&strip, &joins, boxr, 2.0, EngineContext::Local);
        assert!(crossed.rect.bottom() > 4000, "a verified join is read across");
        // …and the narrower page below still bounds it sideways, which is the
        // half of rule 1 that is easy to drop: the requested window reached
        // into the gutter and the clamped one does not.
        assert!(crossed.requested.x < 50);
        assert_eq!(crossed.rect.x, 50);
        assert!(crossed.rect.right() <= 750);
    }

    /// The engine context term. A tiny box still gets the model's whole input,
    /// because LaMa's spatial input is fixed at 512² and a smaller crop would be
    /// padded to it anyway - by us or by the model.
    #[test]
    fn a_small_box_for_a_fixed_input_model_still_gets_the_models_whole_input() {
        let strip = Strip::of_sizes(&[(4000, 4000)]);
        let joins = Joins::unchecked(0);
        let small = Rect::new(2000, 2000, 30, 20);

        let none = decode_window(&strip, &joins, small, 1.0, EngineContext::None);
        assert_eq!(none.rect.w, 30 + 2 * 46, "rung 0 reads the halo and no more");

        let local = decode_window(&strip, &joins, small, 1.0, EngineContext::Local);
        assert_eq!((local.rect.w, local.rect.h), (ENGINE_INPUT, ENGINE_INPUT));
        assert_eq!(local.rect.x + local.rect.w as i64 / 2, small.x + small.w as i64 / 2, "centred");

        let cloud = decode_window(&strip, &joins, small, 1.0, EngineContext::Tier { w: 1024, h: 1024 });
        assert_eq!((cloud.rect.w, cloud.rect.h), (1024, 1024));
    }

    /// A box larger than the engine input keeps its own size plus the halo:
    /// it is tiled, and a window that shrank to 512 would be the silent
    /// downscale rule 4 forbids.
    #[test]
    fn a_box_larger_than_the_engine_input_is_not_shrunk_to_it() {
        let strip = Strip::of_sizes(&[(4000, 4000)]);
        let joins = Joins::unchecked(0);
        let sfx = Rect::new(500, 500, 900, 700);
        let window = decode_window(&strip, &joins, sfx, 1.0, EngineContext::Local);
        assert!(window.rect.w >= 900, "{:?}", window.rect);
        assert!(window.rect.h >= ENGINE_INPUT);
        assert_eq!(window.rect.w, 900 + 2 * halo(1.0));
    }

    /// Rule 4's padding rule, and the variant that does not exist.
    #[test]
    fn a_page_edge_window_records_the_pad_it_used_and_never_reflects() {
        let strip = Strip::of_sizes(&[(800, 4000)]);
        let joins = Joins::unchecked(0);
        let corner = decode_window(&strip, &joins, Rect::new(0, 0, 40, 40), 2.0, EngineContext::Local);
        assert_eq!(corner.rect.x, 0);
        assert_eq!(corner.rect.y, 0);
        assert_eq!(corner.pad, EdgePad::Replicate);
        assert_eq!(EdgePad::Replicate.as_str(), "edge-replicate");
        assert_eq!(EdgePad::Constant.as_str(), "ring-median");
        assert_eq!(EdgePad::None.as_str(), "none");

        let median = decode_window_padded(
            &strip,
            &joins,
            Rect::new(0, 0, 40, 40),
            2.0,
            EngineContext::Local,
            EdgePad::Constant,
        );
        assert_eq!(median.pad, EdgePad::Constant);
        // An interior window records no pad whichever one was offered.
        let interior = decode_window_padded(
            &strip,
            &joins,
            Rect::new(300, 2000, 40, 40),
            2.0,
            EngineContext::None,
            EdgePad::Constant,
        );
        assert_eq!(interior.pad, EdgePad::None);
    }

    /// A box off the strip has no owning page, so there is no rectangle to
    /// clamp to and the window is empty rather than nearest-fit.
    #[test]
    fn a_box_off_the_strip_produces_an_empty_window() {
        let strip = strip();
        let window =
            decode_window(&strip, &Joins::unchecked(strip.joins()), Rect::new(0, 90_000, 10, 10), 2.0, EngineContext::None);
        assert_eq!(window.rect.w, 0);
        assert!(window.is_padded());
    }
}
