//! The Patch tool (`J`) and the Gradient tool (`G`): two more kinds of paint.
//!
//! Both are hand tools on the paint branch of [`super::edit`], beside the
//! brush, the stamp, a solid shape and Region copy, and for the reason those
//! are there: the user has said what goes where, so there is no mask to fit
//! and no quality verdict to ask for. What they add is arithmetic from
//! `cleaner_core::engines` (plan: `docs/patch-gradient-plan.md`, features 2
//! and 3).
//!
//! - **Patch** fills a selection from the page. Normal is a Poisson blend of a
//!   source the user drags to, or the one [`propose_at`] found
//!   ([`cleaner_core::engines::patchsource`]); Content-Aware is the PatchMatch
//!   completion; Smooth is the membrane fill, the "drag onto white paper"
//!   trick without the paper; Auto is the screentone fill where the ring is a
//!   screen and Content-Aware where it is not. Its output is read from the
//!   page, so it records [`Engine::Clone`] with `mode: "patch"`, as Region
//!   copy records the paint engine with a mode of its own rather than a new
//!   engine.
//! - **Gradient** fills a selection with given pixels: one of the five
//!   Photoshop gradients, its opacity stops blended over the page, or the
//!   page's own dots at the gradient's brightness ("As screentone"). It
//!   records [`Engine::Paint`] with `mode: "gradient"` and the gradient
//!   itself, so the layer can be reopened. Its **Auto** type has no line and
//!   no stops: the surface the page has all round the selection, blended in
//!   ([`cleaner_core::engines::autogradient`]); it records `auto: true`.
//!
//! Neither runs on an indexed, CMYK or sub-byte page: they refuse with
//! `decline.reason.paintUnsupportedMode`, the brush's refusal. 8 and 16 bit
//! gray and RGB, with or without alpha, are worked at 0 to 255 and written
//! back at the page's depth; alpha is copied through.
//!
//! Every coordinate on the seam is page percent, as the other gestures'. The
//! parameters are recorded as they arrived, before a strip window moved
//! them, so the snapshot always reads in the anchor page's own percent.

use cleaner_core::engines::autogradient;
use cleaner_core::engines::gradient::{self, Gradient};
use cleaner_core::engines::grid::{Grid, Mask as GridMask};
use cleaner_core::engines::patchmatch::{self, Image};
use cleaner_core::engines::{patchsource, poisson, screentone};
use cleaner_core::image::color::ManagedColor;
use cleaner_core::image::{BitDepth, ColorMode};
use serde::Serialize;

use super::*;

/// The Patch tool's **Patch** option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PatchMode {
    /// Content-Aware, or the screentone fill when the ring is a screen.
    Auto,
    /// Poisson: the source's detail, the destination's edge.
    Normal,
    /// PatchMatch completion from the page around the selection.
    ContentAware,
    /// The membrane fill from the selection's edge, no source.
    Smooth,
}

impl PatchMode {
    fn key(self) -> &'static str {
        match self {
            PatchMode::Auto => "auto",
            PatchMode::Normal => "normal",
            PatchMode::ContentAware => "contentAware",
            PatchMode::Smooth => "smooth",
        }
    }
}

/// What a Patch commit asks for, besides where.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct PatchOptions {
    pub(super) mode: PatchMode,
    /// `true` is Photoshop's Destination: the selection's content goes to
    /// where it was dragged. `false`, Source, fills the selection from there.
    pub(super) destination: bool,
    /// 1 to 7: how far the edge correction carries in ([`poisson::diffusion_reach`]).
    pub(super) diffusion: u8,
    /// 1 to 7: the Content-Aware patch size, `2 * structure + 3` px.
    pub(super) structure: u8,
    /// 0 to 10: recorded. PatchMatch here matches colour exactly, so there is
    /// no adaptation for it to loosen.
    pub(super) color: u8,
    /// From the selection to where it was dragged, in page percent.
    pub(super) offset: (f64, f64),
}

/// What a Gradient commit asks for, besides where.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct GradientOptions {
    /// `start` and `end` in page percent. `None` for a payload that did not
    /// parse, which renders nothing rather than falling to the ladder.
    pub(super) gradient: Option<Gradient>,
    pub(super) as_screentone: bool,
    /// The Auto type: the surface round the selection, `gradient` unread.
    pub(super) auto: bool,
}

/* ------------------------------------------------------------------ */
/* Parsing                                                             */
/* ------------------------------------------------------------------ */

/// The parameters as they arrived, for the snapshot.
fn recorded(params: &serde_json::Value, keys: &[&str]) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for key in keys {
        if let Some(value) = params.get(*key).filter(|value| !value.is_null()) {
            out.insert((*key).to_owned(), value.clone());
        }
    }
    serde_json::Value::Object(out)
}

/// The `patch` tool's paint kind. Every option has a default, so any payload
/// is a patch: a tool that cannot be read must not fall to the ladder.
pub(super) fn patch_kind(params: &serde_json::Value, shape: Option<PaintedShape>) -> PaintKind {
    let mode = match params.get("mode").and_then(|v| v.as_str()) {
        Some("normal") => PatchMode::Normal,
        Some("contentAware") => PatchMode::ContentAware,
        Some("smooth") => PatchMode::Smooth,
        _ => PatchMode::Auto,
    };
    let level = |key: &str, fallback: f64, lo: f64, hi: f64| number(params, key).unwrap_or(fallback).clamp(lo, hi).round() as u8;
    let offset = params.get("offset").map_or((0.0, 0.0), |o| (number(o, "x").unwrap_or(0.0), number(o, "y").unwrap_or(0.0)));
    let options = PatchOptions {
        mode,
        destination: params.get("direction").and_then(|v| v.as_str()) == Some("destination"),
        diffusion: level("diffusion", 5.0, 1.0, 7.0),
        structure: level("structure", 4.0, 1.0, 7.0),
        color: level("color", 0.0, 0.0, 10.0),
        offset: (offset.0.clamp(-200.0, 200.0), offset.1.clamp(-200.0, 200.0)),
    };
    let mut record = recorded(params, &["painted", "stroke"]);
    record["patch"] = serde_json::json!({
        "mode": options.mode.key(),
        "direction": if options.destination { "destination" } else { "source" },
        "diffusion": options.diffusion,
        "structure": options.structure,
        "color": options.color,
        "offset": { "x": options.offset.0, "y": options.offset.1 },
    });
    PaintKind::Patch { shape: shape.map(|mut shape| { shape.feather = 0.0; shape }), options, recorded: record }
}

