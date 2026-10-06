//! `raw + ordered visible patches`, and nothing else.
//!
//! This function is where the fidelity contract stops being a promise and
//! becomes a property: it writes a sample only where a patch's applied mask
//! says so, so the set of changed pixels is a subset of `union(masks)` **by
//! construction**, whatever an engine returned.
//!
//! This is also the limit of what the fidelity test can prove - the subset
//! property holds no matter how bad the model output was, and interior
//! quality belongs to the drift gate and the reviewer.

use crate::image::{ColorMode, Raster};
use crate::mask::{Mask, Rect};
use crate::patch::Patch;

#[derive(Debug, thiserror::Error)]
pub enum CompositeError {
    #[error("patch {id} is {pw}×{ph} but its mask bounds are {mw}×{mh}")]
    Malformed {
        id: String,
        pw: u32,
        ph: u32,
        mw: u32,
        mh: u32,
    },
    #[error("patch {id} is {patch:?}/{patch_depth:?} but the page is {page:?}/{page_depth:?}")]
    ModeMismatch {
        id: String,
        patch: ColorMode,
        patch_depth: crate::image::BitDepth,
        page: ColorMode,
        page_depth: crate::image::BitDepth,
    },
}

/// The displayed and exported page.
///
/// Alpha is copied through unmodified - mask fitting and fills operate on
/// colour channels only, and §2's third clarification says alpha never enters a
/// ring statistic. So an `LA`/`RGBA` patch carries the source's alpha in its
/// buffer and this copies it like any other sample; nothing composites
/// *through* alpha, because a patch is a replacement, not a layer blend.
pub fn composite(page: &Raster, patches: &[Patch]) -> Result<Raster, CompositeError> {
    let mut out = page.clone();
    let whole = Rect::new(0, 0, page.width, page.height);
    for patch in ordered_visible(patches, None) {
        stamp(&mut out, page, patch, whole)?;
        out.color = out.color.after_edit();
    }
    Ok(out)
}

/// The same page, over one rectangle of it.
///
/// **A Phase 1 deliverable and not an optimisation.** A brush
/// stroke needs the surface *under* it to blend its soft edge against, and
/// [`composite`] clones and rebuilds the whole page: on a 4000×6000 scan that is
/// unusable once per stroke. `Raster::rows` is not an answer either - it crops
/// row ranges, because a row range is a contiguous slice at every depth, and a
/// stroke's box is not a row range.
///
/// `rect` is clamped to the page, and the answer is a raster of the clamped
/// rectangle in the page's own mode and depth - never a promotion, so the
/// result can be handed back to `set_sample` as-is.
///
/// `below` is an **exclusive** order ceiling: `Some(n)` composites the patches
/// whose `order < n`, which is what "the surface under this stroke" means for a
/// patch that is about to take order `n`. `None` composites all of them.
///
/// The ordering rule is [`composite`]'s, shared rather than restated, so the two
/// cannot drift apart into two different pages.
pub fn composite_region(
    page: &Raster,
    patches: &[Patch],
    rect: Rect,
    below: Option<u32>,
) -> Result<Raster, CompositeError> {
    let window = clamp_to(rect, page);
    let mut out = crop(page, window);
    for patch in ordered_visible(patches, below) {
        stamp(&mut out, page, patch, window)?;
        out.color = out.color.after_edit();
    }
    Ok(out)
}

/// The visible patches, in the order they composite, optionally cut off above
/// an exclusive `order`.
pub(crate) fn ordered_visible(patches: &[Patch], below: Option<u32>) -> Vec<&Patch> {
    let mut ordered: Vec<&Patch> = patches
        .iter()
        .filter(|p| p.visible)
        .filter(|p| below.is_none_or(|ceiling| p.order < ceiling))
        .collect();
    // Stable by `order`, then by id, so two patches with the same order
    // composite the same way on every run - the region order must be
    // fixed, and "fixed" has to survive a tie.
    ordered.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.id.cmp(&b.id)));
    ordered
}

