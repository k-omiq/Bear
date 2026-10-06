//! Managed display/edit boundary. Native rasters never become display buffers.
//! Untagged RGB and gray assume the sRGB transfer function. Untagged CMYK uses
//! the explicitly named multiplicative-ink approximation for preview only.
use super::{ColorMode, ImageError, Raster};
use lcms2::{Intent, PixelFormat, Profile, Transform};
use sha2::{Digest, Sha256};
use std::{cell::RefCell, collections::VecDeque, rc::Rc};

pub const PIPELINE_VERSION: u8 = 1;
const CACHE_LIMIT: usize = 16;
enum Forward {
    Srgb,
    Gray(Transform<f32, [f32; 3]>),
    Rgb(Transform<[f32; 3], [f32; 3]>),
    Cmyk(Transform<[f32; 4], [f32; 3]>),
    AssumedCmyk,
}
enum Reverse {
    Srgb,
    Gray(Transform<[f32; 3], f32>),
    Rgb(Transform<[f32; 3], [f32; 3]>),
    Cmyk(Transform<[f32; 3], [f32; 4]>),
    Unsupported,
}
pub struct ManagedColor {
    forward: Forward,
    reverse: Reverse,
}
thread_local! {
    static CACHE: RefCell<VecDeque<([u8;32], Rc<ManagedColor>)>> = const { RefCell::new(VecDeque::new()) };
}
fn err(e: impl std::fmt::Display) -> ImageError {
    ImageError::Foreign(format!("color interpretation: {e}"))
}
pub fn linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}
pub fn encoded(v: f32) -> f32 {
    if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}
