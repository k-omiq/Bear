//! Mask fitting: how far to grow the text mask, and where it should route.
//!
//! For each region the precise mask is grown in an ascending series, each
//! candidate's ring is measured, and the largest candidate that keeps
//! improving is taken. What makes it subtle is entirely in the failure
//! cases:
//!
//! - **Ratchet for selection, true minimum for failure.** Upstream's incumbent
//!   is the last *accepted* candidate, so a rejected candidate never updates the
//!   baseline and a box can fail even when a passing candidate existed.
//! - **The zero case ratchets, and that is wanted.** At deviation 0 the loop
//!   settles on the largest perfect mask, swallowing the anti-aliasing fringe
//!   instead of leaving a text-shaped ghost. Below an absolute floor of 0.5 the
//!   largest candidate under the floor wins, and **the floor takes precedence
//!   over the monotone gate**.
//! - **Reject candidates crossing a strong edge**, and never append the raw
//!   rectangle. Nothing else stops a grown mask crossing a bubble stroke onto
//!   outside paper, scoring better there, and winning - after which the fill
//!   paints paper over the outline. The same object reaches the **annulus**
//!   before it reaches the mask, and [`ring::paper_annulus`] is where that half
//!   is handled; the two are one correction and they are applied one ring
//!   apart.
//! - **The threshold is relative to the page noise floor.** A fixed 8.0 sits
//!   below the border deviation of flat white on a JPEG q75 raw.

use crate::constants::{
    FLAT_FLOOR, INPAINT_MIN_STD, MASK_GROWTH_STEP, MASK_GROWTH_STEPS,
    MIN_INPAINTING_RADIUS, MIN_MASK_THICKNESS,
};
use crate::detect::{Region, Segmentation};
use crate::image::Raster;
use crate::mask::{Mask, Rect};

mod noise;
pub mod ring;

pub use noise::{EdgeMap, page_noise_sigma, sobel_magnitude};
pub use ring::{Plane, RingStats};

/// Where §5's routing table sends a region.
///
/// Serialisable because a stored detection ([`crate::project::DetectedRegion`])
/// keeps the route its fit settled on: a later clean starts from the stored
/// mask and must not re-derive the route from a fit it no longer runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    /// Rung 0. Text on flat or planar background - the common case.
    /// `fill_and_denoise` is a stored route from before rung 1 was removed.
    #[serde(alias = "fill_and_denoise")]
    Fill,
    /// The inpaint ladder: the fit failed, or the ring is multimodal, or the
    /// annulus is periodic.
    Inpaint,
}

impl Route {
    pub fn needs_a_model(self) -> bool {
        matches!(self, Route::Inpaint)
    }
}

/// A fitted region: the mask that will actually be applied, and what to do with
/// it.
#[derive(Debug, Clone)]
pub struct Fitted {
    pub mask: Mask,
    /// **The lettering, its fringe, and a hole's worth of margin**: the seed
    /// grown by the *first* step of the candidate series and then by up to
    /// [`crate::constants::MODEL_HOLE_MARGIN`] more where no strong edge stops
    /// it ([`ink_for`]).
    ///
    /// Usually a subset of `mask`, and on most paths a much smaller one; on a
    /// noisy page whose series stopped at the first step it may reach a few
    /// pixels past it. It exists because the two things `mask` is used for stopped being
    /// the same thing. For rung 0 the mask is the region whose
    /// **own paper statistics** are painted back over it, so a mask that ran
    /// wide across flat paper wrote flat paper onto flat paper and cost
    /// nothing. For rungs 2 and up it is the region a **model invents**, and
    /// there a mask that ran wide is the defect the user sees: manga-LaMa
    /// answers a whole grown blob at a tone a level or two off the page's, and
    /// the result is a soft light box sitting in the middle of the panel with
    /// the text gone from a tenth of it.
    ///
    /// The blob is `mask` working as designed. §4's zero case "ratchets, and
    /// that is wanted" - below [`FLAT_FLOOR`] the largest candidate still under
    /// the floor wins, so on any region whose ring is flatter than half an
    /// 8-bit level *every* candidate is accepted and the last of the twelve is
    /// chosen. Measured on `fixtures/pages/page-sfx.png` at scale 2.34: a
    /// 65×169 detection box, a 2,912 px seed, and a chosen mask of 38,985 px -
    /// **355% of the box's own area**, grown 64 native pixels past the glyphs.
    /// The fringe that ratchet exists to swallow is one or two pixels wide.
    ///
    /// So the growth stays where it earns its keep and the model rungs write
    /// through this instead ([`crate::engines::model::applied_mask`], which
    /// adds the isolation ring to *this* mask, and
    /// [`crate::engines::lama::model_hole`], which cuts the tensor's hole to
    /// it). The crop the model reads is untouched - it is 512² of real page
    /// either way, and a smaller hole means *more* of that page is context
    /// rather than less.
    ///
    /// A hand-drawn mask is authoritative, so on the manual path this is the
    /// seed exactly, which is `mask` exactly: a user who painted a rectangle
    /// gets a rectangle inpainted.
    pub ink: Mask,
    /// The pixels `ring` was measured over: [`ring::paper_annulus`] around
    /// `mask`. Carried rather than recomputed so that [`crate::engines::fill`]
    /// paints a tone measured from the same paper the route was decided from -
    /// two builds of the same set are two chances to disagree, and §4's fifth
    /// correction is about exactly the pixels these two would disagree over.
    pub paper: Mask,
    pub ring: RingStats,
    pub route: Route,
    /// The growth, in native pixels, that produced `mask`. Recorded because
    /// §6's second inpaint population is "deviation above `inpaint_min_std`
    /// *and* chosen thickness ≤ `min_inpainting_radius`", and because it goes
    /// into the provenance snapshot.
    pub thickness: u32,
    /// The smallest deviation any candidate achieved, accepted or not. §4's
    /// fail test uses this rather than the incumbent.
    pub best_deviation: f32,
}

/// The mask a region starts from: the segmentation, intersected with the
/// region's masking box. §4 step 1.
pub fn seed_mask(seg: &Segmentation, region: &Region, page_w: u32, page_h: u32) -> Mask {
    let bounds = region.masking.grown(0, page_w, page_h);
    let mut mask = Mask::empty(bounds);
    for y in bounds.y..bounds.bottom() {
        for x in bounds.x..bounds.right() {
            if x < 0 || y < 0 || x >= seg.width as i64 || y >= seg.height as i64 {
                continue;
            }
            if seg.is_text(x as u32, y as u32) {
                mask.set(x, y, true);
            }
        }
    }
    mask
}

