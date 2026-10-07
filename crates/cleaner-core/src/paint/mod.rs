//! Painting: a stroke, and the patch it becomes.
//!
//! **No new patch kind and no new fields.** A
//! painted stroke leaves the same thing every other rung leaves: an opaque
//! [`Raster`] and a binary [`Mask`]. The soft edge is resolved *here*, before
//! the patch exists - the surface under the stroke is composited, the coverage
//! is blended over it in f32, and what comes out is an ordinary replacement
//! patch. So `composite`, the exporter, `tile.rs`, `history.rs` and the
//! fidelity contract all keep working with nothing changed.
//!
//! **What that costs is written down rather than hidden.** Because the blend
//! happens at commit, an anti-aliased paint patch B over an earlier patch A
//! carries A's pixels inside B's semi-transparent rim. Undoing B is exact;
//! undoing **A** leaves a one-to-two-pixel ghost of A's pixels inside B's rim,
//! and only where the two masks overlap. That is the price of keeping
//! `composite` a replacement and the manifest schema unchanged;
//! true parametric independence would need a recompute pass over the patches
//! whose `order` sits above a deleted one.
//!
//! ## Modes
//!
//! Phase 1 serves `Gray8`, `GrayAlpha8`, `Rgb8` and `Rgba8`. Everything else is
//! [`PaintError::UnsupportedMode`], which the shell reports as
//! `decline.reason.paintUnsupportedMode` rather than promoting the page - no
//! helpful promotion. 16-bit, indexed, bitonal and CMYK are Phase 2.
//!
//! UI colors are sRGB; coverage blends in linear-light sRGB before conversion
//! back to native channels. Alpha and every uncovered native sample are copied.

pub mod brush;
pub mod plan;

use crate::composite::{composite_region, CompositeError};
use crate::image::{BitDepth, ColorMode, Raster};
use crate::mask::{Mask, Rect};
use crate::patch::Patch;

use brush::{AccumulationView, RasterizableDab, StampOptions, TipShape};
pub use plan::{BrushSpec, StrokePoint};

/// The angle every Phase 1 dab is stamped at: none.
///
/// The kernel takes `cos(-angle)`/`sin(-angle)` precomputed rather than doing
/// trigonometry itself - that is its cross-runtime determinism contract - so a
/// round tip at zero rotation is these two constants and no `cos` call.
const COS_ZERO: f64 = 1.0;
const SIN_ZERO: f64 = -0.0;

/// The slack around a stroke's box, in page pixels. The outermost dab's soft
/// rim has to have surface under it to blend against.
const EDGE_SLACK: f64 = 2.0;

