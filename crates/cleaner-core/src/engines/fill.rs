//! Rung 0 - fill.
//!
//! One flat colour, painted through the mask and nowhere else. The colour is
//! the per-channel median of the paper just outside the mask:
//! [`crate::fit::Fitted::paper`], the thin annulus the route was decided from.
//! No plane and no gradient. The page is denoised before it is cleaned, so the
//! paper beside a balloon's text is flat, and a flat colour is the whole
//! answer.
//!
//! The work is done in the page's own sample space, which is the difference
//! between a fill that preserves colour fidelity and one that quietly promotes
//! a grayscale page to RGB.

use crate::fit::{Fitted, ring};
use crate::image::{ColorMode, Raster};

/// The pixels a fill produces, covering `mask.bounds`: the median of the
/// paper ring around the mask, one value per channel, inside the mask.
///
/// Both the colour and the route are measured over `fitted.paper` rather than
/// over a fresh annulus. See [`crate::fit::Fitted::paper`]: a ring that reaches
/// across a bubble stroke picks up the stroke, and a fill measured from a
/// different set than the routing is a fill nobody checked. On an indexed page
/// the median is a palette index the ring already holds, so the patch stays on
/// one entry of the page's own palette.
pub fn render(page: &Raster, fitted: &Fitted) -> Raster {
    let bounds = fitted.mask.bounds;
    let samples = page.mode.samples();

    let mut patch = Raster {
        width: bounds.w,
        height: bounds.h,
        mode: page.mode,
        depth: page.depth,
        icc: page.icc.clone(),
        palette: page.palette.clone(),
        trns: page.trns.clone(),
        srgb_intent: page.srgb_intent,
        color: page.color.clone(),
        data: vec![
            0;
            {
                let bits = bounds.w as usize * samples * page.depth.bits() as usize;
                bits.div_ceil(8) * bounds.h as usize
            }
        ],
    };

    let medians = ring::fill_medians(page, &fitted.paper);

    for y in 0..bounds.h {
        for x in 0..bounds.w {
            let (px, py) = (bounds.x + x as i64, bounds.y + y as i64);
            let inside = fitted.mask.contains(px, py)
                && crate::image::color::alpha(page, px as u32, py as u32) > 0.0;
            for (channel, &median) in medians.iter().enumerate() {
                // Outside the mask, or on the alpha channel: keep the source
                // pixel. Alpha is not part of what median fill replaces -
                // only the colour channels get the ring's median.
                let value = if !inside || page.mode.alpha_channel() == Some(channel) {
                    page.sample(px as u32, py as u32, channel)
                } else {
                    if page.mode == ColorMode::Indexed {
                        opaque_palette_median(page, page.sample(px as u32, py as u32, 0), median)
                    } else {
                        median
                    }
                };
                patch.set_sample(x, y, channel, value);
            }
            if inside {
                crate::image::color::avoid_transparent_key(&mut patch, x, y);
            }
        }
    }
    patch
}

/// A palette index includes alpha. A ring median with another alpha cannot
/// replace this pixel; retain its original index rather than invent opacity.
fn opaque_palette_median(page: &Raster, original: u16, median: u16) -> u16 {
    let alpha = |i: u16| {
        page.trns
            .as_ref()
            .and_then(|t| t.get(i as usize))
            .copied()
            .unwrap_or(255)
    };
    if alpha(original) == alpha(median) {
        median
    } else {
        original
    }
}