pub fn byte(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Native fitted edits currently operate on straight samples. Associated TIFF
/// can be displayed and exported natively, but must not enter those operations.
pub fn validate_editing(page: &Raster) -> Result<(), ImageError> {
    page.color.validate_interpretation()?;
    if page.color.associated_alpha {
        return Err(err("associated-alpha TIFF editing requires association-aware native fitting; retain the original TIFF"));
    }
    Ok(())
}

impl ManagedColor {
    pub fn for_raster(page: &Raster) -> Result<Rc<Self>, ImageError> {
        page.color.validate_interpretation()?;
        if page.color.associated_alpha && page.mode.alpha_channel().is_none() {
            return Err(err("associated alpha requires a native alpha channel"));
        }
        let mut hash = Sha256::new();
        hash.update([
            PIPELINE_VERSION,
            page.mode as u8,
            page.depth.bits(),
            page.srgb_intent.unwrap_or(255),
        ]);
        hash.update(page.icc.as_deref().unwrap_or_default());
        hash.update(format!(
            "{:?}:{:?}:{:?}",
            page.color.gamma, page.color.chromaticities, page.color.cicp
        ));
        let key: [u8; 32] = hash.finalize().into();
        CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            if let Some(at) = cache.iter().position(|(k, _)| *k == key) {
                let entry = cache.remove(at).unwrap();
                let result = entry.1.clone();
                cache.push_front(entry);
                return Ok(result);
            }
            let result = Rc::new(Self::build(page)?);
            cache.push_front((key, result.clone()));
            cache.truncate(CACHE_LIMIT);
            Ok(result)
        })
    }
    fn build(page: &Raster) -> Result<Self, ImageError> {
        if let Some([_, transfer, _, _]) = page.color.cicp {
            if !matches!(transfer, 1 | 13 | 14 | 15) {
                return Err(err(format!("unsupported cICP transfer {transfer}; HDR preview/painting requires tested tone mapping")));
            }
        }
        super::metadata::validate_profile(page.icc.as_deref(), page.mode)?;
        let icc = super::metadata::equivalent_icc(
            &page.color,
            page.icc.as_deref(),
            page.srgb_intent,
            page.mode,
        )?;
        let Some(icc) = icc else {
            return Ok(if page.mode == ColorMode::Cmyk {
                Self {
                    forward: Forward::AssumedCmyk,
                    reverse: Reverse::Unsupported,
                }
            } else {
                Self {
                    forward: Forward::Srgb,
                    reverse: Reverse::Srgb,
                }
            });
        };
        let src = Profile::new_icc(&icc).map_err(err)?;
        let dst = Profile::new_srgb();
        let intent = if page.icc.is_some() && page.color.cicp.is_none() {
            src.header_rendering_intent()
        } else {
            match page.srgb_intent {
                Some(0) => Intent::Perceptual,
                Some(2) => Intent::Saturation,
                Some(3) => Intent::AbsoluteColorimetric,
                _ => Intent::RelativeColorimetric,
            }
        };
        let (forward, reverse) = match page.mode {
            ColorMode::Gray | ColorMode::GrayAlpha => (
                Forward::Gray(
                    Transform::new(
                        &src,
                        PixelFormat::GRAY_FLT,
                        &dst,
                        PixelFormat::RGB_FLT,
                        intent,
                    )
                    .map_err(err)?,
                ),
                Reverse::Gray(
                    Transform::new(
                        &dst,
                        PixelFormat::RGB_FLT,
                        &src,
                        PixelFormat::GRAY_FLT,
                        intent,
                    )
                    .map_err(err)?,
                ),
            ),
            ColorMode::Cmyk => (
                Forward::Cmyk(
                    Transform::new(
                        &src,
                        PixelFormat::CMYK_FLT,
                        &dst,
                        PixelFormat::RGB_FLT,
                        intent,
                    )
                    .map_err(err)?,
                ),
                match Transform::new(
                    &dst,
                    PixelFormat::RGB_FLT,
                    &src,
                    PixelFormat::CMYK_FLT,
                    intent,
                ) {
                    Ok(t) => Reverse::Cmyk(t),
                    Err(_) => Reverse::Unsupported,
                },
            ),
            _ => (
                Forward::Rgb(
                    Transform::new(
                        &src,
                        PixelFormat::RGB_FLT,
                        &dst,
                        PixelFormat::RGB_FLT,
                        intent,
                    )
                    .map_err(err)?,
                ),
                Reverse::Rgb(
                    Transform::new(
                        &dst,
                        PixelFormat::RGB_FLT,
                        &src,
                        PixelFormat::RGB_FLT,
                        intent,
                    )
                    .map_err(err)?,
                ),
            ),
        };
        Ok(Self { forward, reverse })
    }
    /// Normalized native channels -> encoded sRGB, retaining source precision.
    pub fn to_srgb(&self, v: [f32; 4]) -> [f32; 3] {
        let mut out = [[0.0; 3]];
        match &self.forward {
            Forward::Srgb => out[0] = [v[0], v[1], v[2]],
            Forward::Gray(t) => t.transform_pixels(&[v[0]], &mut out),
            Forward::Rgb(t) => t.transform_pixels(&[[v[0], v[1], v[2]]], &mut out),
            // LCMS floating CMYK uses percentage units, unlike RGB.
            Forward::Cmyk(t) => t.transform_pixels(&[v.map(|c| c * 100.0)], &mut out),
            Forward::AssumedCmyk => {
                out[0] = [
                    (1.0 - v[0]) * (1.0 - v[3]),
                    (1.0 - v[1]) * (1.0 - v[3]),
                    (1.0 - v[2]) * (1.0 - v[3]),
                ]
            }
        }
        out[0]
    }
    pub fn pixel(&self, page: &Raster, x: u32, y: u32) -> [f32; 3] {
        self.to_srgb(native_pixel(page, x, y))
    }
    pub fn from_srgb(&self, rgb: [f32; 3], mode: ColorMode) -> Result<[f32; 4], ImageError> {
        let mut out = [0.0; 4];
        match &self.reverse {
            Reverse::Srgb => {
                out[..3].copy_from_slice(&rgb);
                if matches!(mode, ColorMode::Gray | ColorMode::GrayAlpha) {
                    out[0] = encoded(0.2126*linear(rgb[0]) + 0.7152*linear(rgb[1]) + 0.0722*linear(rgb[2]));
                }
            }
            Reverse::Gray(t) => { let mut value = [0.0]; t.transform_pixels(&[rgb], &mut value); out[0]=value[0]; }
            Reverse::Rgb(t) => { let mut value = [[0.0;3]]; t.transform_pixels(&[rgb], &mut value); out[..3].copy_from_slice(&value[0]); }
            Reverse::Cmyk(t) => { let mut value = [[0.0;4]]; t.transform_pixels(&[rgb], &mut value); out = value[0].map(|c| c/100.0); }
            Reverse::Unsupported => return Err(err("selected colors require an invertible source profile; untagged CMYK supports preview and native fills only")),
        }
        Ok(out)
    }
}