#[derive(Debug, thiserror::Error)]
pub enum PaintError {
    /// The page is not one of Phase 1's four modes. Carries no detail on
    /// purpose: the shell answers one `decline.reason.*` key, and a key is the
    /// whole of what crosses the seam.
    #[error("painting is not supported on a {mode:?}/{depth:?} page")]
    UnsupportedMode { mode: ColorMode, depth: BitDepth },
    /// The stroke covered no pixel of the page - every point off the edge, or a
    /// brush whose dabs all fell outside it. Not an error the user caused;
    /// the caller declines rather than committing an empty patch.
    #[error("the stroke covered nothing")]
    Empty,
    /// A clone whose whole source lies off the page: there is nothing to copy.
    #[error("the clone source is outside the page")]
    SourceOutside,
    #[error(transparent)]
    Color(#[from] crate::image::ImageError),
    #[error(transparent)]
    Composite(#[from] CompositeError),
}

/// What one painted stroke produced: the applied mask, and the pixels covering
/// its bounds in the page's own mode and depth.
pub struct Painted {
    pub mask: Mask,
    pub pixels: Raster,
    /// The composited page rectangle sampled to render this stroke.
    pub read_window: Rect,
    /// How many dabs the planner emitted. Provenance, not geometry - it is the
    /// one number that says how much of the stroke the commit actually walked.
    pub dabs: usize,
}

/// Whether this page can be painted on at all.
pub fn supports(page: &Raster) -> bool {
    page.depth == BitDepth::Eight
        && matches!(
            page.mode,
            ColorMode::Gray | ColorMode::GrayAlpha | ColorMode::Rgb | ColorMode::Rgba
        )
}

fn require_supported(page: &Raster) -> Result<(), PaintError> {
    crate::image::color::validate_editing(page)?;
    if supports(page) {
        crate::image::color::ManagedColor::for_raster(page)?;
        Ok(())
    } else {
        Err(PaintError::UnsupportedMode {
            mode: page.mode,
            depth: page.depth,
        })
    }
}

/* ------------------------------------------------------------------ */
/* The brush                                                           */
/* ------------------------------------------------------------------ */

/// Paint one stroke and answer the patch it makes.
///
/// `patches` are the page's patches; `below` is the exclusive `order` ceiling
/// that says which of them are *under* this stroke. `color` is straight sRGB
/// bytes, as the `#rrggbb` the seam carries.
pub fn render_brush(
    page: &Raster,
    patches: &[Patch],
    below: Option<u32>,
    points: &[StrokePoint],
    spec: &BrushSpec,
    color: [u8; 3],
) -> Result<Painted, PaintError> {
    require_supported(page)?;

    let dabs = plan::plan_stroke(points, spec);
    let Some(window) = window_of(&dabs, page) else {
        return Err(PaintError::Empty);
    };

    let surface = composite_region(page, patches, window, below)?;
    let (w, h) = (window.w as usize, window.h as usize);
    let managed = crate::image::color::ManagedColor::for_raster(page)?;

    let mut acc = vec![0f32; w * h * 4];
    let mut coverage = vec![0f32; w * h];
    let mut view = AccumulationView {
        color: &mut acc,
        stroke_mask: &mut coverage,
        width: w,
        height: h,
    };
    let options = StampOptions {
        opacity: (spec.opacity / 100.0).clamp(0.0, 1.0),
        hardness: (spec.hardness / 100.0).clamp(0.0, 1.0),
        seed: spec.seed,
        ..StampOptions::default()
    };
    let dab_color = color.map(|v| crate::image::color::linear(v as f32 / 255.0) as f64);
    for dab in &dabs {
        let local = RasterizableDab {
            x: dab.x - window.x as f64,
            y: dab.y - window.y as f64,
            ..*dab
        };
        brush::stamp_dab_subpixel(
            &mut view,
            &local,
            TipShape::Analytic,
            dab_color,
            &options,
            COS_ZERO,
            SIN_ZERO,
        );
    }

    let mut blended = surface.clone();
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let a = acc[i * 4 + 3].clamp(0.0, 1.0);
            if a <= 0.0 || crate::image::color::alpha(&surface, x as u32, y as u32) == 0.0 {
                coverage[i] = 0.0;
                continue;
            }
            let rgb = managed.pixel(&surface, x as u32, y as u32);
            let rgb = std::array::from_fn(|c| {
                crate::image::color::encoded(
                    acc[i * 4 + c] + crate::image::color::linear(rgb[c]) * (1.0 - a),
                )
            });
            write_managed(
                &mut blended,
                x as u32,
                y as u32,
                managed.from_srgb(rgb, page.mode)?,
            );
        }
    }
    finish(page, window, &blended, &coverage, dabs.len())
}

/* ------------------------------------------------------------------ */
/* The clone stamp                                                     */
/* ------------------------------------------------------------------ */

/// Coverage at or above this is a straight copy of the source's own samples.
/// A hard dab at full flow and opacity lands exactly 1.0; the slack is for a
/// low flow that has built up to within rounding of it.
const OPAQUE: f32 = 0.9999;