/// The ascending series of grown masks, in native pixels.
///
/// §4's constants are proxy pixels and growth is **cumulative** - each step
/// dilates the previous result rather than the seed. The first step is
/// `min_mask_thickness`, then `mask_growth_step` for eleven more.
/// The first step of the series, in native pixels: `min_mask_thickness` at this
/// page's proxy scale.
///
/// Named once because three callers need the same number - the series starts
/// there, [`Fitted::ink`] stops there, and the fallback below grows the seed by
/// exactly it - and three spellings of `round().max(1.0)` would eventually
/// disagree by a pixel.
fn first_step(scale: f32) -> u32 {
    ((MIN_MASK_THICKNESS as f32) * scale).round().max(1.0) as u32
}

fn candidates(seed: &Mask, scale: f32, bounds: Rect) -> Vec<(u32, Mask)> {
    let first = first_step(scale);
    let step = ((MASK_GROWTH_STEP as f32) * scale).round().max(1.0) as u32;

    let mut series = Vec::with_capacity(MASK_GROWTH_STEPS as usize + 1);
    let mut current = seed.dilated_within(first, bounds);
    let mut thickness = first;
    series.push((thickness, current.clone()));
    for _ in 0..MASK_GROWTH_STEPS {
        current = current.dilated_within(step, bounds);
        thickness += step;
        series.push((thickness, current.clone()));
    }
    series
}

/// A zero-padding mask can end exactly on the glyph's strong Sobel edge,
/// leaving no adjacent pixel from which the paper flood can start. Move only
/// the sampling ring one source pixel out in that case; keep the write mask
/// authoritative. Stored-mask cleaning uses the same sampling rule.
fn authoritative_paper(page: &Raster, mask: &Mask, edges: &noise::EdgeMap) -> Mask {
    let paper = ring::paper_annulus(page, mask, 0, edges);
    if paper.is_empty() {
        ring::paper_annulus(page, mask, 1, edges)
    } else {
        paper
    }
}

/// Fit one region.
///
/// `manual` skips the candidate search entirely: §4's last subsection makes a
/// hand-drawn mask **authoritative**, because the user has already decided
/// where the edit goes and a search would quietly move the boundary. The ring
/// sample, the periodicity check and the routing still run - that is what makes
/// a manual fill match local paper instead of defaulting to white.
pub fn fit(
    page: &Raster,
    seed: &Mask,
    scale: f32,
    noise_sigma: f32,
    edges: &noise::EdgeMap,
    manual: bool,
) -> Fitted {
    fit_within(page, seed, scale, noise_sigma, edges, manual, Rect::new(0, 0, page.width, page.height))
}

/// [`fit`], with the candidate series clamped to a **decode window** rather
/// than to the whole page.
///
/// Rule 4 makes the window the
/// correctness unit - "the segment is a **scheduling** unit; the decode window
/// is a **correctness** unit" - and this is the clamp that makes that true of a
/// mask. [`crate::strip::decode_window`] is what answers the rectangle, and it
/// ends in [`crate::strip::Strip::clamp`], so the gutter beside a narrow page
/// and an unverified join both arrive here as a smaller `bounds`.
///
/// Passing the whole page is [`fit`], and on a window sized by rule 4 the
/// clamp never truncates: the window is the box grown by
/// `growth(k) + 20`, and the series reaches `growth(k)` from a seed that is
/// inside the box. It bites only where the window itself was cut.
pub fn fit_within(
    page: &Raster,
    seed: &Mask,
    scale: f32,
    noise_sigma: f32,
    edges: &noise::EdgeMap,
    manual: bool,
    bounds: Rect,
) -> Fitted {
    // Whether rung 2 exists for this source at all. Asked here rather
    // than at the engines, so a bitonal or indexed region is routed to the rung
    // that will actually run it: a route naming a rung the source cannot carry
    // would reach the review row and the provenance record as a claim that
    // never happened.
    let rungs = Available::of(page);

    if manual {
        let paper = authoritative_paper(page, seed, edges);
        let ring = ring::sample_over(page, &paper);
        let route = route_for(&ring, false, 0, rungs);
        let best_deviation = ring.deviation;
        return Fitted {
            mask: seed.clone(),
            // Authoritative means authoritative: the set the user painted is
            // both the mask and the set a model writes through, with no ring
            // added and none taken away.
            ink: seed.clone(),
            paper,
            ring,
            route,
            thickness: 0,
            best_deviation,
        };
    }

    let seed = &with_stray_lettering(page, seed, scale, noise_sigma, bounds);
    let series = candidates(seed, scale, bounds);
    let threshold = deviation_threshold(noise_sigma);
    // §8's constants are 8-bit levels and every deviation here is 16-bit luma.
    // Every sibling threshold scales - `deviation_threshold` multiplies its 8.0
    // by 257 and `route_for` multiplies `INPAINT_MIN_STD` by 257 - and this one
    // did not, which made the floor a five-hundredth of one 8-bit level. Below
    // an unreachable floor the branch §4 calls "the zero case ratchets, and that
    // is wanted" never fired on a page with any noise in it at all: the monotone
    // gate ended the search instead, and the anti-aliasing fringe the ratchet
    // exists to swallow was left as a text-shaped ghost. Only a synthetic page
    // whose deviation is exactly zero reached it, which is why the fixtures
    // never said so.
    let flat_floor = FLAT_FLOOR * 257.0;

    let mut best_deviation = f32::MAX;
    let mut chosen: Option<(u32, Mask, Mask, RingStats)> = None;
    let mut incumbent = f32::MAX;

    for (thickness, candidate) in series {
        // Never let a candidate cross a strong edge: a grown mask that reaches
        // over a bubble stroke onto outside paper scores better there, wins,
        // and the fill paints paper over the outline.
        if edges.crosses(&candidate) {
            break;
        }
        let paper = ring::paper_annulus(page, &candidate, 0, edges);
        let stats = ring::sample_over(page, &paper);
        if stats.count == 0 {
            break;
        }
        let deviation = stats.deviation;
        best_deviation = best_deviation.min(deviation);

        let accept = if incumbent == f32::MAX {
            true
        } else if incumbent < flat_floor {
            // The floor takes precedence over the monotone gate: below it the
            // largest candidate still under the floor wins, which is what
            // swallows the anti-aliasing fringe instead of leaving a
            // text-shaped ghost.
            deviation < flat_floor
        } else {
            // Improve on the incumbent by at least 10%, and never grow into a
            // worse ring.
            deviation <= incumbent * 0.9
        };

        if accept {
            incumbent = deviation;
            chosen = Some((thickness, candidate, paper, stats));
        } else if incumbent >= flat_floor {
            // Above the floor the deviation curve must be monotone
            // non-increasing; the first rise ends the search.
            break;
        }
    }

    match chosen {
        Some((thickness, mask, paper, ring)) => {
            let failed = best_deviation > threshold;
            let route = route_for(&ring, failed, thickness, rungs);
            let ink = ink_for(seed, scale, bounds, edges);
            Fitted { mask, ink, paper, ring, route, thickness, best_deviation }
        }
        None => {
            // Nothing was acceptable: fall back to the largest growth that is
            // *admissible*, and route to the ladder - or, on a source the ladder
            // cannot serve, to the rung that can.
            let (thickness, mask) = admissible(seed, scale, bounds, edges);
            let paper = ring::paper_annulus(page, &mask, 0, edges);
            let ring = ring::sample_over(page, &paper);
            Fitted {
                // There is no growth to hold back here: this mask *is* the
                // minimum, so the set a model writes through is the whole of
                // it.
                ink: mask.clone(),
                mask,
                paper,
                ring,
                route: rungs.ladder(),
                thickness,
                best_deviation,
            }
        }
    }
}