/// A gradient off the seam, with its stops held to their ranges.
fn sanitized(mut g: Gradient) -> Option<Gradient> {
    let finite = |v: f64| v.is_finite();
    if !g.start.iter().chain(&g.end).copied().all(finite) || g.colors.is_empty() {
        return None;
    }
    for stop in &mut g.colors {
        stop.at = if finite(stop.at) { stop.at.clamp(0.0, 1.0) } else { 0.0 };
        stop.color = stop.color.map(|c| if finite(c) { c.clamp(0.0, 255.0) } else { 0.0 });
    }
    for stop in &mut g.opacities {
        stop.at = if finite(stop.at) { stop.at.clamp(0.0, 1.0) } else { 0.0 };
        stop.opacity = if finite(stop.opacity) { stop.opacity.clamp(0.0, 1.0) } else { 1.0 };
    }
    Some(g)
}

/// The `gradient` tool's paint kind. `params.gradient` is
/// [`Gradient`]'s own serde shape with `start` and `end` in page percent;
/// `params.kind` of `"auto"` is the Auto type, which reads none of it.
pub(super) fn gradient_kind(params: &serde_json::Value, shape: Option<PaintedShape>) -> PaintKind {
    let auto = params.get("kind").and_then(|v| v.as_str()) == Some("auto");
    let gradient = params
        .get("gradient")
        .filter(|_| !auto)
        .and_then(|value| serde_json::from_value::<Gradient>(value.clone()).ok())
        .and_then(sanitized);
    let options = GradientOptions { gradient, as_screentone: !auto && flag(params, "asScreentone", false), auto };
    let mut record = recorded(params, &["painted", "stroke"]);
    record["gradient"] = serde_json::to_value(&options.gradient).unwrap_or(serde_json::Value::Null);
    record["asScreentone"] = serde_json::json!(options.as_screentone);
    if auto {
        record["auto"] = serde_json::json!(true);
    }
    PaintKind::Gradient { shape, options, recorded: record }
}

/* ------------------------------------------------------------------ */
/* Shared                                                              */
/* ------------------------------------------------------------------ */

/// What [`super::edit`] commits: the patch, its dabs (none), the window it
/// read, and anything the snapshot should say that only the render knew.
pub(super) type Rendered = (run::Made, usize, Rect, serde_json::Value);

/// 8 and 16 bit gray and RGB, with or without alpha.
fn supports(page: &Raster) -> bool {
    matches!(page.mode, ColorMode::Gray | ColorMode::GrayAlpha | ColorMode::Rgb | ColorMode::Rgba)
        && matches!(page.depth, BitDepth::Eight | BitDepth::Sixteen)
}

fn top(page: &Raster) -> f64 {
    ((1u32 << page.depth.bits().min(16)) - 1) as f64
}

fn colour_channels(page: &Raster) -> usize {
    if matches!(page.mode, ColorMode::Rgb | ColorMode::Rgba) { 3 } else { 1 }
}

/// The selection over the page: the drawn shape, or the brush's swept discs.
fn selection(shape: Option<&PaintedShape>, paint: &PaintPlan, width: u32, height: u32) -> Option<Mask> {
    match shape {
        Some(shape) => shape_coverage(shape, 0.0, width, height).map(|(mask, _)| mask),
        None => stroke_mask(
            &PaintedStroke {
                points: paint.points.iter().map(|p| StrokePoint { x: p.x, y: p.y }).collect(),
                radius: paint.spec.size / 2.0,
            },
            width,
            height,
        ),
    }
    .filter(|mask| !mask.is_empty())
}

/// The selection from a seam payload, for the two read-only commands.
fn selection_of(painted: Option<&serde_json::Value>, stroke: Option<&serde_json::Value>, width: u32, height: u32) -> Option<Mask> {
    if let Some(mut shape) = painted.and_then(|value| serde_json::from_value::<PaintedShape>(value.clone()).ok()) {
        shape.feather = 0.0;
        return shape_coverage(&shape, 0.0, width, height).map(|(mask, _)| mask).filter(|mask| !mask.is_empty());
    }
    let stroke = stroke.and_then(|value| serde_json::from_value::<PaintedStroke>(value.clone()).ok())?;
    stroke_mask(&stroke, width, height)
}

/// `mask` moved by `by` and cut to the page.
fn shifted(mask: &Mask, by: (i64, i64), width: u32, height: u32) -> Option<Mask> {
    let moved = Rect::new(mask.bounds.x + by.0, mask.bounds.y + by.1, mask.bounds.w, mask.bounds.h);
    let bounds = moved.intersection(Rect::new(0, 0, width, height));
    if bounds.w == 0 || bounds.h == 0 {
        return None;
    }
    let mut out = Mask::empty(bounds);
    for y in bounds.y..bounds.bottom() {
        for x in bounds.x..bounds.right() {
            out.set_coverage(x, y, mask.coverage(x - by.0, y - by.1));
        }
    }
    (!out.is_empty()).then_some(out)
}

fn union(a: Rect, b: Rect) -> Rect {
    let (x, y) = (a.x.min(b.x), a.y.min(b.y));
    Rect::new(x, y, (a.right().max(b.right()) - x) as u32, (a.bottom().max(b.bottom()) - y) as u32)
}

/// The page under `order` over `window`, clamped to the page.
fn surface(
    job: &Job,
    source_idx: usize,
    page: &Raster,
    order: u32,
    window: Rect,
    strip_context: Option<StripPaintContext<'_>>,
) -> Result<(Raster, Rect), &'static str> {
    let window = window.intersection(Rect::new(0, 0, page.width, page.height));
    let under = patches_over(job, source_idx, order, window, strip_context).map_err(|_| "decline.reason.rungUnavailable")?;
    let pixels = cleaner_core::composite::composite_region(page, &under, window, Some(order))
        .map_err(|_| "decline.reason.rungUnavailable")?;
    if pixels.width != window.w || pixels.height != window.h {
        return Err("decline.reason.rungUnavailable");
    }
    Ok((pixels, window))
}

/// The colour channels of `r`, each scaled to 0 to 255.
fn channels(r: &Raster) -> Vec<Grid> {
    let max = top(r);
    (0..colour_channels(r))
        .map(|k| Grid {
            w: r.width as usize,
            h: r.height as usize,
            data: (0..r.width as usize * r.height as usize)
                .map(|i| r.sample((i % r.width as usize) as u32, (i / r.width as usize) as u32, k) as f64 / max * 255.0)
                .collect(),
        })
        .collect()
}

fn luma(chs: &[Grid]) -> Grid {
    if chs.len() == 1 {
        return chs[0].clone();
    }
    Grid {
        w: chs[0].w,
        h: chs[0].h,
        data: (0..chs[0].data.len()).map(|i| 0.299 * chs[0].data[i] + 0.587 * chs[1].data[i] + 0.114 * chs[2].data[i]).collect(),
    }
}

/// `mask`'s pixels as a flag grid over `window`.
fn hole_in(mask: &Mask, window: Rect) -> GridMask {
    let (w, h) = (window.w as usize, window.h as usize);
    GridMask { w, h, data: (0..w * h).map(|i| mask.coverage(window.x + (i % w) as i64, window.y + (i / w) as i64) > 0).collect() }
}