/// A rectangle, inside the page. A window may be asked for off the edge; a crop
/// may not be taken there.
fn clamp_to(rect: Rect, page: &Raster) -> Rect {
    let x0 = rect.x.clamp(0, page.width as i64);
    let y0 = rect.y.clamp(0, page.height as i64);
    let x1 = rect.right().clamp(x0, page.width as i64);
    let y1 = rect.bottom().clamp(y0, page.height as i64);
    Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32)
}

/// One rectangle of a raster, as a raster of its own.
///
/// Sample by sample rather than by slice, because an arbitrary `x` lands
/// mid-byte at every sub-byte depth - the exact reason `Raster::rows` exists
/// and stops at rows. Everything but the size travels unchanged, so the crop's
/// mode, depth, palette and `tRNS` are the page's.
fn crop(page: &Raster, rect: Rect) -> Raster {
    let mut out = Raster {
        width: rect.w,
        height: rect.h,
        mode: page.mode,
        depth: page.depth,
        icc: page.icc.clone(),
        palette: page.palette.clone(),
        trns: page.trns.clone(),
        srgb_intent: page.srgb_intent,
        color: page.color.clone(),
        data: vec![0; blank_len(rect.w, rect.h, page)],
    };
    let samples = page.mode.samples();
    for y in 0..rect.h {
        for x in 0..rect.w {
            for channel in 0..samples {
                let value = page.sample(rect.x as u32 + x, rect.y as u32 + y, channel);
                out.set_sample(x, y, channel, value);
            }
        }
    }
    out
}

/// The byte length of a `w × h` buffer in a page's mode and depth, padded to a
/// byte boundary per row exactly as `Raster::stride` reads it.
pub(crate) fn blank_len(w: u32, h: u32, like: &Raster) -> usize {
    let bits = w as usize * like.mode.samples() * like.depth.bits() as usize;
    bits.div_ceil(8) * h as usize
}

/// Write one patch into `out`, where `out` covers `window` of the page.
fn stamp(
    out: &mut Raster,
    page: &Raster,
    patch: &Patch,
    window: Rect,
) -> Result<(), CompositeError> {
    if !patch.is_well_formed() {
        return Err(CompositeError::Malformed {
            id: patch.id.clone(),
            pw: patch.pixels.width,
            ph: patch.pixels.height,
            mw: patch.mask.bounds.w,
            mh: patch.mask.bounds.h,
        });
    }
    if patch.pixels.mode != page.mode || patch.pixels.depth != page.depth {
        return Err(CompositeError::ModeMismatch {
            id: patch.id.clone(),
            patch: patch.pixels.mode,
            patch_depth: patch.pixels.depth,
            page: page.mode,
            page_depth: page.depth,
        });
    }

    let bounds = patch.mask.bounds;
    let samples = page.mode.samples();
    let opacity = f64::from(patch.layer_style().opacity) / 100.0;
    if opacity <= 0.0 { return Ok(()); }
    let mut palette_blends = std::collections::HashMap::new();
    let top = (1u32 << page.depth.bits().min(16)) as f64 - 1.0;
    for y in bounds.y.max(window.y)..bounds.bottom().min(window.bottom()) {
        for x in bounds.x.max(window.x)..bounds.right().min(window.right()) {
            // Partial coverage (an anti-aliased rim) is a per-pixel opacity,
            // multiplied into the layer's own.
            let coverage = patch.mask.coverage(x, y);
            if coverage == 0 {
                continue;
            }
            if x < 0 || y < 0 || x >= page.width as i64 || y >= page.height as i64 {
                continue;
            }
            let opacity = opacity * f64::from(coverage) / 255.0;
            let (lx, ly) = ((x - bounds.x) as u32, (y - bounds.y) as u32);
            let (ox, oy) = ((x - window.x) as u32, (y - window.y) as u32);
            if page.mode == ColorMode::Indexed && opacity < 1.0 {
                let below = out.sample(ox, oy, 0);
                let above = patch.pixels.sample(lx, ly, 0);
                let value = *palette_blends.entry((below, above, coverage)).or_insert_with(|| {
                    blend_palette_index(page, &patch.pixels, below, above, opacity)
                });
                out.set_sample(ox, oy, 0, value);
                continue;
            }
            let alpha = page.mode.alpha_channel();
            let (lower_alpha, upper_alpha) = alpha.map_or((top, top), |channel| (
                out.sample(ox, oy, channel) as f64,
                patch.pixels.sample(lx, ly, channel) as f64,
            ));
            let mixed_alpha = lower_alpha * (1.0 - opacity) + upper_alpha * opacity;
            for channel in 0..samples {
                let value = patch.pixels.sample(lx, ly, channel);
                if opacity >= 1.0 {
                    out.set_sample(ox, oy, channel, value);
                } else {
                    let below = out.sample(ox, oy, channel) as f64;
                    let mixed = if alpha.is_some() && alpha != Some(channel) && mixed_alpha > 0.0 {
                        (below * lower_alpha * (1.0 - opacity) + value as f64 * upper_alpha * opacity) / mixed_alpha
                    } else {
                        below * (1.0 - opacity) + value as f64 * opacity
                    };
                    out.set_sample(ox, oy, channel, mixed.round().clamp(0.0, top) as u16);
                }
            }
        }
    }
    Ok(())
}