/// Clone one stroke: copy the page from `offset` away, through the brush.
///
/// `offset` is `target − source` in **native page pixels**, so a pixel at `t`
/// reads from `t − offset`. **The seam spells the same displacement the other
/// way round** - `gesture.js#cloneOffset` sends `source − strokeStart` and
/// rebuilds the source as `strokeStart + offset` - and the negation happens once,
/// at the parse site (`region::clone_offset`). This function only ever sees the
/// internal direction, so the sign question has exactly one place to be wrong in.
/// It is rounded to whole pixels here, once: a clone is a copy, and a copy that
/// lands between pixels is a blur.
///
/// **The source is the page as it stood before the stroke.** Every dab reads
/// the same frozen surface, so a stroke that runs over its own source copies
/// what was there and never its own fresh paint.
///
/// **Coverage first, then one blend.** One stroke holds one offset, so each
/// target pixel has exactly one source pixel and the dabs only decide how much
/// of it lands. They are stamped with the Brush's own tip, so size, hardness,
/// spacing and pen pressure mean here what they mean there. `flow` builds:
/// overlapping dabs add up towards `opacity`, which is the ceiling for the
/// whole stroke however often it crosses itself.
///
/// Where the source falls off the page there is nothing to copy, and the
/// target is left alone and out of the mask.
pub fn render_clone(
    page: &Raster,
    patches: &[Patch],
    below: Option<u32>,
    points: &[StrokePoint],
    spec: &BrushSpec,
    offset: (f64, f64),
) -> Result<Painted, PaintError> {
    require_supported(page)?;

    let dabs = plan::plan_stroke(points, spec);
    let Some(target) = window_of(&dabs, page) else {
        return Err(PaintError::Empty);
    };
    let (dx, dy) = (offset.0.round() as i64, offset.1.round() as i64);
    let reads = clamp_to_page(
        Rect::new(target.x - dx, target.y - dy, target.w, target.h),
        page,
    );
    if reads.w == 0 || reads.h == 0 {
        return Err(PaintError::SourceOutside);
    }

    let surface = composite_region(page, patches, target, below)?;
    let source = composite_region(page, patches, reads, below)?;
    let (w, h) = (target.w as usize, target.h as usize);
    let managed = crate::image::color::ManagedColor::for_raster(page)?;

    // The colour half of the buffer is the kernel's and goes unread: what a
    // clone lays down is not one colour, so only the coverage is kept.
    let mut unused = vec![0f32; w * h * 4];
    let mut coverage = vec![0f32; w * h];
    let mut view = AccumulationView {
        color: &mut unused,
        stroke_mask: &mut coverage,
        width: w,
        height: h,
    };
    let options = StampOptions {
        opacity: (spec.opacity / 100.0).clamp(0.0, 1.0),
        hardness: (spec.hardness / 100.0).clamp(0.0, 1.0),
        build_up: true,
        seed: spec.seed,
        ..StampOptions::default()
    };
    for dab in &dabs {
        let local = RasterizableDab {
            x: dab.x - target.x as f64,
            y: dab.y - target.y as f64,
            ..*dab
        };
        brush::stamp_dab_subpixel(
            &mut view,
            &local,
            TipShape::Analytic,
            [0.0; 3],
            &options,
            COS_ZERO,
            SIN_ZERO,
        );
    }

    let mut out = surface.clone();
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let a = coverage[i].clamp(0.0, 1.0);
            if a <= 0.0 {
                continue;
            }
            let (sx, sy) = (target.x + x as i64 - dx, target.y + y as i64 - dy);
            let (x, y) = (x as u32, y as u32);
            if !reads.contains(sx, sy) || crate::image::color::alpha(&surface, x, y) == 0.0 {
                coverage[i] = 0.0;
                continue;
            }
            let (sx, sy) = ((sx - reads.x) as u32, (sy - reads.y) as u32);
            if crate::image::color::alpha(&source, sx, sy) == 0.0 {
                coverage[i] = 0.0;
                continue;
            }
            if a >= OPAQUE {
                for c in 0..page.mode.samples() {
                    if page.mode.alpha_channel() != Some(c) {
                        out.set_sample(x, y, c, source.sample(sx, sy, c));
                    }
                }
                continue;
            }
            let from = managed.pixel(&source, sx, sy).map(crate::image::color::linear);
            let onto = managed.pixel(&surface, x, y).map(crate::image::color::linear);
            let rgb = std::array::from_fn(|c| {
                crate::image::color::encoded(from[c] * a + onto[c] * (1.0 - a))
            });
            write_managed(&mut out, x, y, managed.from_srgb(rgb, page.mode)?);
        }
    }

    let mut painted = finish(page, target, &out, &coverage, dabs.len())?;
    // Everything the stroke depended on: where it landed and where it read.
    painted.read_window = bounding(target, reads);
    Ok(painted)
}