fn interleave(chs: &[Grid]) -> Image {
    let (w, h, c) = (chs[0].w, chs[0].h, chs.len());
    let mut data = vec![0.0; w * h * c];
    for (k, g) in chs.iter().enumerate() {
        for i in 0..w * h {
            data[i * c + k] = g.data[i];
        }
    }
    Image { w, h, c, data }
}

fn made(engine: Engine, mask: Mask, pixels: Raster) -> run::Made {
    run::Made {
        engine,
        ink: mask.clone(),
        mask,
        pixels,
        provider: None,
        model_sha256: None,
        pad: cleaner_core::strip::EdgePad::None,
        tiles: None,
        matched: None,
    }
}

/* ------------------------------------------------------------------ */
/* Patch                                                               */
/* ------------------------------------------------------------------ */

/// The Patch tool's patch: the selection, filled as its mode says.
pub(super) fn patch(
    job: &Job,
    source_idx: usize,
    page: &Raster,
    paint: &PaintPlan,
    order: u32,
    strip_context: Option<StripPaintContext<'_>>,
) -> Result<Rendered, &'static str> {
    let PaintKind::Patch { shape, options, .. } = &paint.kind else {
        return Err("decline.reason.rungUnavailable");
    };
    if !supports(page) {
        return Err("decline.reason.paintUnsupportedMode");
    }
    let (pw, ph) = (page.width, page.height);
    let selected = selection(shape.as_ref(), paint, pw, ph).ok_or("decline.reason.rungUnavailable")?;
    let offset = ((options.offset.0 / 100.0 * pw as f64).round() as i64, (options.offset.1 / 100.0 * ph as f64).round() as i64);
    // Where the patch goes, and how far its source is from each of its pixels.
    let normal = options.mode == PatchMode::Normal;
    let (dest, shift) = match (normal, options.destination) {
        (true, true) => (shifted(&selected, offset, pw, ph).ok_or("decline.reason.cloneSourceOutside")?, (-offset.0, -offset.1)),
        (true, false) => (selected, offset),
        (false, _) => (selected, (0, 0)),
    };
    if normal && shift == (0, 0) {
        // A source on top of the destination copies the page onto itself.
        return Err("decline.reason.cloneSourceOutside");
    }
    let bounds = dest.bounds;
    let source_box = Rect::new(bounds.x + shift.0, bounds.y + shift.1, bounds.w, bounds.h);
    if normal && (source_box.x < 0 || source_box.y < 0 || source_box.right() > pw as i64 || source_box.bottom() > ph as i64) {
        return Err("decline.reason.cloneSourceOutside");
    }
    let context = match options.mode {
        PatchMode::Normal => 1,
        PatchMode::Smooth => 2,
        PatchMode::ContentAware => bounds.w.max(bounds.h).clamp(32, 192),
        PatchMode::Auto => screentone::MARGIN as u32,
    };
    let reach = if normal { union(bounds, source_box) } else { bounds };
    let (pixels, window) = surface(job, source_idx, page, order, reach.grown(context, pw, ph), strip_context)?;
    let (w, h) = (window.w as usize, window.h as usize);
    let chs = channels(&pixels);
    let hole = hole_in(&dest, window);
    let mut used = hole.clone();
    let mut extra = serde_json::json!({});

    let content_aware = |chs: &[Grid], hole: &GridMask, structure: u8| -> Result<Vec<Grid>, &'static str> {
        let opt = patchmatch::Options { patch: 2 * structure as usize + 3, ..Default::default() };
        let filled = patchmatch::complete(&interleave(chs), hole, None, opt).ok_or("decline.reason.rungUnavailable")?;
        Ok((0..chs.len()).map(|k| filled.channel(k)).collect())
    };
    let out: Vec<Grid> = match options.mode {
        PatchMode::Smooth => {
            let known = hole.not();
            chs.iter().map(|g| poisson::membrane(g, &known)).collect()
        }
        PatchMode::Normal => {
            let at = |x: i64, y: i64| (y.clamp(0, h as i64 - 1) as usize) * w + x.clamp(0, w as i64 - 1) as usize;
            let reach = poisson::diffusion_reach(options.diffusion);
            chs.iter()
                .map(|g| {
                    let source = Grid { w, h, data: (0..w * h).map(|i| g.data[at((i % w) as i64 + shift.0, (i / w) as i64 + shift.1)]).collect() };
                    poisson::poisson(g, &source, &hole, reach)
                })
                .collect()
        }
        PatchMode::ContentAware => content_aware(&chs, &hole, options.structure)?,
        PatchMode::Auto => {
            let gray = luma(&chs);
            let filled = screentone::fill(&gray, &hole, &GridMask::new(w, h, false));
            extra["auto"] = serde_json::json!(if filled.screen { "screentone" } else { "contentAware" });
            if filled.screen {
                used = filled.used.clone();
                if chs.len() == 1 {
                    vec![filled.gray]
                } else {
                    // Each copied pixel takes the colour of the page pixel it
                    // was copied from, with the tone correction; PatchMatch
                    // in colour where the copy did not reach, as the
                    // screentone rung does it.
                    let rest = (0..w * h).any(|i| used.data[i] && filled.from[i].is_none());
                    let pm = if rest { Some(content_aware(&chs, &used, 4)?) } else { None };
                    (0..chs.len())
                        .map(|k| Grid {
                            w,
                            h,
                            data: (0..w * h)
                                .map(|i| match (filled.from[i], &pm) {
                                    _ if !used.data[i] => chs[k].data[i],
                                    (Some(s), _) => chs[k].data[s] + filled.correction.data[i],
                                    (None, Some(pm)) => pm[k].data[i],
                                    (None, None) => filled.gray.data[i],
                                })
                                .collect(),
                        })
                        .collect()
                }
            } else if chs.len() == 1 {
                vec![filled.gray]
            } else {
                content_aware(&chs, &hole, 4)?
            }
        }
    };

    // The layer: the destination with its own soft rim, and the pixels the
    // screentone guard grew it over at full coverage.
    let (mask, patch) = filled_layer(&pixels, window, &used, Some((&hole, &dest)), &out, top(page))?;
    Ok((made(Engine::Clone, mask, patch), 0, window, extra))
}