/// Palette positions have no numerical relationship to color. Blend the
/// colors, then quantize to the existing palette so untouched pixels and the
/// source's indexed format remain unchanged.
fn blend_palette_index(page: &Raster, patch: &Raster, below: u16, above: u16, opacity: f64) -> u16 {
    let Some(palette) = page.palette.as_deref() else { return below; };
    let upper_palette = patch.palette.as_deref().unwrap_or(palette);
    let color = |palette: &[u8], trns: Option<&[u8]>, index: u16| -> Option<[f64; 4]> {
        let rgb = palette.get(index as usize * 3..index as usize * 3 + 3)?;
        Some([rgb[0] as f64, rgb[1] as f64, rgb[2] as f64,
            trns.and_then(|alpha| alpha.get(index as usize)).copied().unwrap_or(255) as f64])
    };
    let Some(lower) = color(palette, page.trns.as_deref(), below) else { return below; };
    let Some(upper) = color(upper_palette, patch.trns.as_deref().or(page.trns.as_deref()), above) else { return below; };
    let mixed_alpha = lower[3] * (1.0 - opacity) + upper[3] * opacity;
    let target: [f64; 4] = std::array::from_fn(|i| {
        if i < 3 && mixed_alpha > 0.0 {
            (lower[i] * lower[3] * (1.0 - opacity) + upper[i] * upper[3] * opacity) / mixed_alpha
        } else { lower[i] * (1.0 - opacity) + upper[i] * opacity }
    });
    (0..(palette.len() / 3).min(1usize << page.depth.bits()))
        .map(|index| {
            let candidate = color(palette, page.trns.as_deref(), index as u16).unwrap();
            let distance: f64 = candidate.iter().zip(target).map(|(a, b)| (a - b).powi(2)).sum();
            (index as u16, distance)
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map_or(below, |(index, _)| index)
}

/// Every pixel where two rasters of the same geometry differ.
///
/// The fidelity test's left-hand side. Returned as coordinates rather than a
/// count so a failure names the pixel it failed on - a count tells you the test
/// broke and nothing about where.
pub fn changed_pixels(before: &Raster, after: &Raster) -> Vec<(u32, u32)> {
    if !before.same_geometry(after) {
        // A geometry change is a total change; reporting it as "every pixel"
        // keeps the caller's assertion honest rather than silently empty.
        return (0..after.height)
            .flat_map(|y| (0..after.width).map(move |x| (x, y)))
            .collect();
    }
    let samples = before.mode.samples();
    let mut changed = Vec::new();
    for y in 0..before.height {
        for x in 0..before.width {
            if (0..samples).any(|c| before.sample(x, y, c) != after.sample(x, y, c)) {
                changed.push((x, y));
            }
        }
    }
    changed
}

/// `dilate(union(applied masks), edit_margin)` - the right-hand side of the
/// contract. Built here so the test and the exporter agree on what it means.
pub fn permitted_region(patches: &[Patch], page_w: u32, page_h: u32) -> Mask {
    let masks: Vec<&Mask> = patches
        .iter()
        .filter(|p| p.visible)
        .map(|p| &p.mask)
        .collect();
    if masks.is_empty() {
        return Mask::empty(Rect::new(0, 0, 0, 0));
    }
    Mask::union(&masks, page_w, page_h).dilated(crate::constants::EDIT_MARGIN, page_w, page_h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{fixtures, BitDepth};
    use crate::patch::{Engine, Provenance};

    fn provenance() -> Provenance {
        Provenance {
            engine: Engine::Fill,
            engine_version: "test".into(),
            model_sha256: None,
            execution_provider: "cpu".into(),
            params_snapshot: "{}".into(),
            mask_sha256: "0".repeat(64),
            source_sha256: "0".repeat(64),
            cloud: None,
            created: 0,
        }
    }

    /// A patch whose pixel buffer differs from the page **everywhere in its
    /// bbox**, but whose mask covers only the middle. If the compositor wrote
    /// the buffer rather than the masked part of it, the subset assertion
    /// fails - which is the only reason to build the fixture this way.
    fn synthetic_patch(page: &Raster, bounds: Rect) -> Patch {
        let mut pixels = Raster {
            width: bounds.w,
            height: bounds.h,
            mode: page.mode,
            depth: page.depth,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None,
            color: Default::default(),
            data: vec![
                0;
                {
                    let bits = bounds.w as usize * page.mode.samples() * page.depth.bits() as usize;
                    bits.div_ceil(8) * bounds.h as usize
                }
            ],
        };
        let ceiling = match page.depth {
            BitDepth::Sixteen => u16::MAX,
            other => (1u16 << other.bits()) - 1,
        };
        for y in 0..bounds.h {
            for x in 0..bounds.w {
                for channel in 0..page.mode.samples() {
                    let source = page.sample((bounds.x as u32) + x, (bounds.y as u32) + y, channel);
                    // Guaranteed different, and inside the depth's range for
                    // indexed sources too - where a value past the palette
                    // would not survive a round-trip.
                    let value = if page.mode == ColorMode::Indexed {
                        let entries = (page.palette.as_ref().map_or(1, |p| p.len() / 3)) as u16;
                        (source + 1) % entries
                    } else {
                        ceiling - source
                    };
                    pixels.set_sample(x, y, channel, value);
                }
            }
        }

        let mut mask = Mask::empty(bounds);
        for y in bounds.y + 1..bounds.bottom() - 1 {
            for x in bounds.x + 1..bounds.right() - 1 {
                mask.set(x, y, true);
            }
        }

        Patch {
            id: "p1".into(),
            ink: mask.clone(),
            mask,
            pixels,
            order: 0,
            visible: true,
            provenance: provenance(),
        }
    }

    #[test]
    fn translucent_layer_does_not_darken_over_transparent_pixels() {
        let page = Raster {
            width: 4, height: 4, mode: ColorMode::GrayAlpha, depth: BitDepth::Eight,
            icc: None, trns: None, srgb_intent: None, palette: None,
            color: Default::default(),
            data: vec![0; 32],
        };
        let mut patch = synthetic_patch(&page, Rect::new(0, 0, 4, 4));
        patch.provenance.params_snapshot = serde_json::json!({"layer": {"opacity": 50}});
        let result = composite(&page, &[patch]).unwrap();
        assert_eq!(result.sample(1, 1, 0), 255);
        assert_eq!(result.sample(1, 1, 1), 128);
        assert_eq!(result.sample(0, 0, 1), 0);
    }

    /// 0, 50 and 100 percent, on every pixel of the mask and none outside
    /// it: nothing, the rounded half-way blend, and the patch itself.
    #[test]
    fn zero_half_and_full_opacity_write_the_expected_pixels() {
        let page = fixtures::by_name("l8").raster;
        let bounds = Rect::new(8, 8, 12, 10);
        for (opacity, expect) in [
            (0u8, (|below: u16| below) as fn(u16) -> u16),
            (50, |below: u16| ((below as f64 + 200.0) / 2.0).round() as u16),
            (100, |_below: u16| 200),
        ] {
            let mut patch = synthetic_patch(&page, bounds);
            patch.pixels.data.fill(200);
            patch.provenance.params_snapshot = serde_json::json!({"layer": {"opacity": opacity}});
            let out = composite(&page, std::slice::from_ref(&patch)).unwrap();
            for y in 0..page.height {
                for x in 0..page.width {
                    let below = page.sample(x, y, 0);
                    let wanted = if patch.mask.contains(x as i64, y as i64) { expect(below) } else { below };
                    assert_eq!(out.sample(x, y, 0), wanted, "{opacity}% at ({x}, {y})");
                }
            }
            let window = composite_region(&page, std::slice::from_ref(&patch), bounds, None).unwrap();
            assert_eq!(window.sample(2, 2, 0), out.sample(10, 10, 0), "{opacity}% window");
        }
    }

    #[test]
    fn partial_opacity_blends_colors_instead_of_palette_positions() {
        let page = Raster {
            width: 4, height: 4, mode: ColorMode::Indexed, depth: BitDepth::Eight,
            icc: None, trns: None, srgb_intent: None,
            color: Default::default(),
            // A deliberately non-monotonic palette: index arithmetic would
            // blend black (0) + white (2) into bright red (1), not gray (3).
            palette: Some(vec![0, 0, 0, 255, 0, 0, 255, 255, 255, 128, 128, 128]),
            data: vec![0; 16],
        };
        let mut patch = synthetic_patch(&page, Rect::new(0, 0, 4, 4));
        patch.pixels.data.fill(2);
        patch.provenance.params_snapshot = serde_json::json!({"layer": {"opacity": 50}});
        let result = composite(&page, std::slice::from_ref(&patch)).unwrap();
        assert_eq!(result.sample(1, 1, 0), 3);
        assert_eq!(result.sample(0, 0, 0), 0);
        assert_eq!(result.palette, page.palette);
        let crop = composite_region(&page, &[patch], Rect::new(1, 1, 1, 1), None).unwrap();
        assert_eq!(crop.sample(0, 0, 0), 3);
    }

    #[test]
    fn saved_layer_transform_and_opacity_match_flattened_export() {
        use crate::export::{export_page, Target};
        use crate::image::{decode, encode, Format};
        let page = fixtures::by_name("l8").raster;
        let mut patch = synthetic_patch(&page, Rect::new(8, 8, 8, 6));
        patch.provenance.params_snapshot = serde_json::json!({"layer": {
            "opacity": 50, "offsetX": 20, "offsetY": 12,
            "rotation": 90.0, "locked": true
        }});
        let presented = patch.presented();
        assert_eq!(presented.layer_style().opacity, 50);
        assert!(presented.layer_style().locked);
        assert_ne!(presented.mask.bounds, Rect::new(8, 8, 8, 6));

        let composed = composite(&page, std::slice::from_ref(&presented)).unwrap();
        assert_eq!(composed.sample(10, 10, 0), page.sample(10, 10, 0));
        let (x, y) = (presented.mask.bounds.x..presented.mask.bounds.right())
            .flat_map(|x| (presented.mask.bounds.y..presented.mask.bounds.bottom()).map(move |y| (x, y)))
            .find(|&(x, y)| presented.mask.contains(x, y)).unwrap();
        let local = ((x - presented.mask.bounds.x) as u32, (y - presented.mask.bounds.y) as u32);
        let expected = (page.sample(x as u32, y as u32, 0) as f64 * 0.5
            + presented.pixels.sample(local.0, local.1, 0) as f64 * 0.5).round() as u16;
        assert_eq!(composed.sample(x as u32, y as u32, 0), expected);

        let source = encode(&page, Format::Png).unwrap();
        let exported = export_page(&source, &[presented], Target::SameAsSource).unwrap();
        let flattened = decode(&exported.bytes).unwrap();
        assert_eq!(flattened.data, composed.data);
    }

    #[test]
    fn a_turned_layer_is_resampled_smoothly_and_a_moved_one_exactly() {
        let page = fixtures::by_name("l8").raster;
        let bounds = Rect::new(8, 8, 20, 20);
        let mut patch = synthetic_patch(&page, bounds);
        patch.mask = Mask::filled(bounds);
        patch.pixels.data.iter_mut().for_each(|value| *value = 200);

        patch.provenance.params_snapshot = serde_json::json!({"layer": {"offsetX": 5, "offsetY": 3}});
        let moved = patch.clone().presented();
        assert_eq!(moved.mask, Mask { bounds: Rect::new(13, 11, 20, 20), bits: vec![255; 400] });
        assert!(moved.pixels.data.iter().all(|&value| value == 200));

        patch.provenance.params_snapshot = serde_json::json!({"layer": {"rotation": 30.0}});
        let turned = patch.presented();
        let partial = turned.mask.bits.iter().filter(|&&c| c > 0 && c < 255).count();
        assert!(partial > 40, "a turned edge is anti-aliased, not stair-stepped ({partial} partial pixels)");
        assert_eq!(turned.mask.coverage(18, 18), 255, "the middle stays solid");
        // Pixels outside the source mask never bleed in: every covered pixel is
        // the layer's own colour.
        for (index, &coverage) in turned.mask.bits.iter().enumerate() {
            if coverage > 0 {
                assert_eq!(turned.pixels.data[index], 200);
            }
        }
    }

    #[test]
    fn shrinking_a_text_shaped_layer_reveals_its_exact_lower_composite() {
        use crate::text_shape::{round_source_dilate, MaskRaster};
        for name in ["l8", "rgba8", "l16"] {
            let page = fixtures::by_name(name).raster;
            let mut base = Mask::empty(Rect::new(20, 20, 1, 1));
            base.set(20, 20, true);
            let base = MaskRaster::from(base);
            let small = round_source_dilate(&base, 2, page.width, page.height).unwrap().to_mask();
            let large = round_source_dilate(&base, 5, page.width, page.height).unwrap().to_mask();
            let mut lower_patch = synthetic_patch(&page, Rect::new(8, 8, 32, 30));
            lower_patch.id = "lower".into();
            let lower = composite(&page, &[lower_patch.clone()]).unwrap();
            let mut top_large = synthetic_patch(&page, large.bounds);
            top_large.id = "text".into();
            top_large.order = 1;
            top_large.mask = large.clone();
            let (x, y) = (25, 20);
            let changed = if lower.sample(x, y, 0) == 0 { 255 } else { 0 };
            top_large.pixels.set_sample((x as i64 - large.bounds.x) as u32,
                                        (y as i64 - large.bounds.y) as u32, 0, changed);
            let wide = composite(&page, &[lower_patch.clone(), top_large.clone()]).unwrap();
            assert_ne!(wide.sample(x, y, 0), lower.sample(x, y, 0), "{name}");

            let mut top_small = synthetic_patch(&page, small.bounds);
            top_small.id = "text".into();
            top_small.order = 1;
            top_small.mask = small.clone();
            let shrunk = composite(&page, &[lower_patch, top_small]).unwrap();
            for py in 0..page.height {
                for px in 0..page.width {
                    if small.contains(px as i64, py as i64) { continue; }
                    for channel in 0..page.mode.samples() {
                        assert_eq!(shrunk.sample(px, py, channel), lower.sample(px, py, channel),
                                   "{name} failed to restore lower sample at {px},{py}");
                    }
                }
            }
        }
    }

    #[test]
    fn nothing_changes_under_an_empty_patch_set() {
        for fixture in fixtures::all() {
            let out = composite(&fixture.raster, &[]).unwrap();
            assert!(
                changed_pixels(&fixture.raster, &out).is_empty(),
                "{}",
                fixture.name
            );
        }
    }

    /// The half of the fidelity test that passthrough cannot satisfy with
    /// `cp`, on every fixture: changed pixels is a subset of
    /// dilate(union(masks), edit_margin).
    #[test]
    fn a_non_empty_patch_set_changes_only_what_it_is_allowed_to() {
        for fixture in fixtures::all() {
            let page = &fixture.raster;
            let bounds = Rect::new(8, 8, 24, 20);
            let patch = synthetic_patch(page, bounds);
            let out = composite(page, std::slice::from_ref(&patch)).unwrap();

            let changed = changed_pixels(page, &out);
            assert!(
                !changed.is_empty(),
                "{}: the patch changed nothing",
                fixture.name
            );

            let permitted = permitted_region(std::slice::from_ref(&patch), page.width, page.height);
            for (x, y) in &changed {
                assert!(
                    permitted.contains(*x as i64, *y as i64),
                    "{}: ({x}, {y}) changed outside the permitted region",
                    fixture.name
                );
            }

            // Tighter than the contract, and true of this compositor: it writes
            // only inside the mask itself, never into the margin. If this ever
            // fails while the assertion above passes, an engine's output has
            // started leaking and the margin is absorbing it.
            for (x, y) in &changed {
                assert!(
                    patch.mask.contains(*x as i64, *y as i64),
                    "{}: ({x}, {y}) is in the margin but outside the mask",
                    fixture.name
                );
            }
        }
    }

    #[test]
    fn an_invisible_patch_writes_nothing() {
        let page = fixtures::by_name("rgb8").raster;
        let mut patch = synthetic_patch(&page, Rect::new(8, 8, 24, 20));
        patch.visible = false;
        let out = composite(&page, &[patch]).unwrap();
        assert!(changed_pixels(&page, &out).is_empty());
    }

    #[test]
    fn later_patches_win_and_ties_break_deterministically() {
        let page = fixtures::by_name("l8").raster;
        let bounds = Rect::new(8, 8, 8, 8);

        let mut first = synthetic_patch(&page, bounds);
        first.id = "a".into();
        let mut second = synthetic_patch(&page, bounds);
        second.id = "b".into();
        for y in 0..bounds.h {
            for x in 0..bounds.w {
                second.pixels.set_sample(x, y, 0, 200);
            }
        }

        let forwards = composite(&page, &[first.clone(), second.clone()]).unwrap();
        let backwards = composite(&page, &[second, first]).unwrap();
        assert_eq!(
            forwards.data, backwards.data,
            "argument order changed the result"
        );
        assert_eq!(
            forwards.sample(10, 10, 0),
            200,
            "the later id did not win the tie"
        );
    }

    /* -- the windowed composite ---------------------------------------- */

    /// **The window is the page, cropped, and nothing else.** Stated over every
    /// fixture mode and depth, including the sub-byte ones where an arbitrary
    /// `x` lands mid-byte - which is the exact reason `Raster::rows` stops at
    /// rows and this had to be written rather than reached for.
    ///
    /// A separate implementation that merely *usually* agrees with `composite`
    /// is two pages, and the fidelity guarantee is stated over one.
    #[test]
    fn a_window_equals_the_full_composite_cropped_to_it() {
        for fixture in fixtures::all() {
            let page = &fixture.raster;
            let patch = synthetic_patch(page, Rect::new(8, 8, 24, 20));
            let patches = std::slice::from_ref(&patch);
            let whole = composite(page, patches).unwrap();

            // One window inside the patch, one straddling its corner, one
            // entirely off it, and one that is the whole page.
            for rect in [
                Rect::new(12, 12, 6, 5),
                Rect::new(4, 4, 12, 12),
                Rect::new(40, 4, 10, 10),
                Rect::new(0, 0, page.width, page.height),
            ] {
                let window = composite_region(page, patches, rect, None).unwrap();
                assert_eq!(
                    (window.width, window.height),
                    (rect.w, rect.h),
                    "{}",
                    fixture.name
                );
                assert_eq!(window.mode, page.mode, "{}", fixture.name);
                assert_eq!(window.depth, page.depth, "{}", fixture.name);
                for y in 0..rect.h {
                    for x in 0..rect.w {
                        for c in 0..page.mode.samples() {
                            assert_eq!(
                                window.sample(x, y, c),
                                whole.sample(rect.x as u32 + x, rect.y as u32 + y, c),
                                "{} at ({x}, {y}) channel {c} of {rect:?}",
                                fixture.name
                            );
                        }
                    }
                }
            }
        }
    }

    /// A window off the edge is clamped rather than refused: a brush stroke
    /// that runs past the page is an ordinary gesture, and the answer to it is
    /// the part of the page it actually crossed.
    #[test]
    fn a_window_off_the_page_is_clamped_to_what_is_there() {
        let page = fixtures::by_name("rgb8").raster;
        let clipped = composite_region(&page, &[], Rect::new(-10, -10, 20, 20), None).unwrap();
        assert_eq!((clipped.width, clipped.height), (10, 10));
        assert_eq!(clipped.sample(0, 0, 0), page.sample(0, 0, 0));

        let outside = composite_region(&page, &[], Rect::new(500, 500, 8, 8), None).unwrap();
        assert_eq!((outside.width, outside.height), (0, 0));
    }

    /// **The order ceiling is exclusive, and it is what "the surface under this
    /// stroke" means.** A patch about to take order `n` blends over the patches
    /// below it and not over itself - otherwise re-running a stroke would paint
    /// over its own previous commit and compound every time.
    #[test]
    fn an_order_ceiling_composites_only_what_is_under_it() {
        let page = fixtures::by_name("l8").raster;
        let bounds = Rect::new(8, 8, 8, 8);
        let flat = |id: &str, order: u32, value: u16| {
            let mut patch = synthetic_patch(&page, bounds);
            patch.id = id.into();
            patch.order = order;
            for y in 0..bounds.h {
                for x in 0..bounds.w {
                    patch.pixels.set_sample(x, y, 0, value);
                }
            }
            patch
        };
        let patches = [flat("under", 0, 30), flat("over", 1, 200)];
        let rect = Rect::new(9, 9, 4, 4);

        let all = composite_region(&page, &patches, rect, None).unwrap();
        assert_eq!(all.sample(1, 1, 0), 200);

        let below = composite_region(&page, &patches, rect, Some(1)).unwrap();
        assert_eq!(
            below.sample(1, 1, 0),
            30,
            "the ceiling let the patch at its own order in"
        );

        let none = composite_region(&page, &patches, rect, Some(0)).unwrap();
        assert_eq!(
            none.sample(1, 1, 0),
            page.sample(10, 10, 0),
            "a zero ceiling is the raw page"
        );
    }

    #[test]
    fn each_layer_preserves_its_immediate_underlay_outside_the_mask_and_export_matches_preview() {
        use crate::export::{export_page, Target};
        use crate::image::{decode, encode, lossless_format_for};
        for fixture in fixtures::all() {
            let page = &fixture.raster;
            let mut a = synthetic_patch(page, Rect::new(8, 8, 24, 20));
            a.id = "a".into();
            let mut b = synthetic_patch(page, Rect::new(16, 12, 24, 20));
            b.id = "b".into();
            b.order = 1;
            let underlay = composite(page, std::slice::from_ref(&a)).unwrap();
            let preview = composite(page, &[a.clone(), b.clone()]).unwrap();
            for y in 0..page.height {
                for x in 0..page.width {
                    if b.mask.contains(x as i64, y as i64) {
                        continue;
                    }
                    for channel in 0..page.mode.samples() {
                        assert_eq!(
                            preview.sample(x, y, channel),
                            underlay.sample(x, y, channel),
                            "{} at ({x},{y}) channel {channel}",
                            fixture.name
                        );
                    }
                }
            }
            let source = encode(page, lossless_format_for(page)).unwrap();
            let exported = export_page(&source, &[a, b], Target::SameAsSource).unwrap();
            let decoded = decode(&exported.bytes).unwrap();
            assert_eq!(
                decoded.data, preview.data,
                "{} preview/export pixel mismatch",
                fixture.name
            );
        }
    }

    #[test]
    fn a_patch_in_the_wrong_mode_is_refused_rather_than_converted() {
        let page = fixtures::by_name("rgb8").raster;
        let mut patch = synthetic_patch(&page, Rect::new(8, 8, 8, 8));
        patch.pixels = fixtures::by_name("l8").raster;
        patch.pixels.width = 8;
        patch.pixels.height = 8;
        assert!(matches!(
            composite(&page, &[patch]),
            Err(CompositeError::ModeMismatch { .. })
        ));
    }
}