/* ------------------------------------------------------------------ */
/* Shared: the window, and the way back to the page's own samples      */
/* ------------------------------------------------------------------ */

/// The stroke's box, grown by the dabs' reach and clamped to the page.
fn window_of(dabs: &[RasterizableDab], page: &Raster) -> Option<Rect> {
    let (x0, y0, x1, y1) = plan::bounds_of(dabs, EDGE_SLACK)?;
    let x0 = x0.floor().clamp(0.0, page.width as f64) as i64;
    let y0 = y0.floor().clamp(0.0, page.height as f64) as i64;
    let x1 = x1.ceil().clamp(x0 as f64, page.width as f64) as i64;
    let y1 = y1.ceil().clamp(y0 as f64, page.height as f64) as i64;
    (x1 > x0 && y1 > y0).then(|| Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
}

fn clamp_to_page(rect: Rect, page: &Raster) -> Rect {
    let x0 = rect.x.clamp(0, page.width as i64);
    let y0 = rect.y.clamp(0, page.height as i64);
    let x1 = rect.right().clamp(x0, page.width as i64);
    let y1 = rect.bottom().clamp(y0, page.height as i64);
    Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32)
}

fn bounding(a: Rect, b: Rect) -> Rect {
    let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
    let (x1, y1) = (a.right().max(b.right()), a.bottom().max(b.bottom()));
    Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32)
}