/// The pixels a solid color fill produces, covering `mask.bounds`.
///
/// Unlike [`render_solid`] (which samples median tone from surroundings),
/// this paints the masked area with a specified RGB color (`[r, g, b]`),
/// adapted to the raster's color mode and bit depth while preserving
/// unchanged outside regions and alpha channels.
pub fn render_solid_color(
    page: &Raster,
    fitted: &Fitted,
    color: [u8; 3],
) -> Result<Raster, crate::image::ImageError> {
    let bounds = fitted.mask.bounds;
    let samples = page.mode.samples();

    let mut patch = Raster {
        width: bounds.w,
        height: bounds.h,
        mode: page.mode,
        depth: page.depth,
        icc: page.icc.clone(),
        palette: page.palette.clone(),
        trns: page.trns.clone(),
        srgb_intent: page.srgb_intent,
        color: page.color.clone(),
        data: vec![
            0;
            {
                let bits = bounds.w as usize * samples * page.depth.bits() as usize;
                bits.div_ceil(8) * bounds.h as usize
            }
        ],
    };

    let fill_samples = selected_samples(page, color)?;
    let mut palette_by_alpha = std::collections::HashMap::new();

    for y in 0..bounds.h {
        for x in 0..bounds.w {
            let (px, py) = (bounds.x + x as i64, bounds.y + y as i64);
            let inside = fitted.mask.contains(px, py)
                && crate::image::color::alpha(page, px as u32, py as u32) > 0.0;
            let local_samples = if inside && page.mode == ColorMode::Indexed && page.trns.is_some()
            {
                let index = page.sample(px as u32, py as u32, 0) as usize;
                let alpha = page
                    .trns
                    .as_ref()
                    .and_then(|t| t.get(index))
                    .copied()
                    .unwrap_or(255);
                vec![*match palette_by_alpha.entry(alpha) {
                    std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert(palette_sample(page, color, Some(alpha))?)
                    }
                }]
            } else {
                fill_samples.clone()
            };
            for (channel, &sample_val) in local_samples.iter().enumerate() {
                let value = if !inside || page.mode.alpha_channel() == Some(channel) {
                    page.sample(px as u32, py as u32, channel)
                } else {
                    sample_val
                };
                patch.set_sample(x, y, channel, value);
            }
            if inside {
                crate::image::color::avoid_transparent_key(&mut patch, x, y);
            }
        }
    }
    Ok(patch)
}

/// UI sRGB selection converted before quantization into the native sample space.
pub fn selected_samples(
    page: &Raster,
    color: [u8; 3],
) -> Result<Vec<u16>, crate::image::ImageError> {
    crate::image::color::validate_editing(page)?;
    let managed = crate::image::color::ManagedColor::for_raster(page)?;
    let rgb = color.map(|v| v as f32 / 255.0);
    let max = ((1u32 << page.depth.bits()) - 1) as f32;
    let fill_samples: Vec<u16> = if page.mode == ColorMode::Indexed {
        vec![palette_sample(page, color, None)?]
    } else {
        managed
            .from_srgb(rgb, page.mode)?
            .iter()
            .take(page.mode.samples())
            .map(|v| (v.clamp(0.0, 1.0) * max).round() as u16)
            .collect()
    };
    Ok(fill_samples)
}

fn palette_sample(
    page: &Raster,
    color: [u8; 3],
    alpha: Option<u8>,
) -> Result<u16, crate::image::ImageError> {
    let managed = crate::image::color::ManagedColor::for_raster(page)?;
    let mut best = (f32::INFINITY, None);
    for (i, entry) in page
        .palette
        .as_deref()
        .unwrap_or_default()
        .chunks_exact(3)
        .enumerate()
    {
        let entry_alpha = page
            .trns
            .as_ref()
            .and_then(|t| t.get(i))
            .copied()
            .unwrap_or(255);
        if alpha.is_some_and(|a| a != entry_alpha) {
            continue;
        }
        let shown = managed.to_srgb([
            entry[0] as f32 / 255.0,
            entry[1] as f32 / 255.0,
            entry[2] as f32 / 255.0,
            0.0,
        ]);
        let distance = (0..3)
            .map(|c| {
                (crate::image::color::linear(shown[c])
                    - crate::image::color::linear(color[c] as f32 / 255.0))
                .powi(2)
            })
            .sum();
        if distance < best.0 {
            best = (distance, Some(i as u16));
        }
    }
    best.1.ok_or_else(|| {
        crate::image::ImageError::Color("indexed source has no compatible palette entry".into())
    })
}