/// The layer for a fill over `window`: the `used` pixels, at the selection's
/// own coverage inside it (`edge`: the selection over the window, and the
/// selection itself) and at full coverage past it, or at full coverage
/// throughout without `edge`, with `out`'s values (0 to 255) written at the
/// page's depth.
fn filled_layer(
    pixels: &Raster,
    window: Rect,
    used: &GridMask,
    edge: Option<(&GridMask, &Mask)>,
    out: &[Grid],
    max: f64,
) -> Result<(Mask, Raster), &'static str> {
    let w = window.w as usize;
    let local: Vec<usize> = (0..used.data.len()).filter(|&i| used.data[i]).collect();
    let (x0, x1) = (local.iter().map(|i| i % w).min().unwrap_or(0), local.iter().map(|i| i % w).max().unwrap_or(0));
    let (y0, y1) = (local.iter().map(|i| i / w).min().unwrap_or(0), local.iter().map(|i| i / w).max().unwrap_or(0));
    let layer = Rect::new(window.x + x0 as i64, window.y + y0 as i64, (x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32);
    let mut mask = Mask::empty(layer);
    let mut patch = cleaner_core::engines::model::page_crop(pixels, Rect::new(layer.x - window.x, layer.y - window.y, layer.w, layer.h));
    for &i in &local {
        let (px, py) = (window.x + (i % w) as i64, window.y + (i / w) as i64);
        let coverage = match edge {
            Some((hole, dest)) if hole.data[i] => dest.coverage(px, py),
            _ => 255,
        };
        mask.set_coverage(px, py, coverage.max(1));
        let (cx, cy) = ((px - layer.x) as u32, (py - layer.y) as u32);
        for (k, g) in out.iter().enumerate() {
            patch.set_sample(cx, cy, k, (g.data[i].clamp(0.0, 255.0) / 255.0 * max).round() as u16);
        }
    }
    if mask.is_empty() {
        return Err("decline.reason.rungUnavailable");
    }
    Ok((mask, patch))
}

/* ------------------------------------------------------------------ */
/* Gradient                                                            */
/* ------------------------------------------------------------------ */

/// The gradient in page pixels, from the seam's page percent.
fn in_pixels(g: &Gradient, width: u32, height: u32) -> Gradient {
    let px = |p: [f64; 2]| [p[0] / 100.0 * width as f64, p[1] / 100.0 * height as f64];
    Gradient { start: px(g.start), end: px(g.end), ..g.clone() }
}

/// The Gradient tool's patch: the gradient over the page inside the selection.
pub(super) fn gradient(
    job: &Job,
    source_idx: usize,
    page: &Raster,
    paint: &PaintPlan,
    order: u32,
    strip_context: Option<StripPaintContext<'_>>,
) -> Result<Rendered, &'static str> {
    let PaintKind::Gradient { shape, options, .. } = &paint.kind else {
        return Err("decline.reason.rungUnavailable");
    };
    if !supports(page) {
        return Err("decline.reason.paintUnsupportedMode");
    }
    if options.auto {
        return auto_gradient(job, source_idx, page, shape.as_ref(), paint, order, strip_context);
    }
    let g = options.gradient.as_ref().ok_or("decline.reason.rungUnavailable")?;
    let (pw, ph) = (page.width, page.height);
    let g = in_pixels(g, pw, ph);
    let mask = selection(shape.as_ref(), paint, pw, ph).ok_or("decline.reason.rungUnavailable")?;
    let bounds = mask.bounds;
    let gray_page = colour_channels(page) == 1;
    let dots = options.as_screentone && gray_page;
    let reach = if dots { bounds.grown(screentone::MARGIN as u32, pw, ph) } else { bounds };
    let (under, window) = surface(job, source_idx, page, order, reach, strip_context)?;
    let managed = ManagedColor::for_raster(page).map_err(|_| "decline.reason.paintUnsupportedColor")?;
    let max = top(page);
    let channels_n = colour_channels(page);
    let alpha = page.mode.alpha_channel();
    let mut patch = cleaner_core::engines::model::page_crop(&under, Rect::new(bounds.x - window.x, bounds.y - window.y, bounds.w, bounds.h));
    // The plain gradient, blended by its opacity stops over the page.
    let mut tone: Option<Grid> = dots.then(|| luma(&channels(&under)));
    for y in 0..bounds.h {
        for x in 0..bounds.w {
            let (px, py) = (bounds.x + x as i64, bounds.y + y as i64);
            if mask.coverage(px, py) == 0 {
                continue;
            }
            let (c, a) = gradient::shade(&g, px, py);
            let native = managed
                .from_srgb(c.map(|v| (v / 255.0) as f32), page.mode)
                .map_err(|_| "decline.reason.paintUnsupportedColor")?;
            for (k, value) in native.iter().take(channels_n).enumerate() {
                let below = patch.sample(x, y, k) as f64;
                let level = below + (f64::from(*value).clamp(0.0, 1.0) * max - below) * a;
                patch.set_sample(x, y, k, level.round().clamp(0.0, max) as u16);
                if let Some(tone) = tone.as_mut() {
                    let i = (py - window.y) as usize * window.w as usize + (px - window.x) as usize;
                    tone.data[i] = level / max * 255.0;
                }
            }
            if let Some(channel) = alpha {
                patch.set_sample(x, y, channel, max as u16);
            }
        }
    }
    let mut extra = serde_json::json!({});
    if options.as_screentone {
        let mut screen = false;
        if let Some(tone) = tone {
            let gray = luma(&channels(&under));
            let hole = hole_in(&mask, window);
            let filled = screentone::fill_with(&gray, &hole, &GridMask::new(window.w as usize, window.h as usize, false), Some(&tone));
            if filled.screen {
                screen = true;
                for (i, _) in hole.data.iter().enumerate().filter(|(_, &v)| v) {
                    let (px, py) = (window.x + (i % window.w as usize) as i64, window.y + (i / window.w as usize) as i64);
                    let value = (filled.gray.data[i].clamp(0.0, 255.0) / 255.0 * max).round() as u16;
                    patch.set_sample((px - bounds.x) as u32, (py - bounds.y) as u32, 0, value);
                }
            }
        }
        if !screen {
            crate::events::notice("retouch.notice.noScreen", serde_json::json!({}), "info");
        }
        extra["screen"] = serde_json::json!(screen);
    }
    Ok((made(Engine::Paint, mask, patch), 0, window, extra))
}

/// The Gradient tool's Auto type: the selection, and the lettering's glow
/// past it, filled with the surface all round it. Refused where the ring is
/// no one smooth surface (a screen, a drawing).
fn auto_gradient(
    job: &Job,
    source_idx: usize,
    page: &Raster,
    shape: Option<&PaintedShape>,
    paint: &PaintPlan,
    order: u32,
    strip_context: Option<StripPaintContext<'_>>,
) -> Result<Rendered, &'static str> {
    let (pw, ph) = (page.width, page.height);
    let dest = selection(shape, paint, pw, ph).ok_or("decline.reason.rungUnavailable")?;
    let (pixels, window) = surface(job, source_idx, page, order, dest.bounds.grown(autogradient::MARGIN, pw, ph), strip_context)?;
    let hole = hole_in(&dest, window);
    let filled = autogradient::fill(&channels(&pixels), &hole).ok_or("decline.reason.noSmoothSurround")?;
    // Full coverage: the fill meets the page by itself, and a selection's
    // soft rim across a letter it cut would let the letter show through.
    let (mask, patch) = filled_layer(&pixels, window, &filled.paint, None, &filled.channels, top(page))?;
    Ok((made(Engine::Paint, mask, patch), 0, window, serde_json::json!({})))
}