/// The mask is the support of the coverage, and the patch is the blended RGBA
/// cropped to it and folded back into the page's own mode and depth.
fn finish(
    page: &Raster,
    window: Rect,
    blended: &Raster,
    coverage: &[f32],
    dabs: usize,
) -> Result<Painted, PaintError> {
    let (w, h) = (window.w as usize, window.h as usize);
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
    for y in 0..h {
        for x in 0..w {
            if coverage[y * w + x] > 0.0 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    if x0 == usize::MAX {
        return Err(PaintError::Empty);
    }

    let bounds = Rect::new(
        window.x + x0 as i64,
        window.y + y0 as i64,
        (x1 - x0) as u32,
        (y1 - y0) as u32,
    );
    let mut mask = Mask::empty(bounds);
    let mut pixels = Raster {
        width: bounds.w,
        height: bounds.h,
        mode: page.mode,
        depth: page.depth,
        icc: page.icc.clone(),
        palette: page.palette.clone(),
        trns: page.trns.clone(),
        srgb_intent: page.srgb_intent,
        color: page.color.clone(),
        data: vec![0; crate::composite::blank_len(bounds.w, bounds.h, page)],
    };
    for y in y0..y1 {
        for x in x0..x1 {
            let at = y * w + x;
            let (lx, ly) = ((x - x0) as u32, (y - y0) as u32);
            if coverage[at] > 0.0 {
                mask.set(bounds.x + lx as i64, bounds.y + ly as i64, true);
            }
            for c in 0..page.mode.samples() {
                pixels.set_sample(lx, ly, c, blended.sample(x as u32, y as u32, c));
            }
        }
    }

    Ok(Painted { mask, pixels, read_window: window, dabs })
}

fn write_managed(pixels: &mut Raster, x: u32, y: u32, values: [f32; 4]) {
    for (c, value) in values.iter().enumerate().take(pixels.mode.samples()) {
        if pixels.mode.alpha_channel() != Some(c) {
            pixels.set_sample(x, y, c, crate::image::color::byte(*value) as u16);
        }
    }
    crate::image::color::avoid_transparent_key(pixels, x, y);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::fixtures;

    fn stroke(points: &[(f64, f64)]) -> Vec<StrokePoint> {
        points
            .iter()
            .map(|(x, y)| StrokePoint {
                x: *x,
                y: *y,
                p: 1.0,
            })
            .collect()
    }

    fn hard_brush(size: f64) -> BrushSpec {
        BrushSpec {
            size,
            hardness: 100.0,
            flow: 100.0,
            opacity: 100.0,
            spacing: 10.0,
            pressure_size: false,
            pressure_opacity: false,
            seed: 7,
        }
    }

    #[test]
    fn selected_srgb_color_is_converted_to_linear_source_samples() {
        let mut page = fixtures::by_name("rgb8").raster;
        page.color.gamma = Some(100000);
        let painted = render_brush(
            &page,
            &[],
            None,
            &stroke(&[(10.0, 10.0)]),
            &hard_brush(7.0),
            [128, 128, 128],
        )
        .unwrap();
        let x = (10 - painted.mask.bounds.x) as u32;
        let y = (10 - painted.mask.bounds.y) as u32;
        // IEC sRGB decode of 128/255 is 0.21586 -> 55 in linear RGB8.
        for c in 0..3 {
            assert!((painted.pixels.sample(x, y, c) as i32 - 55).abs() <= 1);
        }
    }
    #[test]
    fn half_coverage_blends_in_linear_light_and_keeps_uncovered_native_bytes() {
        let mut page = fixtures::by_name("rgb8").raster;
        page.data.fill(0);
        let mut spec = hard_brush(7.0);
        spec.opacity = 50.0;
        let painted = render_brush(
            &page,
            &[],
            None,
            &stroke(&[(10.0, 10.0)]),
            &spec,
            [255, 255, 255],
        )
        .unwrap();
        let x = (10 - painted.mask.bounds.x) as u32;
        let y = (10 - painted.mask.bounds.y) as u32;
        assert_eq!(painted.pixels.sample(x, y, 0), 188);
        for y in 0..painted.pixels.height {
            for x in 0..painted.pixels.width {
                if !painted.mask.contains(
                    painted.mask.bounds.x + x as i64,
                    painted.mask.bounds.y + y as i64,
                ) {
                    for c in 0..3 {
                        assert_eq!(painted.pixels.sample(x, y, c), 0);
                    }
                }
            }
        }
    }
    #[test]
    fn opaque_clone_preserves_native_values_even_in_non_srgb_space() {
        let mut page = fixtures::by_name("rgb8").raster;
        page.color.gamma = Some(100000);
        let painted = render_clone(
            &page,
            &[],
            None,
            &stroke(&[(12.0, 10.0)]),
            &hard_brush(7.0),
            (6.0, 0.0),
        )
        .unwrap();
        let x = (12 - painted.mask.bounds.x) as u32;
        let y = (10 - painted.mask.bounds.y) as u32;
        for c in 0..3 {
            assert_eq!(painted.pixels.sample(x, y, c), page.sample(6, 10, c));
        }
    }

    /// The four Phase 1 modes are served and every other one is refused by
    /// name rather than promoted. This is the whole of `decline.reason
    /// .paintUnsupportedMode`'s population, computed from the fixtures rather
    /// than typed out, so a new fixture mode lands on one side or the other
    /// without this test being edited.
    #[test]
    fn phase_one_serves_four_modes_and_declines_the_rest() {
        for fixture in fixtures::all() {
            let page = &fixture.raster;
            let expected = page.depth == BitDepth::Eight
                && matches!(
                    page.mode,
                    ColorMode::Gray | ColorMode::GrayAlpha | ColorMode::Rgb | ColorMode::Rgba
                );
            assert_eq!(supports(page), expected, "{}", fixture.name);

            let painted = render_brush(
                page,
                &[],
                None,
                &stroke(&[(4.0, 4.0), (10.0, 6.0)]),
                &hard_brush(5.0),
                [255, 0, 0],
            );
            match painted {
                Err(PaintError::UnsupportedMode { .. }) => assert!(!expected, "{}", fixture.name),
                other => assert!(expected && other.is_ok(), "{}", fixture.name),
            }
        }
    }

    /// **The patch is a replacement and the mask is its support.** Both halves
    /// matter: pixels covering `mask.bounds`, so `Patch::is_well_formed` holds
    /// and the compositor can place it, and a mask that is not empty, so
    /// something actually changes.
    #[test]
    fn a_stroke_becomes_a_well_formed_patch_in_the_pages_own_mode() {
        for name in ["l8", "rgb8"] {
            let page = fixtures::by_name(name).raster;
            let painted = render_brush(
                &page,
                &[],
                None,
                &stroke(&[(3.0, 3.0), (12.0, 9.0)]),
                &hard_brush(4.0),
                [12, 200, 40],
            )
            .unwrap();
            assert_eq!(painted.pixels.width, painted.mask.bounds.w, "{name}");
            assert_eq!(painted.pixels.height, painted.mask.bounds.h, "{name}");
            assert_eq!(painted.pixels.mode, page.mode, "{name}");
            assert_eq!(painted.pixels.depth, page.depth, "{name}");
            assert!(!painted.mask.is_empty(), "{name}");
            assert!(painted.dabs >= 2, "{name}");
        }
    }

    /// A fully opaque hard dab lands the colour it was given, and on a gray
    /// page it lands that colour's 709 luma. The centre pixel is the one the
    /// blend cannot have softened.
    #[test]
    fn an_opaque_hard_dab_writes_the_colour_it_was_asked_for() {
        let page = fixtures::by_name("rgb8").raster;
        let painted = render_brush(
            &page,
            &[],
            None,
            &stroke(&[(8.0, 8.0)]),
            &hard_brush(6.0),
            [10, 20, 30],
        )
        .unwrap();
        let (lx, ly) = (
            (8 - painted.mask.bounds.x) as u32,
            (8 - painted.mask.bounds.y) as u32,
        );
        assert_eq!(
            [
                painted.pixels.sample(lx, ly, 0),
                painted.pixels.sample(lx, ly, 1),
                painted.pixels.sample(lx, ly, 2)
            ],
            [10, 20, 30]
        );

        let gray = fixtures::by_name("l8").raster;
        let painted = render_brush(
            &gray,
            &[],
            None,
            &stroke(&[(8.0, 8.0)]),
            &hard_brush(6.0),
            [10, 20, 30],
        )
        .unwrap();
        let (lx, ly) = (
            (8 - painted.mask.bounds.x) as u32,
            (8 - painted.mask.bounds.y) as u32,
        );
        assert_eq!(painted.pixels.sample(lx, ly, 0), 19);
    }

    /// **The gray fold commutes with the blend.** A neutral colour painted at
    /// any coverage onto a gray page must not tint anything, and a pixel the
    /// stroke did not cover must come back the sample it went in as. This is
    /// the property the module's Rec. 709-on-encoded-samples choice buys, and
    /// the reason the choice is not simply "whatever luma".
    #[test]
    fn a_neutral_colour_on_a_gray_page_leaves_uncovered_samples_alone() {
        let page = fixtures::by_name("l8").raster;
        let soft = BrushSpec {
            hardness: 0.0,
            ..hard_brush(7.0)
        };
        let painted = render_brush(
            &page,
            &[],
            None,
            &stroke(&[(9.0, 9.0)]),
            &soft,
            [128, 128, 128],
        )
        .unwrap();
        let bounds = painted.mask.bounds;
        for y in 0..bounds.h {
            for x in 0..bounds.w {
                let (px, py) = (bounds.x as u32 + x, bounds.y as u32 + y);
                if !painted.mask.contains(px as i64, py as i64) {
                    assert_eq!(
                        painted.pixels.sample(x, y, 0),
                        page.sample(px, py, 0),
                        "({px},{py}) outside the mask moved"
                    );
                }
            }
        }
    }

    /// Alpha is copied, never produced. A stroke over an `RGBA` page carries
    /// the source's alpha into the patch unchanged, whatever the brush's own
    /// opacity was.
    #[test]
    fn alpha_is_carried_through_rather_than_painted() {
        let page = fixtures::by_name("rgba8").raster;
        let spec = BrushSpec {
            opacity: 60.0,
            ..hard_brush(6.0)
        };
        let painted = render_brush(
            &page,
            &[],
            None,
            &stroke(&[(6.0, 6.0), (11.0, 11.0)]),
            &spec,
            [0, 0, 0],
        )
        .unwrap();
        let bounds = painted.mask.bounds;
        for y in 0..bounds.h {
            for x in 0..bounds.w {
                assert_eq!(
                    painted.pixels.sample(x, y, 3),
                    page.sample(bounds.x as u32 + x, bounds.y as u32 + y, 3)
                );
            }
        }
    }

    /// An empty stroke is `Empty` rather than a patch with nothing in it, and a
    /// stroke entirely off the page is the same answer - a caller that
    /// committed either would put a zero-area record in the manifest.
    #[test]
    fn a_stroke_that_covers_nothing_is_refused_rather_than_committed() {
        let page = fixtures::by_name("l8").raster;
        assert!(matches!(
            render_brush(&page, &[], None, &[], &hard_brush(5.0), [0, 0, 0]),
            Err(PaintError::Empty)
        ));
        assert!(matches!(
            render_brush(
                &page,
                &[],
                None,
                &stroke(&[(-900.0, -900.0)]),
                &hard_brush(5.0),
                [0, 0, 0]
            ),
            Err(PaintError::Empty)
        ));
    }

    /// A gray page that is black on the left half and white on the right, so
    /// a clone across the middle has a source and a target that differ.
    fn split_page() -> Raster {
        let mut page = fixtures::by_name("l8").raster;
        for y in 0..page.height {
            for x in 0..page.width {
                page.set_sample(x, y, 0, if x < 32 { 0 } else { 255 });
            }
        }
        page
    }

    /// Every pixel a clone landed is the source's own sample, moved by `offset`.
    fn assert_copied(page: &Raster, painted: &Painted, offset: (i64, i64)) {
        let bounds = painted.mask.bounds;
        let mut copied = 0;
        for y in 0..bounds.h {
            for x in 0..bounds.w {
                let (px, py) = (bounds.x + x as i64, bounds.y + y as i64);
                if !painted.mask.contains(px, py) {
                    continue;
                }
                copied += 1;
                for c in 0..page.mode.samples() {
                    assert_eq!(
                        painted.pixels.sample(x, y, c),
                        page.sample((px - offset.0) as u32, (py - offset.1) as u32, c),
                        "({px},{py}) channel {c}"
                    );
                }
            }
        }
        assert!(copied > 0);
    }

    /// **A clone is a copy.** At full hardness, flow and opacity every pixel
    /// under the brush is the source's own sample - the whole disc, not a soft
    /// cone of which only the centre survives.
    #[test]
    fn a_hard_clone_copies_the_source_under_the_whole_brush() {
        let page = fixtures::by_name("rgb8").raster;
        let painted = render_clone(
            &page,
            &[],
            None,
            &stroke(&[(30.0, 20.0), (36.0, 20.0)]),
            &hard_brush(9.0),
            (12.0, -5.0),
        )
        .unwrap();
        assert_copied(&page, &painted, (12, -5));
        for x in 27..=39 {
            assert!(painted.mask.contains(x, 20), "column {x} is under the brush");
        }
        assert_eq!(painted.pixels.mode, page.mode);
        assert_eq!(painted.pixels.width, painted.mask.bounds.w);
    }

    /// The offset is a whole number of pixels for the whole stroke. A
    /// fractional one used to be rounded dab by dab, which moved the source a
    /// pixel back and forth along the stroke.
    #[test]
    fn a_fractional_offset_is_one_whole_pixel_offset_for_the_stroke() {
        let page = fixtures::by_name("rgb8").raster;
        let painted = render_clone(
            &page,
            &[],
            None,
            &stroke(&[(30.2, 20.7), (41.9, 23.4)]),
            &hard_brush(7.0),
            (10.4, 6.6),
        )
        .unwrap();
        assert_copied(&page, &painted, (10, 7));
    }

    /// **The source is frozen when the stroke starts.** A stroke that runs
    /// over the place it reads from copies what was there before it, never
    /// the paint it has just laid down.
    #[test]
    fn a_stroke_over_its_own_source_copies_the_page_as_it_was() {
        let page = fixtures::by_name("rgb8").raster;
        let painted = render_clone(
            &page,
            &[],
            None,
            &stroke(&[(20.0, 24.0), (44.0, 24.0)]),
            &hard_brush(7.0),
            (4.0, 0.0),
        )
        .unwrap();
        assert_copied(&page, &painted, (4, 0));
    }

    /// **Opacity is the ceiling of the stroke.** Dabs a tenth of a diameter
    /// apart overlap ten deep, and a 50% clone is still half source and half
    /// page at the end. It used to multiply into each dab and build to a full
    /// copy.
    #[test]
    fn opacity_caps_the_stroke_however_many_dabs_overlap() {
        let page = split_page();
        let spec = BrushSpec {
            opacity: 50.0,
            ..hard_brush(9.0)
        };
        let painted = render_clone(
            &page,
            &[],
            None,
            &stroke(&[(40.0, 24.0), (56.0, 24.0)]),
            &spec,
            (30.0, 0.0),
        )
        .unwrap();
        let bounds = painted.mask.bounds;
        // Half black over white, mixed in linear light: 188, the number the
        // Brush's own half-coverage test finds.
        for x in 40..=56 {
            let value = painted
                .pixels
                .sample((x - bounds.x) as u32, (24 - bounds.y) as u32, 0);
            assert_eq!(value, 188, "column {x}");
        }
    }

    /// **Flow builds, towards the opacity.** One dab at 20% flow is a fifth of
    /// a copy; a stroke of overlapping dabs is more, and never the whole.
    #[test]
    fn flow_builds_up_where_dabs_overlap() {
        let page = split_page();
        let spec = BrushSpec {
            flow: 20.0,
            ..hard_brush(9.0)
        };
        let read = |points: &[(f64, f64)]| {
            let painted =
                render_clone(&page, &[], None, &stroke(points), &spec, (30.0, 0.0)).unwrap();
            let bounds = painted.mask.bounds;
            painted
                .pixels
                .sample((48 - bounds.x) as u32, (24 - bounds.y) as u32, 0)
        };
        // White paper going towards a black source: darker is more copy.
        let tap = read(&[(48.0, 24.0)]);
        let drag = read(&[(46.0, 24.0), (50.0, 24.0)]);
        assert!(tap < 255, "{tap}");
        assert!(drag < tap, "{tap} then {drag}");
        assert!(drag > 0, "{drag}");
    }

    /// **Hardness is the Brush's hardness.** A soft tip fades to its rim and a
    /// hard one does not; the old clone ignored the setting.
    #[test]
    fn a_soft_clone_fades_towards_its_rim() {
        let page = split_page();
        let spec = BrushSpec {
            hardness: 0.0,
            ..hard_brush(15.0)
        };
        let painted =
            render_clone(&page, &[], None, &stroke(&[(48.0, 24.0)]), &spec, (30.0, 0.0)).unwrap();
        let bounds = painted.mask.bounds;
        let at = |x: i64| {
            painted
                .pixels
                .sample((x - bounds.x) as u32, (24 - bounds.y) as u32, 0)
        };
        assert_eq!(at(48), 0, "the centre is a full copy");
        assert!(at(51) > at(48) && at(54) > at(51), "{} {} {}", at(48), at(51), at(54));
    }

    /// Where the source runs off the page there is nothing to copy: those
    /// pixels stay as they were and out of the mask, and a stroke whose whole
    /// source is outside is refused by name rather than committed as a patch
    /// that changes nothing.
    #[test]
    fn a_source_off_the_page_copies_nothing() {
        let page = fixtures::by_name("rgb8").raster;
        let partly = render_clone(
            &page,
            &[],
            None,
            &stroke(&[(10.0, 10.0)]),
            &hard_brush(9.0),
            (12.0, 0.0),
        )
        .unwrap();
        assert_copied(&page, &partly, (12, 0));
        assert!(!partly.mask.contains(8, 10), "its source is at x = -4");
        assert!(partly.mask.contains(13, 10));

        assert!(matches!(
            render_clone(
                &page,
                &[],
                None,
                &stroke(&[(2.0, 2.0)]),
                &hard_brush(5.0),
                (40.0, 40.0),
            ),
            Err(PaintError::SourceOutside)
        ));
    }

    /// What the stroke depended on is where it landed and where it read, as
    /// one rectangle: the provenance digest is taken over it.
    #[test]
    fn the_read_window_spans_the_target_and_the_source() {
        let page = fixtures::by_name("l8").raster;
        let painted = render_clone(
            &page,
            &[],
            None,
            &stroke(&[(40.0, 30.0)]),
            &hard_brush(5.0),
            (25.0, 10.0),
        )
        .unwrap();
        let window = painted.read_window;
        assert!(window.contains(40, 30) && window.contains(15, 20));
        assert_eq!(painted.pixels.mode, ColorMode::Gray);
    }
}