pub fn native_pixel(page: &Raster, x: u32, y: u32) -> [f32; 4] {
    let max = ((1u32 << page.depth.bits()) - 1) as f32;
    let coverage = if page.color.associated_alpha {
        alpha(page, x, y)
    } else {
        1.0
    };
    // TIFF association is in the stored sample encoding. Unassociate before
    // interpreting the source profile, without modifying the native raster.
    let s = |c| {
        if coverage > 0.0 {
            (page.sample(x, y, c) as f32 / max / coverage).clamp(0.0, 1.0)
        } else {
            0.0
        }
    };
    match page.mode {
        ColorMode::Gray | ColorMode::GrayAlpha => [s(0), s(0), s(0), 0.0],
        ColorMode::Rgb | ColorMode::Rgba => [s(0), s(1), s(2), 0.0],
        ColorMode::Cmyk => [s(0), s(1), s(2), s(3)],
        ColorMode::Indexed => {
            let i = page.sample(x, y, 0) as usize * 3;
            let p = page.palette.as_deref().unwrap_or_default();
            [
                p.get(i).copied().unwrap_or(0) as f32 / 255.0,
                p.get(i + 1).copied().unwrap_or(0) as f32 / 255.0,
                p.get(i + 2).copied().unwrap_or(0) as f32 / 255.0,
                0.0,
            ]
        }
    }
}
/// PNG color-key matching is performed before reducing to display depth.
pub fn alpha(page: &Raster, x: u32, y: u32) -> f32 {
    if let Some(c) = page.mode.alpha_channel() {
        return page.sample(x, y, c) as f32 / ((1u32 << page.depth.bits()) - 1) as f32;
    }
    let Some(t) = &page.trns else {
        return 1.0;
    };
    match page.mode {
        ColorMode::Indexed => {
            t.get(page.sample(x, y, 0) as usize).copied().unwrap_or(255) as f32 / 255.0
        }
        ColorMode::Gray if t.len() == 2 => {
            if page.sample(x, y, 0) == u16::from_be_bytes([t[0], t[1]]) {
                0.0
            } else {
                1.0
            }
        }
        ColorMode::Rgb
            if t.len() == 6
                && (0..3)
                    .all(|c| page.sample(x, y, c) == u16::from_be_bytes([t[c * 2], t[c * 2 + 1]])) =>
        {
            0.0
        }
        _ => 1.0,
    }
}