/// Whether a palette/packed-gray source can only approximate this selection.
pub fn color_is_approximated(
    page: &Raster,
    color: [u8; 3],
) -> Result<bool, crate::image::ImageError> {
    if page.mode != ColorMode::Indexed && page.depth.bits() >= 8 {
        return Ok(false);
    }
    let managed = crate::image::color::ManagedColor::for_raster(page)?;
    let mut candidates = vec![selected_samples(page, color)?];
    if page.mode == ColorMode::Indexed {
        // An exact match at a different opacity cannot be used for this page.
        let mut alphas = std::collections::HashSet::new();
        for i in 0..page.palette.as_deref().unwrap_or_default().len() / 3 {
            let alpha = page
                .trns
                .as_ref()
                .and_then(|t| t.get(i))
                .copied()
                .unwrap_or(255);
            if alpha > 0 && alphas.insert(alpha) {
                candidates.push(vec![palette_sample(page, color, Some(alpha))?]);
            }
        }
    }
    let mut pixel = Raster {
        width: 1,
        height: 1,
        mode: page.mode,
        depth: page.depth,
        icc: page.icc.clone(),
        palette: page.palette.clone(),
        trns: None,
        srgb_intent: page.srgb_intent,
        color: page.color.clone(),
        data: vec![0; (page.mode.samples() * page.depth.bits() as usize).div_ceil(8)],
    };
    for samples in candidates {
        for (c, v) in samples.iter().enumerate() {
            pixel.set_sample(0, 0, c, *v);
        }
        let shown = managed.pixel(&pixel, 0, 0);
        if (0..3).any(|c| (shown[c] * 255.0 - color[c] as f32).abs() > 1.0) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The tone a fill would use, for the review row and for the cluster
/// reconciliation. Luma, so two regions on different colour pages still
/// compare.
pub fn tone(fitted: &Fitted) -> u16 {
    fitted.ring.median
}

/// Whether two regions' tones are close enough to pool.
///
/// §7: "Pool only across regions whose ring medians agree within ~3 levels; a
/// page with a white panel and a grey-toned panel keeps two tones." Three
/// 8-bit levels, in 16-bit luma.
pub fn tones_agree(a: u16, b: u16) -> bool {
    (a as i32 - b as i32).abs() <= 3 * 257
}

/// Group regions into tone clusters, so a fill tone is reconciled across a
/// panel rather than pooled across a page.
pub fn cluster(tones: &[u16]) -> Vec<usize> {
    // Single-link over a sorted order, which for a one-dimensional quantity is
    // the whole of the clustering: two tones join if they agree, and a chain of
    // agreements is one cluster.
    let mut order: Vec<usize> = (0..tones.len()).collect();
    order.sort_by_key(|i| tones[*i]);

    let mut assignment = vec![0usize; tones.len()];
    let mut cluster = 0usize;
    for (rank, &index) in order.iter().enumerate() {
        if rank > 0 {
            let previous = order[rank - 1];
            if !tones_agree(tones[previous], tones[index]) {
                cluster += 1;
            }
        }
        assignment[index] = cluster;
    }
    assignment
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fit::{self, Route};
    use crate::image::{BitDepth, fixtures};
    use crate::mask::{Mask, Rect};

    fn gray_page(w: u32, h: u32, f: impl Fn(u32, u32) -> u8) -> Raster {
        let mut data = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            for x in 0..w {
                data.push(f(x, y));
            }
        }
        Raster {
            width: w,
            height: h,
            mode: ColorMode::Gray,
            depth: BitDepth::Eight,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None, color: Default::default(),
            data,
        }
    }

    fn fitted_over(page: &Raster, rect: Rect) -> Fitted {
        let seed = Mask::filled(rect);
        fit::fit(page, &seed, 1.0, 0.0, &fit::EdgeMap::none(page.width, page.height), true)
    }

    #[test]
    fn a_fill_on_cream_paper_paints_cream_rather_than_white() {
        // The near-white snap that is refused would paint 255 here.
        let page = gray_page(80, 80, |_, _| 246);
        let fitted = fitted_over(&page, Rect::new(30, 30, 20, 20));
        let patch = render(&page, &fitted);
        assert_eq!(patch.sample(10, 10, 0), 246);
    }

    #[test]
    fn a_gradient_ring_is_filled_with_one_flat_colour() {
        // No plane: the ring's median, the same value across the whole mask.
        let page = gray_page(80, 80, |x, _| (150 + x) as u8);
        let fitted = fitted_over(&page, Rect::new(30, 30, 20, 20));
        let patch = render(&page, &fitted);
        let left = patch.sample(2, 10, 0);
        let right = patch.sample(17, 10, 0);
        assert_eq!(left, right, "the fill carried a gradient");
        assert_eq!(left, ring::fill_medians(&page, &fitted.paper)[0]);
    }

    #[test]
    fn pixels_outside_the_mask_are_the_page_untouched() {
        let page = gray_page(80, 80, |x, y| ((x * 3 + y * 5) % 251) as u8);
        let mut fitted = fitted_over(&page, Rect::new(30, 30, 20, 20));
        // Punch a hole so part of the bbox is outside the mask.
        fitted.mask.set(35, 35, false);
        let patch = render(&page, &fitted);
        assert_eq!(patch.sample(5, 5, 0), page.sample(35, 35, 0));
    }

    #[test]
    fn an_alpha_channel_is_copied_rather_than_filled() {
        let page = fixtures::by_name("rgba8").raster;
        let fitted = fitted_over(&page, Rect::new(20, 16, 12, 10));
        let patch = render(&page, &fitted);
        for y in 0..patch.height {
            for x in 0..patch.width {
                assert_eq!(
                    patch.sample(x, y, 3),
                    page.sample(20 + x, 16 + y, 3),
                    "alpha changed at {x},{y}"
                );
            }
        }
    }

    #[test]
    fn an_indexed_fill_stays_on_a_single_palette_index() {
        let page = fixtures::by_name("indexed-p").raster;
        let fitted = fitted_over(&page, Rect::new(20, 16, 12, 10));
        let patch = render(&page, &fitted);
        let entries = page.palette.as_ref().unwrap().len() / 3;
        let mut inside: Vec<u16> = Vec::new();
        for y in 0..patch.height {
            for x in 0..patch.width {
                let (px, py) = (20 + x as i64, 16 + y as i64);
                if fitted.mask.contains(px, py) {
                    inside.push(patch.sample(x, y, 0));
                }
            }
        }
        assert!(!inside.is_empty());
        assert!(inside.iter().all(|v| *v == inside[0]), "a gradient was written into indices");
        assert!((inside[0] as usize) < entries, "the index is outside the palette");
    }

    #[test]
    fn a_white_panel_and_a_grey_panel_keep_two_tones() {
        // Pooled per page this would be one average tone on both.
        let tones = [246 * 257, 247 * 257, 180 * 257, 181 * 257];
        let clusters = cluster(&tones);
        assert_eq!(clusters[0], clusters[1]);
        assert_eq!(clusters[2], clusters[3]);
        assert_ne!(clusters[0], clusters[2]);
    }

    #[test]
    fn a_flat_region_routes_to_the_fill_and_a_toned_one_does_not() {
        let flat = gray_page(80, 80, |_, _| 246);
        assert_eq!(fitted_over(&flat, Rect::new(30, 30, 20, 20)).route, Route::Fill);

        let toned = gray_page(80, 80, |x, y| if x % 8 < 3 && y % 8 < 3 { 40 } else { 250 });
        assert_eq!(fitted_over(&toned, Rect::new(30, 30, 20, 20)).route, Route::Inpaint);
    }

    #[test]
    fn a_fill_paints_the_surrounding_paper_over_the_text() {
        let mut page = gray_page(80, 80, |_, _| 255);
        for y in 35..45 {
            for x in 35..45 {
                page.data[(y * 80 + x) as usize] = 0;
            }
        }
        let fitted = fitted_over(&page, Rect::new(30, 30, 20, 20));
        let patch = render(&page, &fitted);
        for y in 0..patch.height {
            for x in 0..patch.width {
                let (px, py) = (30 + x as i64, 30 + y as i64);
                if fitted.mask.contains(px, py) {
                    assert_eq!(patch.sample(x, y, 0), 255, "masked pixel was not flat white at {x},{y}");
                }
            }
        }
    }

    #[test]
    fn a_solid_color_fill_paints_chosen_color_in_gray_and_rgb() {
        let gray = gray_page(80, 80, |_, _| 100);
        let fitted = fitted_over(&gray, Rect::new(30, 30, 20, 20));
        // Pure white [255, 255, 255]
        let patch = render_solid_color(&gray, &fitted, [255, 255, 255]).unwrap();
        assert_eq!(patch.sample(10, 10, 0), 255);

        // RGB page
        let rgb_page = fixtures::by_name("rgb8").raster;
        let fitted_rgb = fitted_over(&rgb_page, Rect::new(10, 10, 20, 20));
        let patch_rgb = render_solid_color(&rgb_page, &fitted_rgb, [255, 128, 64]).unwrap();
        assert_eq!(patch_rgb.sample(5, 5, 0), 255);
        assert_eq!(patch_rgb.sample(5, 5, 1), 128);
        assert_eq!(patch_rgb.sample(5, 5, 2), 64);
    }

    #[test]
    fn a_solid_color_fill_preserves_alpha_and_scales_depth() {
        let rgba = fixtures::by_name("rgba8").raster;
        let fitted = fitted_over(&rgba, Rect::new(20, 16, 12, 10));
        let patch = render_solid_color(&rgba, &fitted, [255, 255, 255]).unwrap();
        assert_eq!(patch.sample(5, 5, 0), 255);
        assert_eq!(patch.sample(5, 5, 1), 255);
        assert_eq!(patch.sample(5, 5, 2), 255);
        // Alpha preserved from page
        assert_eq!(patch.sample(5, 5, 3), rgba.sample(25, 21, 3));

        // 16-bit gray page
        let l16 = fixtures::by_name("l16").raster;
        let fitted_16 = fitted_over(&l16, Rect::new(10, 10, 20, 20));
        let patch_16 = render_solid_color(&l16, &fitted_16, [255, 255, 255]).unwrap();
        assert_eq!(patch_16.sample(5, 5, 0), 65535);
    }

    #[test]
    fn a_solid_color_fill_keeps_indexed_and_cmyk_pages_in_their_native_modes() {
        let indexed = fixtures::by_name("indexed-p").raster;
        let indexed_fit = fitted_over(&indexed, Rect::new(20, 16, 12, 10));
        let indexed_patch = render_solid_color(&indexed, &indexed_fit, [250, 20, 30]).unwrap();
        let entries = indexed.palette.as_ref().unwrap().len() / 3;
        assert_eq!(indexed_patch.mode, ColorMode::Indexed);
        assert!((indexed_patch.sample(5, 5, 0) as usize) < entries);

        let cmyk = fixtures::by_name("cmyk8").raster;
        let cmyk_fit = fitted_over(&cmyk, Rect::new(20, 16, 12, 10));
        assert_eq!(render_solid_color(&cmyk, &cmyk_fit, [255, 0, 0]).unwrap().mode, ColorMode::Cmyk);
    }
    #[test]
    fn selected_fill_keeps_opacity_and_native_color_description() {
        let mut page = fixtures::by_name("indexed-p").raster;
        page.palette = Some(vec![255, 0, 0, 0, 0, 0, 255, 255, 255, 128, 128, 128]);
        page.trns = Some(vec![0, 64, 128, 255]);
        page.srgb_intent = Some(1);
        let mut fitted = fitted_over(&page, Rect::new(20, 16, 12, 10));
        fitted.mask.set(24, 20, false);
        let patch = render_solid_color(&page, &fitted, [255, 0, 0]).unwrap();
        assert_eq!(patch.srgb_intent, page.srgb_intent);
        assert!(color_is_approximated(&page, [255, 0, 0]).unwrap());
        for y in 0..patch.height {
            for x in 0..patch.width {
                let px = (fitted.mask.bounds.x + x as i64) as u32;
                let py = (fitted.mask.bounds.y + y as i64) as u32;
                assert_eq!(
                    crate::image::color::alpha(&patch, x, y),
                    crate::image::color::alpha(&page, px, py)
                );
                if !fitted.mask.contains(px as i64, py as i64)
                    || crate::image::color::alpha(&page, px, py) == 0.0
                {
                    assert_eq!(patch.sample(x, y, 0), page.sample(px, py, 0));
                }
            }
        }
        let mut keyed = gray_page(80, 80, |_, _| 100);
        keyed.trns = Some(vec![0, 0]);
        keyed.set_sample(32, 32, 0, 0);
        let fitted = fitted_over(&keyed, Rect::new(30, 30, 10, 10));
        let patch = render_solid_color(&keyed, &fitted, [0, 0, 0]).unwrap();
        for y in 0..patch.height {
            for x in 0..patch.width {
                assert_eq!(
                    crate::image::color::alpha(&patch, x, y),
                    crate::image::color::alpha(
                        &keyed,
                        (fitted.mask.bounds.x + x as i64) as u32,
                        (fitted.mask.bounds.y + y as i64) as u32
                    )
                );
            }
        }
    }
    #[test]
    fn native_fills_preserve_color_key_and_palette_opacity() {
        let mut keyed = gray_page(80, 80, |x, y| {
            if (30..40).contains(&x) && (30..40).contains(&y) {
                100
            } else {
                0
            }
        });
        keyed.trns = Some(vec![0, 0]);
        keyed.set_sample(32, 32, 0, 0);
        let fit = fitted_over(&keyed, Rect::new(30, 30, 10, 10));
        for patch in [render(&keyed, &fit), render_solid_color(&keyed, &fit, [128, 128, 128]).unwrap()] {
            for y in 0..patch.height {
                for x in 0..patch.width {
                    let (px, py) = (
                        (fit.mask.bounds.x + x as i64) as u32,
                        (fit.mask.bounds.y + y as i64) as u32,
                    );
                    assert_eq!(
                        crate::image::color::alpha(&patch, x, y),
                        crate::image::color::alpha(&keyed, px, py)
                    );
                    if keyed.sample(px, py, 0) == 0 {
                        assert_eq!(patch.sample(x, y, 0), 0);
                    }
                }
            }
        }
        let mut indexed = fixtures::by_name("indexed-p").raster;
        indexed.trns = Some(vec![0, 64, 128, 255]);
        let fit = fitted_over(&indexed, Rect::new(20, 16, 12, 10));
        for patch in [render(&indexed, &fit), render_solid_color(&indexed, &fit, [128, 128, 128]).unwrap()] {
            for y in 0..patch.height {
                for x in 0..patch.width {
                    let (px, py) = (
                        (fit.mask.bounds.x + x as i64) as u32,
                        (fit.mask.bounds.y + y as i64) as u32,
                    );
                    assert_eq!(
                        crate::image::color::alpha(&patch, x, y),
                        crate::image::color::alpha(&indexed, px, py)
                    );
                }
            }
        }
    }
    #[test]
    fn native_fills_do_not_change_hidden_transparent_color_samples() {
        let mut page = fixtures::by_name("rgba8").raster;
        for y in 0..page.height {
            for x in 0..page.width {
                page.set_sample(x, y, 3, 0);
            }
        }
        let fit = fitted_over(&page, Rect::new(20, 16, 12, 10));
        for patch in [render(&page, &fit), render_solid_color(&page, &fit, [128, 128, 128]).unwrap()] {
            for y in 0..patch.height {
                for x in 0..patch.width {
                    for c in 0..4 {
                        assert_eq!(
                            patch.sample(x, y, c),
                            page.sample(
                                (fit.mask.bounds.x + x as i64) as u32,
                                (fit.mask.bounds.y + y as i64) as u32,
                                c
                            )
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn selected_fill_transforms_at_sixteen_bits_before_quantization() {
        let mut page = fixtures::by_name("rgb16").raster;
        page.color.gamma = Some(100000);
        let fitted = fitted_over(&page, Rect::new(20, 16, 12, 10));
        let patch = render_solid_color(&page, &fitted, [128, 100, 40]).unwrap();
        for (c, v) in [128.0f64, 100.0, 40.0].iter().enumerate() {
            let expected = (((v / 255.0 + 0.055) / 1.055).powf(2.4) * 65535.0).round() as i32;
            assert!((patch.sample(5, 5, c) as i32 - expected).abs() <= 2);
            assert_ne!(
                patch.sample(5, 5, c) % 257,
                0,
                "premature RGB8 quantization"
            );
        }
    }
    #[test]
    fn untagged_cmyk_selected_fill_is_actionably_refused() {
        let mut page = fixtures::by_name("cmyk8").raster;
        page.icc = None;
        let fitted = fitted_over(&page, Rect::new(20, 16, 12, 10));
        assert!(render_solid_color(&page, &fitted, [128, 128, 128])
            .unwrap_err()
            .to_string()
            .contains("source profile"));
    }
}