/// Fit the background and route, but let an explicit padding own the mask
/// boundary. Both output masks grow directly from the segmentation seed, not
/// from the candidate search's mask or its separately padded model ink.
/// Zero therefore preserves the seed exactly. Re-sample the paper around the
/// actual output so Fill measures the same surround as a later stored clean.
pub fn fit_with_padding(
    page: &Raster,
    seed: &Mask,
    scale: f32,
    noise_sigma: f32,
    edges: &noise::EdgeMap,
    bounds: Rect,
    padding: u32,
) -> Fitted {
    let mut fitted = fit_within(page, seed, scale, noise_sigma, edges, false, bounds);
    fitted.mask = seed.dilated_within(padding, bounds);
    fitted.ink = fitted.mask.clone();
    fitted.paper = authoritative_paper(page, &fitted.mask, edges);
    fitted.ring = ring::sample_over(page, &fitted.paper);
    fitted.thickness = padding;
    fitted
}

/// The set a model rung writes through, for a region whose candidate series
/// settled: the seed grown by the first step and then by up to
/// [`crate::constants::MODEL_HOLE_MARGIN`] more, taking the largest of those
/// growths whose border clears the edge map.
///
/// The margin exists because the stroke-tight hole - the first step alone,
/// which is what this returned before - leaves glyph-shaped marks on real
/// scans, and the retry button removed them by accident: a re-run's hole is the
/// stored applied mask, which is that hole grown by the isolation radius. So the
/// first pass now gives the model what the second pass gave it. The walk is
/// downward, the way [`admissible`] searches, so that a balloon whose lettering
/// nearly touches its outline gets exactly as much margin as fits and never a
/// hole across the outline; the first step is the floor because it is the hole
/// every region already had.
fn ink_for(seed: &Mask, scale: f32, bounds: Rect, edges: &noise::EdgeMap) -> Mask {
    let first = first_step(scale);
    for radius in (first + 1..=first + crate::constants::MODEL_HOLE_MARGIN).rev() {
        let candidate = seed.dilated_within(radius, bounds);
        if !edges.crosses(&candidate) {
            return candidate;
        }
    }
    seed.dilated_within(first, bounds)
}

/// How far from the lettering an ink island may sit and still be taken for
/// lettering the segmentation missed, in multiples of the first step.
const STRAY_REACH_STEPS: u32 = 2;

/// The share of the paper around the lettering that must sit at one tone for
/// [`with_stray_lettering`] to run at all.
const STRAY_FLAT_SHARE: f32 = 0.9;

/// The seed plus the lettering the segmentation left out of it, on flat paper.
///
/// A lettering mask misses small pieces of the text it marks: furigana beside
/// the kanji, the dots of a dakuten, a stroke's thin tip. Left out, each piece
/// is a strong edge a few pixels from the seed, so the first candidate's border
/// lands on it, the series stops before it starts, and the fit falls back to
/// [`admissible`]'s one-pixel growth. The fill then paints paper over the
/// glyphs and leaves the pieces and the whole anti-aliasing fringe standing,
/// and [`crate::quality::assess`] never sees them: it scores the mask's
/// interior and the paper outside its bounding box, and the pieces are between
/// the two. Measured on a real chapter, 7 of the 16 regions whose fit fell back
/// that way kept visible specks, against 3 of the 296 regions whose fit grew.
///
/// An island joins the seed when all of these hold:
///
/// - it lies within [`STRAY_REACH_STEPS`] first steps of the lettering;
/// - it does not reach the edge of that search box, so a balloon outline or a
///   panel border that runs past the lettering is never taken;
/// - its box does not span the lettering's box in both directions, so a small
///   balloon's whole outline, which fits inside the search box, is not either.
///
/// And only on flat paper: at least [`STRAY_FLAT_SHARE`] of the pixels around
/// the lettering, past its anti-aliasing, must sit at the paper's tone. A
/// screentone or a drawn background fails that, and is left to the fit as it
/// was. Where it holds, painting a wrongly taken island is painting the paper's
/// own tone over it, which is the cheap mistake.
fn with_stray_lettering(page: &Raster, seed: &Mask, scale: f32, noise_sigma: f32, bounds: Rect) -> Mask {
    let Some(letters) = set_bounds(seed) else { return seed.clone() };
    let reach = STRAY_REACH_STEPS * first_step(scale);
    let grown = letters.grown(reach, u32::MAX, u32::MAX);
    let x0 = grown.x.max(bounds.x).max(0);
    let y0 = grown.y.max(bounds.y).max(0);
    let x1 = grown.right().min(bounds.right()).min(page.width as i64);
    let y1 = grown.bottom().min(bounds.bottom()).min(page.height as i64);
    if x1 <= x0 || y1 <= y0 {
        return seed.clone();
    }
    let search = Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32);
    let luma = |x: i64, y: i64| page.luma16_at(x as u32, y as u32) as i32;

    // The paper, read past the lettering's anti-aliasing ramp.
    let fringe = seed.dilated_within(antialias_pad(scale) + 1, search);
    let mut paper_levels: Vec<i32> = Vec::new();
    for y in search.y..search.bottom() {
        for x in search.x..search.right() {
            if !fringe.contains(x, y) {
                paper_levels.push(luma(x, y));
            }
        }
    }
    if paper_levels.is_empty() {
        return seed.clone();
    }
    paper_levels.sort_unstable();
    let paper = paper_levels[paper_levels.len() / 2];
    let tolerance = (2.0 * deviation_threshold(noise_sigma)) as i32;
    let at_paper = paper_levels.iter().filter(|&&v| (v - paper).abs() <= tolerance).count();
    if (at_paper as f32) < STRAY_FLAT_SHARE * paper_levels.len() as f32 {
        return seed.clone();
    }

    let is_ink = |x: i64, y: i64| !seed.contains(x, y) && (luma(x, y) - paper).abs() > tolerance;
    let r = reach as i64;
    let near_lettering = |x: i64, y: i64| {
        (-r..=r).any(|dy| (-r..=r).any(|dx| seed.contains(x + dx, y + dy)))
    };

    let mut strays = Mask::empty(search);
    let mut seen = Mask::empty(search);
    let mut island = Vec::new();
    for sy in search.y..search.bottom() {
        for sx in search.x..search.right() {
            if seen.contains(sx, sy) || !is_ink(sx, sy) {
                continue;
            }
            // One 8-connected island, flooded from here.
            island.clear();
            seen.set(sx, sy, true);
            let mut stack = vec![(sx, sy)];
            let mut edge = false;
            let (mut ix0, mut iy0, mut ix1, mut iy1) = (sx, sy, sx, sy);
            while let Some((x, y)) = stack.pop() {
                island.push((x, y));
                edge |= x == search.x || y == search.y || x == search.right() - 1 || y == search.bottom() - 1;
                (ix0, iy0, ix1, iy1) = (ix0.min(x), iy0.min(y), ix1.max(x), iy1.max(y));
                for (dx, dy) in [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
                    let (nx, ny) = (x + dx, y + dy);
                    if search.contains(nx, ny) && !seen.contains(nx, ny) && is_ink(nx, ny) {
                        seen.set(nx, ny, true);
                        stack.push((nx, ny));
                    }
                }
            }
            let spans = ix1 - ix0 + 1 >= letters.w as i64 && iy1 - iy0 + 1 >= letters.h as i64;
            if edge || spans || !island.iter().any(|&(x, y)| near_lettering(x, y)) {
                continue;
            }
            for &(x, y) in &island {
                strays.set(x, y, true);
            }
        }
    }
    if strays.is_empty() {
        return seed.clone();
    }
    Mask::union(&[seed, &strays], page.width, page.height)
}