/* ------------------------------------------------------------------ */
/* Read-only commands: the proposed source, Match surroundings          */
/* ------------------------------------------------------------------ */

/// A point in page percent.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Percent {
    pub x: f64,
    pub y: f64,
}

/// `proposePatchSource`'s answer.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchSource {
    /// From the selection to its proposed source, in page percent.
    pub offset: Percent,
}

/// The page as it looks now (every visible layer), its source slot, and the
/// job, or `None` when the page or the expected source is not there.
fn current_page(located: &Located, expected: Option<&(usize, String)>) -> Result<Option<(Job, usize, Raster)>, String> {
    let _lock = run::lock_job(&located.job_path)?;
    let job = Job::open(&located.job_path).map_err(|e| e.to_string())?;
    let Some(source_idx) = Library::resolve_page(&job.project, located.page_index) else {
        return Ok(None);
    };
    let Some(path) = job.source_path(source_idx) else {
        return Ok(None);
    };
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    if expected.is_some_and(|(index, sha)| *index != source_idx || *sha != sha256_hex(&bytes)) {
        return Ok(None);
    }
    let page = job.display_page(source_idx, &bytes).map_err(|e| e.to_string())?;
    Ok(Some((job, source_idx, page)))
}

/// The page under every visible layer over `window`.
fn looks(job: &Job, source_idx: usize, page: &Raster, window: Rect) -> Option<(Raster, Rect)> {
    let window = window.intersection(Rect::new(0, 0, page.width, page.height));
    let order = next_order(job, source_idx);
    let under = patches_over(job, source_idx, order, window, None).ok()?;
    let pixels = cleaner_core::composite::composite_region(page, &under, window, None).ok()?;
    Some((pixels, window))
}

/// The proposed Patch source for a selection: the offset whose band matches
/// the selection's band ([`patchsource::propose`]), on the page as it looks
/// now. `None` for a page that is not there, an unsupported page, an empty
/// selection, or one with no room for a source.
pub(super) fn propose_at(
    located: &Located,
    expected: Option<&(usize, String)>,
    painted: Option<&serde_json::Value>,
    stroke: Option<&serde_json::Value>,
) -> Result<Option<PatchSource>, String> {
    let Some((job, source_idx, page)) = current_page(located, expected)? else {
        return Ok(None);
    };
    if !supports(&page) {
        return Ok(None);
    }
    let Some(selected) = selection_of(painted, stroke, page.width, page.height) else {
        return Ok(None);
    };
    let size = selected.bounds.w.max(selected.bounds.h) as f64;
    let reach = (patchsource::REACH * size).ceil() as u32 + 16;
    let Some((pixels, window)) = looks(&job, source_idx, &page, selected.bounds.grown(reach, page.width, page.height)) else {
        return Ok(None);
    };
    let gray = luma(&channels(&pixels));
    let Some((dx, dy)) = patchsource::propose(&gray, &hole_in(&selected, window)) else {
        return Ok(None);
    };
    Ok(Some(PatchSource {
        offset: Percent { x: dx as f64 / page.width as f64 * 100.0, y: dy as f64 / page.height as f64 * 100.0 },
    }))
}

/// **Match surroundings**: the gradient [`gradient::fit`] reads off the ring
/// around a selection (2 to 12 px out), on the page as it looks now, in the
/// seam's shape with `start` and `end` in page percent.
pub(super) fn fit_at(
    located: &Located,
    expected: Option<&(usize, String)>,
    painted: Option<&serde_json::Value>,
    stroke: Option<&serde_json::Value>,
) -> Result<Option<serde_json::Value>, String> {
    let Some((job, source_idx, page)) = current_page(located, expected)? else {
        return Ok(None);
    };
    if !supports(&page) {
        return Ok(None);
    }
    let Some(selected) = selection_of(painted, stroke, page.width, page.height) else {
        return Ok(None);
    };
    let Some((pixels, window)) = looks(&job, source_idx, &page, selected.bounds.grown(14, page.width, page.height)) else {
        return Ok(None);
    };
    let Ok(managed) = ManagedColor::for_raster(&pixels) else {
        return Ok(None);
    };
    let hole = hole_in(&selected, window);
    let ring = hole.dilate(12).and_not(&hole.dilate(2));
    let w = window.w as usize;
    let picked: Vec<usize> = (0..ring.data.len()).filter(|&i| ring.data[i]).collect();
    let stride = picked.len().div_ceil(20_000).max(1);
    let samples: Vec<(f64, f64, [f64; 3])> = picked
        .iter()
        .step_by(stride)
        .map(|&i| {
            let (x, y) = ((i % w) as u32, (i / w) as u32);
            let c = managed.pixel(&pixels, x, y).map(|v| f64::from(v.clamp(0.0, 1.0)) * 255.0);
            (window.x as f64 + x as f64 + 0.5, window.y as f64 + y as f64 + 0.5, c)
        })
        .collect();
    let b = selected.bounds;
    let Some(mut fitted) = gradient::fit(&samples, (b.x as f64, b.y as f64, b.right() as f64, b.bottom() as f64)) else {
        return Ok(None);
    };
    let pct = |p: [f64; 2]| [p[0] / page.width as f64 * 100.0, p[1] / page.height as f64 * 100.0];
    fitted.start = pct(fitted.start);
    fitted.end = pct(fitted.end);
    Ok(serde_json::to_value(&fitted).ok())
}

/// The source binding both read-only commands share with `createRegion`.
fn expected_of(index: Option<usize>, sha: Option<String>) -> Option<Option<(usize, String)>> {
    match (index, sha) {
        (Some(index), Some(sha)) => Some(Some((index, sha))),
        (None, None) => Some(None),
        _ => None,
    }
}