/// An opaque edit cannot use the tuple reserved by PNG tRNS. Choose the
/// adjacent native code (one native quantum) without changing the key metadata.
/// Call only for pixels whose original coverage is nonzero.
pub fn avoid_transparent_key(page: &mut Raster, x: u32, y: u32) {
    if !matches!(page.mode, ColorMode::Gray | ColorMode::Rgb)
        || page.trns.is_none()
        || alpha(page, x, y) != 0.0
    {
        return;
    }
    let channel = page.mode.samples() - 1;
    let value = page.sample(x, y, channel);
    let max = ((1u32 << page.depth.bits()) - 1) as u16;
    page.set_sample(
        x,
        y,
        channel,
        if value < max { value + 1 } else { value - 1 },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{decode, fixtures, proxy, BitDepth};
    const ROOT: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/color-reference/"
    );
    fn fixture(name: &str) -> Raster {
        decode(&std::fs::read(format!("{ROOT}{name}")).unwrap()).unwrap()
    }
    #[test]
    fn associated_tiff_preview_unassociates_without_mutating_native_samples() {
        let mut page = fixtures::by_name("rgba8").raster;
        page.width = 1;
        page.height = 1;
        page.data = vec![64, 32, 16, 128];
        page.color.associated_alpha = true;
        let original = page.data.clone();
        let shown = proxy::display(&page).unwrap();
        assert_eq!(shown.data, [128, 64, 32, 128]);
        assert!(!shown.color.associated_alpha);
        assert_eq!(page.data, original);
        assert!(validate_editing(&page)
            .unwrap_err()
            .to_string()
            .contains("associated-alpha"));
        let managed = ManagedColor::for_raster(&page).unwrap();
        let mut patch = page.clone();
        assert!(
            crate::engines::model::commit_srgb(&managed, &mut patch, 0, 0, [1.0; 3], 1.0, 0.0)
                .is_err()
        );
        assert_eq!(patch.data, original);
        page.mode = ColorMode::GrayAlpha;
        page.data = vec![64, 128];
        assert_eq!(proxy::display(&page).unwrap().data, [128, 128, 128, 128]);
        page.data = vec![64, 0];
        assert_eq!(proxy::display(&page).unwrap().data, [0, 0, 0, 0]);
    }

    #[test]
    fn unsupported_tiff_interpretation_is_refused_before_transform_cache_use() {
        let mut page = fixtures::by_name("rgb8").raster;
        ManagedColor::for_raster(&page).unwrap();
        page.color.unsupported_tiff_color = vec![301];
        assert!(ManagedColor::for_raster(&page).is_err());
        assert!(validate_editing(&page).is_err());
    }

    #[test]
    fn independent_linear_rgb_reference_matches_srgb_transfer_within_one_code() {
        let source = fixture("gamma-chrm.png");
        let shown = proxy::display(&source).unwrap();
        for y in 0..source.height {
            for x in 0..source.width {
                for c in 0..3 {
                    // IEC sRGB equation, independent of the LCMS transform under test.
                    let v = source.sample(x, y, c) as f64 / 255.0;
                    let expected = if v <= 0.0031308 {
                        12.92 * v
                    } else {
                        1.055 * v.powf(1.0 / 2.4) - 0.055
                    };
                    assert!(
                        (shown.sample(x, y, c) as i32 - (expected * 255.0).round() as i32).abs()
                            <= 1
                    );
                }
            }
        }
        assert_eq!(shown.srgb_intent, Some(1));
        assert!(shown.icc.is_none());
    }
    #[test]
    fn cmyk_profile_transform_matches_independently_decoded_reference_within_one_code() {
        let mut source = Raster {
            width: 16,
            height: 8,
            mode: ColorMode::Cmyk,
            depth: BitDepth::Eight,
            icc: Some(fixtures::cmyk_profile()),
            palette: None,
            trns: None,
            srgb_intent: None,
            color: Default::default(),
            data: std::fs::read(format!("{ROOT}cmyk-adobe.jpg.cmyk")).unwrap(),
        };
        let reference = std::fs::read(format!("{ROOT}cmyk-adobe.srgb")).unwrap();
        let shown = proxy::display(&source).unwrap();
        for (actual, expected) in shown.data.iter().zip(&reference) {
            assert!(
                (*actual as i16 - *expected as i16).abs() <= 1,
                "{actual} vs {expected}"
            );
        }
        source.icc = Some(fixtures::srgb_profile());
        assert!(
            proxy::display(&source).is_err(),
            "CMYK numbers must never use an RGB ICC"
        );
    }
    #[test]
    fn full_precision_color_keys_and_partial_palette_alpha_are_preserved() {
        for name in ["gray16-key.png", "rgb16-key.png", "indexed-alpha.png"] {
            let source = fixture(name);
            let shown = proxy::display(&source).unwrap();
            assert_eq!(shown.mode, ColorMode::Rgba);
            for x in 0..source.width {
                assert_eq!(shown.sample(x, 0, 3), byte(alpha(&source, x, 0)) as u16);
            }
            if source.depth == BitDepth::Sixteen {
                assert_eq!(shown.sample(0, 0, 3), 0, "{name}");
                assert_eq!(shown.sample(1, 0, 3), 255, "{name}");
            } else {
                assert_eq!(
                    (0..4).map(|x| shown.sample(x, 0, 3)).collect::<Vec<_>>(),
                    [0, 64, 128, 255]
                );
            }
        }
    }
    #[test]
    fn tagged_gray_matches_independent_gamma_reference() {
        let mut source = fixtures::by_name("l8").raster;
        source.icc = Some(fixtures::gray_profile());
        let shown = proxy::display(&source).unwrap();
        for y in 0..source.height {
            for x in 0..source.width {
                let light = (source.sample(x, y, 0) as f64 / 255.0).powf(2.2);
                let expected = if light <= 0.0031308 {
                    12.92 * light
                } else {
                    1.055 * light.powf(1.0 / 2.4) - 0.055
                };
                for c in 0..3 {
                    assert!(
                        (shown.sample(x, y, c) as i32 - (expected * 255.0).round() as i32).abs()
                            <= 1
                    );
                }
            }
        }
    }
    #[test]
    fn cicp_is_authoritative_over_legacy_rgb_profile_and_srgb_over_chromaticities() {
        let mut source = fixtures::by_name("rgb8").raster;
        source.color.cicp = Some([12, 13, 0, 1]);
        let expected = proxy::display(&source).unwrap();
        source.icc = Some(fixtures::srgb_profile());
        assert_eq!(proxy::display(&source).unwrap().data, expected.data);
        source.color.cicp = None;
        source.icc = None;
        source.srgb_intent = Some(1);
        let expected = proxy::display(&source).unwrap();
        source.color.gamma = Some(100000);
        source.color.chromaticities = Some([31270, 32900, 68000, 32000, 26500, 69000, 15000, 6000]);
        assert_eq!(proxy::display(&source).unwrap().data, expected.data);
    }
    #[test]
    fn hdr_is_refused_even_when_a_legacy_icc_is_present() {
        let mut source = fixtures::by_name("rgb8-icc").raster;
        source.color.cicp = Some([9, 16, 0, 1]);
        assert!(proxy::display(&source)
            .unwrap_err()
            .to_string()
            .contains("HDR"));
    }
    #[test]
    fn cache_is_bounded_and_reuses_identical_interpretations() {
        let mut source = fixtures::by_name("rgb8").raster;
        let a = ManagedColor::for_raster(&source).unwrap();
        assert!(Rc::ptr_eq(&a, &ManagedColor::for_raster(&source).unwrap()));
        for gamma in 40000..40040 {
            source.color.gamma = Some(gamma);
            ManagedColor::for_raster(&source).unwrap();
        }
        CACHE.with(|c| assert_eq!(c.borrow().len(), CACHE_LIMIT));
    }
}