/// The box of a mask's set pixels, which can be much smaller than its bounds.
fn set_bounds(mask: &Mask) -> Option<Rect> {
    let b = mask.bounds;
    let (mut x0, mut y0, mut x1, mut y1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    for y in b.y..b.bottom() {
        for x in b.x..b.right() {
            if mask.contains(x, y) {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            }
        }
    }
    (x0 <= x1).then(|| Rect::new(x0, y0, (x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32))
}

/// The fallback mask for a region no candidate fitted: the largest growth at or
/// below the first step whose border does not sit on a strong edge.
///
/// **This is the answer to an incoherent earlier fallback.** That fallback
/// was incoherent on its own terms: it returned
/// `seed.dilated_within(first_step, bounds)` - the very mask whose strong-edge
/// crossing had refused every candidate - and routed it to a model, which then
/// wrote into the outline the refusal existed to protect. Seven of the eighteen
/// gated regions on `page-crowded` take this path.
///
/// There were two ways out, decline the region or route with the last
/// admissible mask, and "the second keeps the region and the first
/// keeps the rule". Both are kept by searching **downward** instead of
/// sideways. The candidate series only ever grows, so when its first step is
/// already refused there is no admissible member of it - but there are nine
/// smaller radii below that step, and the rule the series enforces applies to
/// them one at a time. The largest that clears the edge map is the answer.
///
/// **The walk stops at the lettering's anti-aliasing, not at the seed.** A
/// segmentation mask ends inside the ramp from glyph to paper, and the ramp is
/// lettering: measured on the demo chapter's own SAM masks at scale 1.56, a
/// quarter of the first pixel shell outside the mask is still glyph ink and
/// the second is at the background rate. The walk used to go on down to the
/// seed itself, and did on 107 of the chapter's 227 cloud regions - every
/// radius up to the first step crossed an edge, mostly the strokes of glyphs
/// the old seed had cut - which left that ramp outside the hole for a model to
/// read as picture and for a fill to leave standing as a glyph-shaped ghost.
/// [`antialias_pad`] is the ramp at this page's scale, and it is taken even
/// where its border meets an edge: that close to the lettering, the edge is
/// the lettering's own, or art the glyph already overruns - the fixture
/// defect seen on seven `page-crowded` regions, where no smaller honest mask
/// exists either. **A region is never dropped for want of an admissible
/// mask.**
fn admissible(seed: &Mask, scale: f32, bounds: Rect, edges: &noise::EdgeMap) -> (u32, Mask) {
    let floor = antialias_pad(scale).min(first_step(scale));
    for radius in (floor + 1..=first_step(scale)).rev() {
        let candidate = seed.dilated_within(radius, bounds);
        if !edges.crosses(&candidate) {
            return (radius, candidate);
        }
    }
    (floor, seed.dilated_within(floor, bounds))
}

/// How far a glyph's anti-aliasing reaches past its segmentation mask, in
/// native pixels: one proxy pixel at this page's scale.
fn antialias_pad(scale: f32) -> u32 {
    (ANTIALIAS_PAD as f32 * scale).round().max(1.0) as u32
}

/// [`antialias_pad`] in proxy pixels, the unit of §4's other constants.
const ANTIALIAS_PAD: u32 = 1;

/// The most [`outlined`] grows lettering, in proxy pixels.
const OUTLINE_MAX: u32 = 2 * MIN_MASK_THICKNESS;

/// Lettering outside a balloon, grown by half its own stroke width (at most
/// [`OUTLINE_MAX`] proxy pixels), so the outline an effect draws around its
/// letters is part of what is cleaned.
///
/// A lettering mask marks the letters and not that outline, and outside a
/// balloon the art beside it is full of strong edges, so every growth of the
/// seed crosses one and the fit keeps a hole a pixel or two past the letters.
/// The outline is left standing around the hole, and a model rung reads it as
/// the letters' surround and paints their shape back in its colour. Measured
/// on chapter 109 with the whole run (SAM-TS-L, Ogkalu small, LaMa): effects
/// drawn black with a white outline came back as white letters. Their outlines
/// end 4 to 5 px past the mask, and their strokes are 10 to 25 px wide, so half
/// a stroke reaches past the outline on every one of them and grows small
/// lettering by little. A balloon's lettering sits on paper, where the fit
/// grows freely, and is not grown here.
pub fn outlined(seed: &Mask, scale: f32, bounds: Rect) -> Mask {
    let half = stroke_width(seed) / 2.0;
    let radius = half.min(OUTLINE_MAX as f32 * scale).round() as u32;
    if radius == 0 { seed.clone() } else { seed.dilated_within(radius, bounds) }
}

/// Twice a mask's area over its border pixels: the width of a stroke much
/// longer than it is wide. Zero for an empty mask.
fn stroke_width(mask: &Mask) -> f32 {
    let bounds = mask.bounds;
    let (mut area, mut border) = (0u32, 0u32);
    for y in bounds.y..bounds.bottom() {
        for x in bounds.x..bounds.right() {
            if !mask.contains(x, y) {
                continue;
            }
            area += 1;
            if !mask.contains(x - 1, y) || !mask.contains(x + 1, y)
                || !mask.contains(x, y - 1) || !mask.contains(x, y + 1) {
                border += 1;
            }
        }
    }
    if border == 0 { 0.0 } else { 2.0 * area as f32 / border as f32 }
}

/// `max(8.0, 2.5 × page_noise_sigma)`, in 16-bit luma.
///
/// §4: a fixed 8.0 sits below the border deviation of flat white on a JPEG q75
/// raw, where blocking alone gives 5–12, so every region on every JPEG source
/// would fail.
pub fn deviation_threshold(noise_sigma: f32) -> f32 {
    (8.0f32 * 257.0).max(2.5 * noise_sigma)
}

/// Which rungs above rung 0 can run on this source at all.
///
/// **A route names a rung that will actually run.** Rung 0 needs no entry here:
/// it is arithmetic over the page's own samples, it serves every mode and depth
/// this application reads, and it is therefore the floor every other answer
/// falls back to.
///
/// The engines are asked rather than the mode and depth being re-tested here,
/// so there is one statement of what each rung can carry and the router reads
/// it instead of keeping a second copy that drifts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Available {
    inpaint: bool,
}

impl Available {
    fn of(page: &Raster) -> Available {
        Available { inpaint: crate::engines::lama::applies(page) }
    }

    /// Where a region the fit could not settle belongs.
    ///
    /// The ladder, when the source can carry a model rung's output - and rung 0
    /// when it cannot. **A bitonal scan is the case that made this matter.**
    /// Every one of its annuli is dither, so [`ring::sample`] reports every one
    /// of them periodic and §5's table sends every region on the page to the
    /// ladder; rung 2 then declines all of them for the depth
    /// ([`crate::engines::lama::Decline::Depth`]), and the page cleans nothing
    /// at all. The engine is right to refuse - a continuous-tone answer written
    /// into one bit per sample is per-pixel nearest-entry snapping, which is
    /// refused - and the router was wrong to send it there. Rung 0's flat
    /// fill serves a bitonal source and always has.
    ///
    /// The same holds one mode over, for indexed and CMYK, where rungs 2-4
    /// are disabled outright: the honest answer is the rung that runs, not
    /// a route to a rung that will refuse it.
    fn ladder(self) -> Route {
        if self.inpaint { Route::Inpaint } else { Route::Fill }
    }
}

/// §5's routing table.
fn route_for(
    ring: &RingStats,
    failed: bool,
    thickness: u32,
    rungs: Available,
) -> Route {
    // A periodic annulus forbids rung 0 regardless of deviation.
    if failed || ring.multimodal || ring.periodic || ring.count == 0 {
        return rungs.ladder();
    }
    // The second inpaint population: fitted, but poorly, and pinned tight.
    if ring.deviation > INPAINT_MIN_STD * 257.0 && thickness <= MIN_INPAINTING_RADIUS {
        return rungs.ladder();
    }
    Route::Fill
}

/// The rectangle a region's engine context is cut from: the reference box,
/// which §2 defines as the merged box grown by 20 px and which §5.2 calls the
/// base cutout.
pub fn context_window(region: &Region, page_w: u32, page_h: u32) -> Rect {
    region.reference.grown(0, page_w, page_h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{BitDepth, ColorMode, fixtures};

    const PAGE: u32 = 100;
    /// Small enough that the annulus has room on all four sides of it.
    const REGION: Rect = Rect { x: 42, y: 44, w: 16, h: 12 };

    /// An 8 px halftone grid - `ring.rs`'s own screentone, which its tests pin
    /// as periodic.
    fn tone(x: u32, y: u32) -> bool {
        x % 8 < 3 && y % 8 < 3
    }

    fn gray_page(f: impl Fn(u32, u32) -> u8) -> Raster {
        let mut data = Vec::with_capacity((PAGE * PAGE) as usize);
        for y in 0..PAGE {
            for x in 0..PAGE {
                data.push(f(x, y));
            }
        }
        Raster {
            width: PAGE,
            height: PAGE,
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

    /// The same picture at one bit per sample, packed the way a 1-bit PNG is:
    /// rows padded to a byte boundary, and a set bit meaning white.
    fn bitonal_page(ink: impl Fn(u32, u32) -> bool) -> Raster {
        let stride = (PAGE as usize).div_ceil(8);
        let mut data = vec![0u8; stride * PAGE as usize];
        for y in 0..PAGE {
            for x in 0..PAGE {
                if !ink(x, y) {
                    data[y as usize * stride + x as usize / 8] |= 0x80 >> (x % 8);
                }
            }
        }
        Raster {
            width: PAGE,
            height: PAGE,
            mode: ColorMode::Gray,
            depth: BitDepth::One,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None,
            color: Default::default(),
            data,
        }
    }

    fn routed(page: &Raster, rect: Rect) -> Fitted {
        let seed = Mask::filled(rect);
        let edges = EdgeMap::none(page.width, page.height);
        fit(page, &seed, 1.0, page_noise_sigma(page), &edges, false)
    }

    /// A route naming a rung that will actually run, at the depth below the
    /// one it was written for.
    ///
    /// A 1-bit scan is dither everywhere, so every annulus on it is periodic and
    /// §5's table sent **every region of the page** to the ladder - where rung 2
    /// declines all of them for the depth and the page cleaned nothing at all.
    /// The tone here is the same tone in both halves of the test, so what the
    /// route turns on is the depth and not the pattern.
    #[test]
    fn a_bitonal_halftone_routes_to_rung_zero_where_the_same_tone_in_grey_routes_to_the_ladder() {
        let grey = gray_page(|x, y| if tone(x, y) { 40 } else { 250 });
        assert!(crate::engines::lama::applies(&grey));
        assert_eq!(routed(&grey, REGION).route, Route::Inpaint, "the ladder lost its own case");

        let bitonal = bitonal_page(tone);
        assert!(!crate::engines::lama::applies(&bitonal));
        let fitted = routed(&bitonal, REGION);
        assert_eq!(fitted.route, Route::Fill);

        // And it is the availability of the rung that put it there, rather than
        // the ring happening to read differently at one bit: handed the very
        // same measurement with the ladder open to this source, §5's table
        // sends it to the ladder.
        let sigma = page_noise_sigma(&bitonal);
        let failed = fitted.best_deviation > deviation_threshold(sigma);
        assert_eq!(
            route_for(&fitted.ring, failed, fitted.thickness, Available { inpaint: true }),
            Route::Inpaint,
        );
    }

    /// A tone the ring can no longer see is still kept off rung 0.
    ///
    /// [`ring::paper_annulus`] restricts the ring to the paper on the mask's own
    /// side of a strong edge, and its own tests record the one texture that
    /// restriction flattens: a **hard-edged** halftone, whose dots are strong
    /// through and through, so the flood has nothing to enter and the ring reads
    /// as constant paper. Left there the region would route to a flat fill and
    /// land a grey rectangle in dotted tone, which is the core failure this
    /// module exists to avoid.
    ///
    /// What holds it is §4's fifth correction, one ring in: a mask growing
    /// through a tone puts its border on a dot within the first step, the
    /// candidate is refused, and no candidate is ever accepted. The two
    /// corrections are the same statement about the same edge map, and this is
    /// the case where only the outer one has anything left to say.
    ///
    /// Both edge maps are real here. With [`EdgeMap::none`] the ring would catch
    /// it unaided, and the test would prove nothing about the restriction.
    #[test]
    fn a_halftone_the_ring_restriction_flattens_is_still_kept_off_rung_zero() {
        let page = gray_page(|x, y| if tone(x, y) { 40 } else { 250 });
        let edges = EdgeMap::sobel(&page);
        let seed = Mask::filled(REGION);

        // The premise: the ring by itself no longer reports anything unusual.
        let candidate = seed.dilated_within(4, Rect::new(0, 0, PAGE, PAGE));
        let blinded = ring::sample(&page, &candidate, 0, &edges);
        assert!(!blinded.periodic, "the premise is gone; rewrite this test");
        assert!(blinded.deviation < 1.0, "the premise is gone: {}", blinded.deviation);

        // And the route is the ladder regardless.
        let fitted = fit(&page, &seed, 1.0, page_noise_sigma(&page), &edges, false);
        assert_eq!(fitted.route, Route::Inpaint, "a flat fill would land in dotted tone");
    }

    #[test]
    fn a_stored_fill_and_denoise_route_reads_as_the_fill() {
        let route: Route = serde_json::from_str("\"fill_and_denoise\"").unwrap();
        assert_eq!(route, Route::Fill);
        assert_eq!(serde_json::to_value(Route::Fill).unwrap(), serde_json::json!("fill"));
    }

    /// The invariant behind that one, over every colour mode and depth the
    /// application reads: a route names a rung that will actually run.
    #[test]
    fn no_route_ever_names_a_rung_the_source_cannot_carry() {
        for fixture in fixtures::all() {
            let page = fixture.raster;
            let rect = Rect::new(24, 18, 12, 10);
            let route = routed(&page, rect).route;
            if route == Route::Inpaint {
                assert!(
                    crate::engines::lama::applies(&page),
                    "{} was routed to a rung that would decline it",
                    fixture.name
                );
            }
        }
    }

    /* -- lettering the segmentation missed ------------------------------ */

    /// A glyph on white paper, marked by the seed, with a 2×2 dot two pixels
    /// right of it that the seed left out: a dakuten the segmentation missed.
    fn glyph_with_missed_dot() -> (Raster, Mask, (i64, i64)) {
        let dot = (REGION.right() + 2, REGION.y + 3);
        let page = gray_page(|x, y| {
            let (x, y) = (x as i64, y as i64);
            let on_dot = (dot.0..dot.0 + 2).contains(&x) && (dot.1..dot.1 + 2).contains(&y);
            if REGION.contains(x, y) || on_dot { 0 } else { 250 }
        });
        (page, Mask::filled(REGION), dot)
    }

    /// The missed dot is lettering. Without it in the seed, the first
    /// candidate's border lands on the dot's edge, the series never starts, and
    /// the fill leaves the dot and the fringe standing. With it, the mask grows
    /// as it does for any balloon and the dot is painted over.
    #[test]
    fn a_missed_dot_beside_the_lettering_joins_the_mask_and_the_mask_grows() {
        let (page, seed, dot) = glyph_with_missed_dot();
        let edges = EdgeMap::sobel(&page);
        assert!(edges.crosses(&seed.dilated(4, PAGE, PAGE)), "the premise is gone: the dot no longer stops the series");

        let fitted = fit(&page, &seed, 1.0, page_noise_sigma(&page), &edges, false);
        assert!(fitted.mask.contains(dot.0, dot.1) && fitted.mask.contains(dot.0 + 1, dot.1 + 1));
        assert!(fitted.thickness >= 4, "the series still fell back, at {}", fitted.thickness);
        assert_eq!(fitted.route, Route::Fill);
    }

    /// A balloon outline close around the lettering fits inside the search box
    /// whole, and is still not lettering: its box spans the lettering's.
    #[test]
    fn a_small_balloon_outline_is_never_taken_for_missed_lettering() {
        let outline = REGION.grown(5, PAGE, PAGE);
        let inner = REGION.grown(4, PAGE, PAGE);
        let page = gray_page(|x, y| {
            let (x, y) = (x as i64, y as i64);
            if REGION.contains(x, y) || (outline.contains(x, y) && !inner.contains(x, y)) { 0 } else { 250 }
        });
        let seed = Mask::filled(REGION);
        let taken = with_stray_lettering(&page, &seed, 1.0, page_noise_sigma(&page), Rect::new(0, 0, PAGE, PAGE));
        assert_eq!(taken.count(), seed.count(), "the outline joined the lettering");
    }

    /// Screentone around the lettering is not paper, and no dot of it is taken.
    #[test]
    fn no_island_is_taken_from_screentone() {
        let page = gray_page(|x, y| if REGION.contains(x as i64, y as i64) || tone(x, y) { 40 } else { 250 });
        let seed = Mask::filled(REGION);
        let taken = with_stray_lettering(&page, &seed, 1.0, page_noise_sigma(&page), Rect::new(0, 0, PAGE, PAGE));
        assert_eq!(taken.count(), seed.count(), "screentone dots joined the lettering");
    }

    /* -- what a model rung is allowed to write through ------------------- */

    /// The hole margin stops at an outline. A stroke six pixels right of the
    /// lettering: the first step clears it, the full margin would cross it,
    /// and `ink` is the largest growth in between whose border does not.
    #[test]
    fn the_hole_margin_stops_short_of_a_strong_edge() {
        let stroke = REGION.right() + 6;
        let page = gray_page(|x, _| if (x as i64) == stroke || (x as i64) == stroke + 1 { 0 } else { 250 });
        let seed = Mask::filled(REGION);
        let edges = EdgeMap::sobel(&page);
        let fitted = fit(&page, &seed, 1.0, page_noise_sigma(&page), &edges, false);
        let first = seed.dilated(4, PAGE, PAGE);
        let full = seed.dilated(4 + crate::constants::MODEL_HOLE_MARGIN, PAGE, PAGE);
        assert!(fitted.ink.count() > first.count(), "no margin was taken at all");
        assert!(fitted.ink.count() < full.count(), "the margin reached across the stroke");
        assert!(!edges.crosses(&fitted.ink), "ink's border sits on the stroke");
    }

    /// **The blob, and the mask that is not it.**
    ///
    /// On flat paper the deviation is zero at every candidate, so the zero case
    /// ratchets all the way to the end of the series and `mask` is the seed grown
    /// by `4 + 11 × 2`. That is [`Fitted::mask`] working as designed and it is
    /// harmless where rung 0 repaints the region's own paper over it. It is the
    /// user-visible box where a model invents the tone instead, so
    /// [`Fitted::ink`] stops at the first step plus the hole margin, and the
    /// whole of a model rung's geometry is measured from that.
    #[test]
    fn the_ratchet_grows_the_mask_and_the_set_a_model_writes_through_stays_on_the_lettering() {
        let page = gray_page(|_, _| 250);
        let seed = Mask::filled(REGION);
        let fitted = fit(&page, &seed, 1.0, page_noise_sigma(&page), &EdgeMap::none(PAGE, PAGE), false);

        assert_eq!(fitted.thickness, 26, "the zero case did not ratchet to the end");
        assert_eq!(
            fitted.ink,
            seed.dilated(4 + crate::constants::MODEL_HOLE_MARGIN, PAGE, PAGE),
            "ink is the first step plus the hole margin, exactly"
        );
        assert_eq!(fitted.thickness, 26, "and the growth is still recorded in full");

        // A subset, and a small one: the seed is 16×12 and the mask is 68×64.
        for y in 0..PAGE as i64 {
            for x in 0..PAGE as i64 {
                assert!(
                    !fitted.ink.contains(x, y) || fitted.mask.contains(x, y),
                    "({x}, {y}) is written through but outside the fitted mask"
                );
            }
        }
        // A third of the blob, with the hole margin on: 912 against 3,044
        // here, where the first step alone was 480.
        assert!(
            fitted.ink.count() * 3 < fitted.mask.count(),
            "ink {} against mask {}",
            fitted.ink.count(),
            fitted.mask.count()
        );

        // And against the detection box the user sees, which is what the defect
        // was reported as: the grown mask is several times its area and the
        // written set is a small multiple of it.
        let box_area = (REGION.w * REGION.h) as usize;
        assert!(fitted.mask.count() > 3 * box_area, "the blob is gone; rewrite this test");
        assert!(fitted.ink.count() < 5 * box_area);
    }

    #[test]
    fn explicit_padding_replaces_automatic_mask_and_ink_growth() {
        let page = gray_page(|_, _| 250);
        let seed = Mask::filled(REGION);
        let edges = EdgeMap::none(PAGE, PAGE);
        let bounds = Rect::new(0, 0, PAGE, PAGE);
        let automatic = fit(&page, &seed, 1.0, 0.0, &edges, false);
        assert!(automatic.thickness > 4, "fixture must exercise automatic growth");
        for padding in [0, 4, 2, 0] {
            let fitted = fit_with_padding(&page, &seed, 1.0, 0.0, &edges, bounds, padding);
            let expected = seed.dilated(padding, PAGE, PAGE);
            assert_eq!(fitted.mask, expected);
            assert_eq!(fitted.ink, expected);
            assert_eq!(fitted.thickness, padding);
            assert_eq!(fitted.route, automatic.route);
            assert_eq!(fitted.paper, ring::paper_annulus(&page, &expected, 0, &edges));
        }
        let clipped = Rect::new(REGION.x, REGION.y, REGION.w + 1, REGION.h + 1);
        let fitted = fit_with_padding(&page, &seed, 1.0, 0.0, &edges, clipped, 4);
        assert_eq!(fitted.mask, seed.dilated_within(4, clipped));
    }

    #[test]
    fn zero_padding_samples_paper_past_the_glyph_edge_without_growing_the_mask() {
        let seed = Mask::filled(REGION);
        let page = gray_page(|x, y| if seed.contains(x as i64, y as i64) { 30 } else { 250 });
        let edges = EdgeMap::sobel(&page);
        let bounds = Rect::new(0, 0, PAGE, PAGE);
        assert!(ring::paper_annulus(&page, &seed, 0, &edges).is_empty());
        let fitted = fit_with_padding(&page, &seed, 1.0, 0.0, &edges, bounds, 0);
        let stored = fit_within(&page, &seed, 1.0, 0.0, &edges, true, bounds);
        assert_eq!(fitted.mask, seed);
        assert_eq!(fitted.ink, seed);
        assert_eq!(fitted.paper, stored.paper);
        assert_eq!(fitted.ring.median, 250 * 257);
        assert!(fitted.ring.count > 0);
    }

    #[test]
    fn zero_padding_paper_sampling_does_not_cross_a_nearby_outline() {
        let seed = Mask::filled(REGION);
        let outline = REGION.grown(4, PAGE, PAGE);
        let page = gray_page(|x, y| {
            let (x, y) = (x as i64, y as i64);
            if seed.contains(x, y) || ((x == outline.x || x == outline.right() - 1)
                && y >= outline.y && y < outline.bottom())
                || ((y == outline.y || y == outline.bottom() - 1)
                    && x >= outline.x && x < outline.right()) { 30 }
            else if x > outline.x && x < outline.right() - 1
                && y > outline.y && y < outline.bottom() - 1 { 250 }
            else { 150 }
        });
        let edges = EdgeMap::sobel(&page);
        assert!(ring::paper_annulus(&page, &seed, 0, &edges).is_empty());
        let paper = authoritative_paper(&page, &seed, &edges);
        assert!(!paper.is_empty());
        for y in 0..PAGE as i64 {
            for x in 0..PAGE as i64 {
                if paper.contains(x, y) {
                    assert!(x > outline.x && x < outline.right() - 1
                        && y > outline.y && y < outline.bottom() - 1, "sample ({x},{y}) crossed the outline");
                }
            }
        }
    }

    /// A hand-drawn mask is authoritative on both readings: no search runs, and
    /// nothing is held back from the model either.
    #[test]
    fn a_hand_drawn_mask_is_the_set_written_through_exactly() {
        let page = gray_page(|_, _| 250);
        let seed = Mask::filled(REGION);
        let fitted = fit(&page, &seed, 1.0, page_noise_sigma(&page), &EdgeMap::none(PAGE, PAGE), true);
        assert_eq!(fitted.mask, seed);
        assert_eq!(fitted.ink, seed);
        assert_eq!(fitted.thickness, 0);
    }

    /// The old fallback, answered: **the fallback no longer hands a model
    /// the mask that was refused.**
    ///
    /// One black line on flat paper, and a seed close enough to it that the first
    /// step's border lands on the line's gradient. Every candidate is therefore
    /// refused and the fallback runs - and what it returns is the largest growth
    /// *below* the first step whose border clears the edge, rather than the first
    /// step itself.
    #[test]
    fn the_fallback_returns_an_admissible_mask_rather_than_the_refused_one() {
        // The only edge on the page, so it sets the peak the threshold is a
        // fraction of. A 1 px line has zero gradient at its own centre and a
        // strong one at x = 57 and x = 59.
        let page = gray_page(|x, _| if x == 58 { 0 } else { 250 });
        let edges = EdgeMap::sobel(&page);
        assert!(edges.is_strong(57, 50) && !edges.is_strong(56, 50), "the premise moved");

        let seed = Mask::filled(Rect::new(50, 44, 4, 12));
        // The premise: the first step of the series is inadmissible.
        assert!(edges.crosses(&seed.dilated(4, PAGE, PAGE)), "the first step no longer crosses");

        let fitted = fit(&page, &seed, 1.0, page_noise_sigma(&page), &edges, false);
        assert_eq!(fitted.thickness, 3, "the largest admissible growth below the first step");
        assert_eq!(fitted.mask, seed.dilated(3, PAGE, PAGE));
        assert!(!edges.crosses(&fitted.mask), "the mask handed to a model sits on an outline");
        assert_eq!(fitted.ink, fitted.mask, "this mask is already the minimum");
        assert!(fitted.route.needs_a_model(), "the fallback still routes to the ladder");
    }

    /// **The walk stops at the anti-aliasing, not at the seed.** Two-pixel
    /// stripes put a strong edge under every border the seed could grow to, so
    /// every radius is refused - and what comes back is still the seed grown
    /// by one proxy pixel at the page's scale, never the bare seed that left a
    /// glyph's ramp outside the hole.
    #[test]
    fn the_fallback_never_stops_inside_the_lettering_antialiasing() {
        let page = gray_page(|x, _| if x % 4 < 2 { 0 } else { 250 });
        let edges = EdgeMap::sobel(&page);
        let seed = Mask::filled(Rect::new(50, 44, 4, 12));
        for (scale, pad) in [(1.0, 1), (1.5635, 2), (2.0, 2), (3.2, 3)] {
            assert!(edges.crosses(&seed.dilated(1, PAGE, PAGE)), "the premise moved");
            let fitted = fit(&page, &seed, scale, page_noise_sigma(&page), &edges, false);
            assert_eq!(fitted.thickness, pad, "scale {scale}");
            assert_eq!(fitted.mask, seed.dilated(pad, PAGE, PAGE), "scale {scale}");
            assert_eq!(fitted.ink, fitted.mask, "scale {scale}");
        }
    }

    /// The invariant, over every fixture and both paths: what a model writes
    /// through is never larger than what was fitted.
    #[test]
    fn the_written_set_is_a_subset_of_the_fitted_mask_on_every_source() {
        for fixture in fixtures::all() {
            let page = fixture.raster;
            let seed = Mask::filled(Rect::new(24, 18, 12, 10));
            for manual in [false, true] {
                let edges = EdgeMap::sobel(&page);
                let fitted =
                    fit(&page, &seed, 1.0, page_noise_sigma(&page), &edges, manual);
                for y in 0..page.height as i64 {
                    for x in 0..page.width as i64 {
                        assert!(
                            !fitted.ink.contains(x, y) || fitted.mask.contains(x, y),
                            "{} ({x}, {y}): written through, outside the fitted mask",
                            fixture.name
                        );
                    }
                }
            }
        }
    }

    /// An effect on busy art: a black stroke 12 px wide with a white outline
    /// 4 px wide, on stripes whose every border is a strong edge. The fit alone
    /// keeps a hole inside the outline; grown by half its stroke first, the
    /// whole outline is inside the set a model writes through.
    #[test]
    fn an_outlined_effect_on_art_is_cleaned_out_to_its_outline() {
        let stroke = Rect::new(40, 30, 12, 40);
        let outline = Rect::new(36, 26, 20, 48);
        let page = gray_page(|x, y| {
            let (x, y) = (x as i64, y as i64);
            if stroke.contains(x, y) { 0 } else if outline.contains(x, y) { 250 } else if x % 4 < 2 { 0 } else { 250 }
        });
        let edges = EdgeMap::sobel(&page);
        let seed = Mask::filled(stroke);
        let in_outline = |mask: &Mask| (outline.y..outline.bottom())
            .all(|y| (outline.x..outline.right()).all(|x| mask.contains(x, y)));
        let bare = fit(&page, &seed, 1.0, page_noise_sigma(&page), &edges, false);
        assert!(!in_outline(&bare.ink), "the premise moved: the fit alone reaches the outline's edge");

        let grown = outlined(&seed, 1.0, Rect::new(0, 0, PAGE, PAGE));
        assert_eq!(grown, seed.dilated(5, PAGE, PAGE), "half of a 9.6 px stroke, rounded");
        let fitted = fit(&page, &grown, 1.0, page_noise_sigma(&page), &edges, false);
        assert!(in_outline(&fitted.ink), "part of the outline is left standing");
        assert!(fitted.route.needs_a_model(), "an effect on art is a model rung's");
    }

    /// Half the stroke, never more than [`OUTLINE_MAX`] proxy pixels at the
    /// page's scale, and nothing for nothing.
    #[test]
    fn outline_growth_is_half_the_stroke_up_to_a_bound() {
        let bounds = Rect::new(0, 0, PAGE, PAGE);
        let thin = Mask::filled(Rect::new(40, 10, 4, 80));
        assert_eq!(outlined(&thin, 1.0, bounds), thin.dilated(2, PAGE, PAGE));
        let wide = Mask::filled(Rect::new(20, 20, 60, 60));
        assert_eq!(outlined(&wide, 1.0, bounds), wide.dilated(OUTLINE_MAX, PAGE, PAGE));
        assert_eq!(outlined(&wide, 1.5, bounds), wide.dilated(12, PAGE, PAGE));
        let empty = Mask::empty(Rect::new(10, 10, 5, 5));
        assert_eq!(outlined(&empty, 1.0, bounds), empty);
    }
}