/// `proposePatchSource`: where the Patch tool's ghost starts.
#[tauri::command]
pub async fn propose_patch_source(
    app: tauri::AppHandle,
    chapter_id: String,
    page_index: u32,
    expected_source_idx: Option<usize>,
    expected_source_sha: Option<String>,
    painted: Option<serde_json::Value>,
    stroke: Option<serde_json::Value>,
) -> Result<Option<PatchSource>, String> {
    crate::library::blocking(move || {
        let Some(expected) = expected_of(expected_source_idx, expected_source_sha) else {
            return Ok(None);
        };
        let library = Library::for_app(&app)?;
        let located = match locate_page(&library, &chapter_id, page_index as usize) {
            Ok(Some(located)) => located,
            Ok(None) | Err(LibraryError::Unknown { .. } | LibraryError::NoJob { .. }) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        propose_at(&located, expected.as_ref(), painted.as_ref(), stroke.as_ref())
    })
    .await
}

/// `fitGradient`: the Gradient tool's Match surroundings.
#[tauri::command]
pub async fn fit_gradient(
    app: tauri::AppHandle,
    chapter_id: String,
    page_index: u32,
    expected_source_idx: Option<usize>,
    expected_source_sha: Option<String>,
    painted: Option<serde_json::Value>,
    stroke: Option<serde_json::Value>,
) -> Result<Option<serde_json::Value>, String> {
    crate::library::blocking(move || {
        let Some(expected) = expected_of(expected_source_idx, expected_source_sha) else {
            return Ok(None);
        };
        let library = Library::for_app(&app)?;
        let located = match locate_page(&library, &chapter_id, page_index as usize) {
            Ok(Some(located)) => located,
            Ok(None) | Err(LibraryError::Unknown { .. } | LibraryError::NoJob { .. }) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        fit_at(&located, expected.as_ref(), painted.as_ref(), stroke.as_ref())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::super::tests::{edit_fixture, fixture_plan, test_bench};
    use super::*;

    fn raster(name: &str, w: u32, h: u32, pixel: impl Fn(u32, u32) -> Vec<u8>) -> Raster {
        let mut r = cleaner_core::image::fixtures::by_name(name).raster;
        r.width = w;
        r.height = h;
        let pixel = &pixel;
        r.data = (0..h).flat_map(|y| (0..w).flat_map(move |x| pixel(x, y))).collect();
        r
    }

    fn rect_percent(x0: f64, y0: f64, x1: f64, y1: f64) -> serde_json::Value {
        serde_json::json!({"kind": "rect", "feather": 0,
            "points": [{"x": x0, "y": y0}, {"x": x1, "y": y0}, {"x": x1, "y": y1}, {"x": x0, "y": y1}]})
    }

    /// Run one selection tool on a fresh job over `raw`, and answer with the
    /// outcome, the page as composited after it, and the layer's snapshot.
    fn run_tool(name: &str, raw: &Raster, tool: &str, params: serde_json::Value) -> (Outcome, Option<(Raster, Patch)>) {
        let (root, located) = edit_fixture(name, raw);
        let bounds = Rect::new(0, 0, raw.width, raw.height);
        let mut plan = fixture_plan("layer", Geometry::Given(bounds), Engine::Paint, bounds);
        plan.tool = Some(tool.into());
        plan.paint = Some(paint_plan(tool, &params).expect("a selection tool always plans"));
        let outcome = edit_with_bench(&mut test_bench(&root), Engine::Fill, &located, plan).unwrap();
        let job = Job::open(&located.job_path).unwrap();
        let made = job.project.patches.iter().find(|r| r.id == "layer").map(|record| {
            let patch = job.load_patch(record).unwrap();
            (cleaner_core::composite::composite(raw, std::slice::from_ref(&patch)).unwrap(), patch)
        });
        let _ = std::fs::remove_dir_all(root);
        (outcome, made)
    }

    /// A vertical ramp with a black block of "lettering" at 44..68.
    fn ramp_with_lettering() -> Raster {
        raster("l8", 128, 128, |x, y| vec![if (44..68).contains(&x) && (44..68).contains(&y) { 0 } else { 60 + y as u8 }])
    }

    fn worst_in(page: &Raster, x0: u32, y0: u32, x1: u32, y1: u32, truth: impl Fn(u32, u32) -> f64) -> f64 {
        let mut worst: f64 = 0.0;
        for y in y0..y1 {
            for x in x0..x1 {
                worst = worst.max((page.sample(x, y, 0) as f64 - truth(x, y)).abs());
            }
        }
        worst
    }

    #[test]
    fn retouch_patch_normal_takes_the_source_detail_with_the_destination_edge() {
        let raw = ramp_with_lettering();
        // the selection 40..72 square, its source 40 px to the right
        let params = serde_json::json!({"mode": "normal", "direction": "source", "diffusion": 7,
            "offset": {"x": 31.25, "y": 0}, "painted": rect_percent(31.25, 31.25, 56.25, 56.25)});
        let (outcome, made) = run_tool("patch-normal", &raw, "patch", params);
        assert!(matches!(outcome, Outcome::Cleaned(_)));
        let (page, patch) = made.unwrap();
        let worst = worst_in(&page, 40, 40, 72, 72, |_, y| 60.0 + y as f64);
        assert!(worst <= 3.0, "the ramp is back to within {worst}");
        let snapshot = &patch.provenance.params_snapshot;
        assert_eq!(patch.provenance.engine, Engine::Clone);
        assert_eq!(snapshot["mode"], "patch");
        assert_eq!(snapshot["rung"], "ladder.rung.clone");
        assert_eq!(snapshot["tool"], "patch");
        assert_eq!(snapshot["patch"], serde_json::json!({"mode": "normal", "direction": "source",
            "diffusion": 7, "structure": 4, "color": 0, "offset": {"x": 31.25, "y": 0.0}}));
        assert_eq!(snapshot["painted"], rect_percent(31.25, 31.25, 56.25, 56.25));
    }

    #[test]
    fn retouch_patch_destination_copies_the_selection_to_where_it_is_dragged() {
        let raw = ramp_with_lettering();
        // the clean square at 80..112, dragged 40 px left onto the lettering
        let params = serde_json::json!({"mode": "normal", "direction": "destination", "diffusion": 7,
            "offset": {"x": -31.25, "y": 0}, "painted": rect_percent(62.5, 31.25, 87.5, 56.25)});
        let (outcome, made) = run_tool("patch-destination", &raw, "patch", params);
        assert!(matches!(outcome, Outcome::Cleaned(_)));
        let (page, patch) = made.unwrap();
        assert_eq!(patch.mask.bounds, Rect::new(40, 40, 32, 32), "the layer sits where it was dragged");
        assert!(worst_in(&page, 40, 40, 72, 72, |_, y| 60.0 + y as f64) <= 3.0);
    }

    #[test]
    fn retouch_patch_refuses_a_source_off_the_page() {
        let raw = ramp_with_lettering();
        let params = serde_json::json!({"mode": "normal", "offset": {"x": 60, "y": 0},
            "painted": rect_percent(31.25, 31.25, 56.25, 56.25)});
        let (outcome, made) = run_tool("patch-outside", &raw, "patch", params);
        assert!(matches!(outcome, Outcome::Refused("decline.reason.cloneSourceOutside")));
        assert!(made.is_none());
    }

    #[test]
    fn retouch_patch_smooth_and_content_aware_need_no_source() {
        let paper = |x: u32, y: u32| vec![if (44..68).contains(&x) && (44..68).contains(&y) { 0 } else { 200 }];
        let raw = raster("l8", 128, 128, paper);
        for (mode, tolerance) in [("smooth", 1.0), ("contentAware", 2.0), ("auto", 2.0)] {
            // an offset is sent and ignored: these modes read no source
            let params = serde_json::json!({"mode": mode, "offset": {"x": 90, "y": 90},
                "painted": rect_percent(31.25, 31.25, 56.25, 56.25)});
            let (outcome, made) = run_tool(&format!("patch-{mode}"), &raw, "patch", params);
            assert!(matches!(outcome, Outcome::Cleaned(_)), "{mode}");
            let (page, patch) = made.unwrap();
            let worst = worst_in(&page, 40, 40, 72, 72, |_, _| 200.0);
            assert!(worst <= tolerance, "{mode}: paper back to within {worst}");
            assert_eq!(patch.provenance.params_snapshot["patch"]["mode"], mode);
            if mode == "auto" {
                assert_eq!(patch.provenance.params_snapshot["auto"], "contentAware", "flat paper is no screen");
            }
        }
    }

    /// A 45 degree dot screen, ramped, with two white letter strokes.
    fn screen_page(w: u32, h: u32, letters: bool) -> Raster {
        raster("l8", w, h, move |x, y| {
            let (fx, fy) = (x as f64, y as f64);
            let coverage = 0.8 - 0.5 * fy / h as f64;
            let (u, v) = ((fx + fy) / 6.0, (fx - fy) / 6.0);
            let cell = ((u * std::f64::consts::TAU).cos() + (v * std::f64::consts::TAU).cos()) / 2.0;
            let letter = letters && ((96..106).contains(&x) && (80..170).contains(&y));
            vec![if letter { 255 } else if cell > 1.0 - 2.0 * coverage { 20 } else { 235 }]
        })
    }

    #[test]
    fn retouch_patch_auto_takes_the_screentone_fill_on_a_screen() {
        let raw = screen_page(200, 240, true);
        let params = serde_json::json!({"mode": "auto",
            "painted": rect_percent(47.0, 32.5, 54.0, 71.5)});
        let (outcome, made) = run_tool("patch-auto-screen", &raw, "patch", params);
        assert!(matches!(outcome, Outcome::Cleaned(_)));
        let (page, patch) = made.unwrap();
        assert_eq!(patch.provenance.params_snapshot["auto"], "screentone");
        let white = (80..170).flat_map(|y| (96..106).map(move |x| (x, y))).filter(|&(x, y)| page.sample(x, y, 0) > 250).count();
        assert!(white < 900 / 3, "{white} of 900 letter pixels still white");
    }

    fn linear(start: [f64; 2], end: [f64; 2], colors: serde_json::Value, opacities: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"kind": "linear", "start": start, "end": end, "colors": colors,
            "opacities": opacities, "reverse": false, "dither": false})
    }

    #[test]
    fn retouch_gradient_fills_the_selection_and_records_the_gradient() {
        let raw = raster("rgb8", 128, 128, |_, _| vec![0, 0, 0]);
        let g = linear([25.0, 50.0], [75.0, 50.0],
            serde_json::json!([{"at": 0, "color": [255, 0, 0]}, {"at": 1, "color": [0, 0, 255]}]),
            serde_json::json!([{"at": 0, "opacity": 1}, {"at": 1, "opacity": 1}]));
        let params = serde_json::json!({"opacity": 70, "gradient": g, "painted": rect_percent(25.0, 25.0, 75.0, 75.0)});
        let (outcome, made) = run_tool("gradient-rgb", &raw, "gradient", params);
        assert!(matches!(outcome, Outcome::Cleaned(_)));
        let (_, patch) = made.unwrap();
        assert_eq!(patch.provenance.engine, Engine::Paint);
        // the stored pixels are the gradient; the layer's opacity is applied
        // by the compositor, as a solid shape's is
        let px = |x: u32, y: u32| (0..3).map(|k| patch.pixels.sample(x - 32, y - 32, k)).collect::<Vec<_>>();
        let start = px(33, 64);
        assert!(start[0] > 240 && start[2] < 15, "start {start:?}");
        let end = px(94, 64);
        assert!(end[2] > 240 && end[0] < 15, "end {end:?}");
        let middle = px(64, 64);
        assert!((120..136).contains(&middle[0]) && (120..136).contains(&middle[2]), "middle {middle:?}");
        let snapshot = &patch.provenance.params_snapshot;
        assert_eq!(snapshot["mode"], "gradient");
        assert_eq!(snapshot["rung"], "ladder.rung.paint");
        assert_eq!(snapshot["layer"]["opacity"], 70);
        let back: Gradient = serde_json::from_value(snapshot["gradient"].clone()).unwrap();
        assert_eq!(back, serde_json::from_value::<Gradient>(g).unwrap(), "the gradient reopens as it was drawn");
        assert_eq!(snapshot["asScreentone"], false);
    }

    #[test]
    fn retouch_gradient_blends_its_opacity_stops_over_a_gray_page() {
        let raw = raster("l8", 100, 100, |_, _| vec![255]);
        let g = linear([0.0, 50.0], [100.0, 50.0],
            serde_json::json!([{"at": 0, "color": [0, 0, 0]}]),
            serde_json::json!([{"at": 0, "opacity": 1}, {"at": 1, "opacity": 0}]));
        let params = serde_json::json!({"gradient": g, "painted": rect_percent(0.0, 0.0, 100.0, 100.0)});
        let (outcome, made) = run_tool("gradient-gray", &raw, "gradient", params);
        assert!(matches!(outcome, Outcome::Cleaned(_)));
        let (page, _) = made.unwrap();
        assert!(page.sample(0, 50, 0) < 5, "{}", page.sample(0, 50, 0));
        assert!((120..136).contains(&page.sample(50, 50, 0)), "{}", page.sample(50, 50, 0));
        assert!(page.sample(99, 50, 0) > 250, "{}", page.sample(99, 50, 0));
    }

    #[test]
    fn retouch_gradient_as_screentone_draws_the_pages_dots_or_says_there_are_none() {
        let g = linear([50.0, 30.0], [50.0, 70.0],
            serde_json::json!([{"at": 0, "color": [40, 40, 40]}, {"at": 1, "color": [200, 200, 200]}]),
            serde_json::json!([]));
        let painted = rect_percent(40.0, 30.0, 60.0, 70.0);
        let raw = screen_page(200, 240, false);
        let params = serde_json::json!({"gradient": g, "asScreentone": true, "painted": painted});
        let (outcome, made) = run_tool("gradient-dots", &raw, "gradient", params.clone());
        assert!(matches!(outcome, Outcome::Cleaned(_)));
        let (page, patch) = made.unwrap();
        assert_eq!(patch.provenance.params_snapshot["screen"], true);
        // dots, not a smooth ramp: almost every pixel is near ink or paper
        let (mut grey, mut all) = (0, 0);
        for y in 75..165 {
            for x in 82..118 {
                let v = page.sample(x, y, 0);
                all += 1;
                if (60..200).contains(&v) {
                    grey += 1;
                }
            }
        }
        assert!(grey * 5 < all, "{grey} of {all} pixels are mid grey");
        // flat paper is no screen: the plain gradient, and the snapshot says so
        let flat = raster("l8", 200, 240, |_, _| vec![255]);
        let (outcome, made) = run_tool("gradient-nodots", &flat, "gradient", params);
        assert!(matches!(outcome, Outcome::Cleaned(_)));
        let (page, patch) = made.unwrap();
        assert_eq!(patch.provenance.params_snapshot["screen"], false);
        assert!(page.sample(100, 74, 0) < page.sample(100, 166, 0), "the plain ramp runs dark to light");
    }

    #[test]
    fn retouch_selection_tools_refuse_indexed_pages() {
        let raw = cleaner_core::image::fixtures::by_name("indexed-p").raster;
        let painted = rect_percent(25.0, 25.0, 60.0, 60.0);
        let g = linear([25.0, 25.0], [60.0, 60.0], serde_json::json!([{"at": 0, "color": [0, 0, 0]}]), serde_json::json!([]));
        for (tool, params) in [
            ("gradient", serde_json::json!({"gradient": g, "painted": painted})),
            ("patch", serde_json::json!({"mode": "smooth", "painted": painted})),
        ] {
            let (outcome, made) = run_tool(&format!("{tool}-indexed"), &raw, tool, params);
            assert!(matches!(outcome, Outcome::Refused("decline.reason.paintUnsupportedMode")), "{tool}");
            assert!(made.is_none());
        }
    }

    #[test]
    fn retouch_auto_gradient_fills_from_all_round_and_refuses_noise() {
        // a bubble eased dark red to red down the page, white lettering with
        // a red glow in the middle, and the selection stopping inside the glow
        let bubble = |x: u32, y: u32| {
            let f = (y as f64 / 160.0).powf(1.4);
            let mut c = [10.0 + 100.0 * f, 2.0 + 16.0 * f, 1.0 + 19.0 * f];
            let d = (60i64 - x as i64).max(x as i64 - 100).max(70 - y as i64).max(y as i64 - 90) as f64;
            if d <= 0.0 {
                c = [250.0; 3];
            } else if d < 8.0 {
                c[0] += 120.0 * (1.0 - d / 8.0);
            }
            c.map(|v| v.clamp(0.0, 255.0) as u8).to_vec()
        };
        let raw = raster("rgb8", 160, 160, bubble);
        let painted = rect_percent(36.0, 42.0, 64.0, 58.0);
        let (outcome, made) = run_tool("gradient-auto", &raw, "gradient", serde_json::json!({"kind": "auto", "painted": painted}));
        assert!(matches!(outcome, Outcome::Cleaned(_)));
        let (page, patch) = made.unwrap();
        assert_eq!(patch.provenance.engine, Engine::Paint);
        let snapshot = &patch.provenance.params_snapshot;
        assert_eq!((snapshot["mode"].as_str(), snapshot["auto"].as_bool()), (Some("gradient"), Some(true)));
        assert!(snapshot["gradient"].is_null());
        for (x, y) in [(80, 80), (55, 80), (80, 95)] {
            let want = 10.0 + 100.0 * (y as f64 / 160.0).powf(1.4);
            let got = page.sample(x, y, 0) as f64;
            assert!((got - want).abs() < 8.0, "({x}, {y}): {got} for {want}, the lettering and the glow past the selection are gone");
        }
        let noise = raster("rgb8", 96, 96, |x, y| vec![((x * 37 + y * 91) % 251) as u8, ((x * 13 + y * 7) % 241) as u8, ((x * y) % 239) as u8]);
        let (outcome, _) = run_tool("gradient-auto-noise", &noise, "gradient", serde_json::json!({"kind": "auto", "painted": rect_percent(35.0, 35.0, 65.0, 65.0)}));
        assert!(matches!(outcome, Outcome::Refused("decline.reason.noSmoothSurround")));
    }

    #[test]
    fn retouch_payloads_always_plan_and_an_unreadable_gradient_refuses() {
        let plan = paint_plan("patch", &serde_json::json!({})).unwrap();
        let PaintKind::Patch { options, .. } = plan.kind else { panic!("not a patch") };
        assert_eq!(options, PatchOptions { mode: PatchMode::Auto, destination: false, diffusion: 5, structure: 4, color: 0, offset: (0.0, 0.0) });
        let params = serde_json::json!({"mode": "contentAware", "direction": "destination", "diffusion": 9, "structure": 0, "color": 12});
        let PaintKind::Patch { options, .. } = paint_plan("patch", &params).unwrap().kind else { panic!() };
        assert_eq!((options.mode, options.destination, options.diffusion, options.structure, options.color), (PatchMode::ContentAware, true, 7, 1, 10));
        let raw = ramp_with_lettering();
        let params = serde_json::json!({"gradient": {"kind": "spiral"}, "painted": rect_percent(25.0, 25.0, 60.0, 60.0)});
        let (outcome, _) = run_tool("gradient-unreadable", &raw, "gradient", params);
        assert!(matches!(outcome, Outcome::Refused("decline.reason.rungUnavailable")));
    }

    #[test]
    fn retouch_proposes_a_source_and_fits_the_surroundings() {
        let raw = ramp_with_lettering();
        let (root, located) = edit_fixture("retouch-read", &raw);
        let painted = rect_percent(31.25, 31.25, 56.25, 56.25);
        let source = propose_at(&located, None, Some(&painted), None).unwrap().expect("a source");
        let (dx, dy) = (source.offset.x / 100.0 * 128.0, source.offset.y / 100.0 * 128.0);
        assert!(dx.abs() >= 32.0 || dy.abs() >= 32.0, "({dx}, {dy}) overlaps the selection");
        assert!(dy.abs() <= 4.0, "a vertical ramp keeps the source on the same rows: dy {dy}");
        // a source slot that is not the page's is no answer
        assert!(propose_at(&located, Some(&(0, "not-the-page".into())), Some(&painted), None).unwrap().is_none());
        let fitted: Gradient = serde_json::from_value(fit_at(&located, None, Some(&painted), None).unwrap().expect("a fit")).unwrap();
        assert_eq!(fitted.kind, gradient::Kind::Linear);
        assert!((fitted.end[1] - fitted.start[1]).abs() > 10.0 * (fitted.end[0] - fitted.start[0]).abs(), "vertical: {fitted:?}");
        let _ = std::fs::remove_dir_all(root);
    }
}
