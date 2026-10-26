//! **Grey pages written as grayscale**, when the export asks for it.
//!
//! Many grey scans arrive as RGB or RGBA: three channels holding one value.
//! By default an export keeps the page's own colour type, as import does, so
//! such a page leaves as RGB. [`Tone::Grayscale`] writes it as grayscale
//! instead: gray for RGB, gray with alpha for RGBA. A page with any colour in
//! it, an indexed or CMYK page, and a page already gray are written as they
//! are.
//!
//! Grey is decided on the cleaned page with the tolerance
//! [`Raster::is_neutral`] uses: every pixel's channels within 1.5 of 255
//! levels. A colour hand edit on a grey page therefore keeps it colour. Each
//! gray sample is the mean of the three.
//!
//! A page described by its transfer alone (untagged, `sRGB`, or `gAMA` with a
//! D65 or absent `cHRM`) keeps that description: on the neutral axis the
//! transfer is the whole of it, so the page moves by at most one level. A page
//! with an ICC profile or `cICP`, or other primaries, is read through its
//! description into sRGB and written as sRGB gray, because an RGB profile
//! cannot describe a gray image; the plan declares that ([`declarations`]).

use crate::image::color::ManagedColor;
use crate::image::{ColorDescription, ColorMode, Header, Raster};
use crate::mask::Mask;

use super::Declaration;

/// What colour type an export writes a page in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tone {
    /// The page's own colour type, whatever it holds.
    #[default]
    Source,
    /// A grey RGB or RGBA page is written as grayscale. Colour stays colour.
    Grayscale,
}

impl Tone {
    /// Whether this tone could change a page of `mode`: only an RGB or RGBA
    /// page ever becomes grayscale.
    pub fn may_convert(self, mode: ColorMode) -> bool {
        self == Tone::Grayscale && matches!(mode, ColorMode::Rgb | ColorMode::Rgba)
    }

    /// `raster` as this tone writes it: the grayscale page when it is grey,
    /// `None` when it is written as it is.
    pub fn apply(self, raster: &Raster) -> Option<Raster> {
        if self.may_convert(raster.mode) { grayscale(raster, None) } else { None }
    }

    /// [`Tone::apply`] for a region layer, grey wherever `mask` is set. The
    /// pixels outside it are hidden by the layer mask, so a model's tint
    /// there does not keep the document colour; they take the mean too.
    pub fn apply_within(self, raster: &Raster, mask: &Mask) -> Option<Raster> {
        if self.may_convert(raster.mode) { grayscale(raster, Some(mask)) } else { None }
    }
}

/// What the plan says a grayscale export may change about a page with this
/// header, before any pixel is read: the page is only converted if it turns
/// out to be grey, so each sentence says so.
pub fn declarations(header: &Header) -> Vec<Declaration> {
    if !Tone::Grayscale.may_convert(header.mode) {
        return Vec::new();
    }
    let mut declared = Vec::new();
    let reason = |what: &str| format!("if the page is gray, {what}");
    if header.icc.is_some() {
        declared.push(Declaration::MetadataNormalized { field: "ICC".into(), reason: reason("it is converted through the profile and written as sRGB gray") });
    }
    if header.color.cicp.is_some() {
        declared.push(Declaration::MetadataNormalized { field: "cICP".into(), reason: reason("it is converted and written as sRGB gray") });
    }
    if header.icc.is_none() && header.color.cicp.is_none() && !d65_or_none(header.color.chromaticities) {
        declared.push(Declaration::MetadataNormalized { field: "cHRM".into(), reason: reason("it is converted and written as sRGB gray") });
    }
    for (present, field) in [
        (header.color.significant_bits.is_some(), "sBIT"),
        (header.color.content_light.is_some(), "cLLI"),
        (header.color.mastering_display.is_some(), "mDCV"),
    ] {
        if present {
            declared.push(Declaration::MetadataNormalized { field: field.into(), reason: reason("it is omitted from the grayscale file") });
        }
    }
    declared
}

/// `cHRM` that leaves the neutral axis where sRGB has it: absent, or a D65
/// white point. Gray has no primaries, so the white is all that matters.
fn d65_or_none(chromaticities: Option<[u32; 8]>) -> bool {
    chromaticities.is_none_or(|c| c[0].abs_diff(31270) <= 50 && c[1].abs_diff(32900) <= 50)
}

/// `raster` as grayscale when every pixel (every pixel inside `within`, when
/// given) is grey, else `None`.
///
/// `None` too for a page whose colour cannot be read faithfully: associated
/// alpha, a `tRNS` colour key, or a description the colour pipeline refuses.
/// Such a page is written in its own mode.
pub fn grayscale(raster: &Raster, within: Option<&Mask>) -> Option<Raster> {
    let mode = match raster.mode {
        ColorMode::Rgb => ColorMode::Gray,
        ColorMode::Rgba => ColorMode::GrayAlpha,
        _ => return None,
    };
    if raster.color.associated_alpha || raster.trns.is_some() {
        return None;
    }
    let top = ((1u32 << raster.depth.bits()) - 1) as f32;
    let tolerance = 1.5 / 255.0;
    let transfer_only = raster.icc.is_none() && raster.color.cicp.is_none() && d65_or_none(raster.color.chromaticities);
    // The transfer-only page is read as stored; any other is read into sRGB.
    let managed = if transfer_only { None } else { Some(ManagedColor::for_raster(raster).ok()?) };
    let read = |x: u32, y: u32| -> [f32; 3] {
        match &managed {
            Some(managed) => managed.pixel(raster, x, y),
            None => [0, 1, 2].map(|c| raster.sample(x, y, c) as f32 / top),
        }
    };
    let mut gray = Raster {
        width: raster.width,
        height: raster.height,
        mode,
        depth: raster.depth,
        icc: None,
        palette: None,
        trns: None,
        srgb_intent: if transfer_only { raster.srgb_intent } else { Some(raster.srgb_intent.unwrap_or(0)) },
        color: ColorDescription {
            gamma: if transfer_only { raster.color.gamma } else { None },
            safe_ancillary: raster.color.safe_ancillary.clone(),
            ..ColorDescription::default()
        },
        data: Vec::new(),
    };
    gray.data = vec![0; gray.stride() * gray.height as usize];
    for y in 0..raster.height {
        for x in 0..raster.width {
            let [r, g, b] = read(x, y);
            let checked = within.is_none_or(|mask| mask.contains(i64::from(x) + mask.bounds.x, i64::from(y) + mask.bounds.y));
            if checked && ((r - g).abs() >= tolerance || (g - b).abs() >= tolerance || (r - b).abs() >= tolerance) {
                return None;
            }
            let value = ((r + g + b) / 3.0).clamp(0.0, 1.0);
            gray.set_sample(x, y, 0, (value * top).round() as u16);
            if mode == ColorMode::GrayAlpha {
                gray.set_sample(x, y, 1, raster.sample(x, y, 3));
            }
        }
    }
    Some(gray)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{BitDepth, fixtures};

    fn page(mode: ColorMode, depth: BitDepth, pixel: impl Fn(u32, u32) -> [u16; 4]) -> Raster {
        let mut raster = Raster {
            width: 6,
            height: 4,
            mode,
            depth,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None,
            color: Default::default(),
            data: Vec::new(),
        };
        raster.data = vec![0; raster.stride() * raster.height as usize];
        for y in 0..raster.height {
            for x in 0..raster.width {
                for c in 0..mode.samples() {
                    raster.set_sample(x, y, c, pixel(x, y)[c]);
                }
            }
        }
        raster
    }

    #[test]
    fn a_grey_rgb_page_becomes_gray_with_the_same_values() {
        let source = page(ColorMode::Rgb, BitDepth::Eight, |x, y| {
            let v = (x * 40 + y * 3) as u16;
            [v, v, v, 0]
        });
        let gray = Tone::Grayscale.apply(&source).unwrap();
        assert_eq!((gray.mode, gray.depth), (ColorMode::Gray, BitDepth::Eight));
        for y in 0..4 {
            for x in 0..6 {
                assert_eq!(gray.sample(x, y, 0), source.sample(x, y, 0));
            }
        }
        assert_eq!(gray.srgb_intent, None, "an undescribed page stays undescribed");
    }

    #[test]
    fn rgba_keeps_its_alpha_and_sixteen_bits_keep_their_precision() {
        let source = page(ColorMode::Rgba, BitDepth::Sixteen, |x, _| {
            let v = 1000 + x as u16 * 9000;
            [v, v + 50, v, 300 * x as u16]
        });
        let gray = Tone::Grayscale.apply(&source).unwrap();
        assert_eq!((gray.mode, gray.depth), (ColorMode::GrayAlpha, BitDepth::Sixteen));
        for x in 0..6 {
            assert!((gray.sample(x, 0, 0) as i32 - source.sample(x, 0, 0) as i32 - 17).abs() <= 1);
            assert_eq!(gray.sample(x, 0, 1), source.sample(x, 0, 3));
        }
    }

    #[test]
    fn one_coloured_pixel_keeps_the_page_colour() {
        let source = page(ColorMode::Rgb, BitDepth::Eight, |x, y| if (x, y) == (5, 3) { [120, 118, 125, 0] } else { [90, 90, 90, 0] });
        assert!(Tone::Grayscale.apply(&source).is_none());
        // One level apart is still grey, as `is_neutral` reads it.
        let near = page(ColorMode::Rgb, BitDepth::Eight, |_, _| [90, 91, 90, 0]);
        assert!(near.is_neutral() && Tone::Grayscale.apply(&near).is_some());
    }

    #[test]
    fn source_tone_and_other_modes_are_never_converted() {
        let grey = page(ColorMode::Rgb, BitDepth::Eight, |_, _| [90, 90, 90, 0]);
        assert!(Tone::Source.apply(&grey).is_none());
        for name in ["l8", "la16", "indexed-p", "cmyk8"] {
            assert!(Tone::Grayscale.apply(&fixtures::by_name(name).raster).is_none(), "{name}");
        }
        let mut keyed = grey.clone();
        keyed.trns = Some(vec![0, 90, 0, 90, 0, 90]);
        assert!(grayscale(&keyed, None).is_none(), "a colour key has no gray equivalent here");
    }

    fn header_of(page: &Raster) -> Header {
        Header {
            palette: None,
            width: page.width,
            height: page.height,
            mode: page.mode,
            depth: page.depth,
            icc: page.icc.clone(),
            color: page.color.clone(),
            srgb_intent: page.srgb_intent,
            trns: page.trns.clone(),
        }
    }

    #[test]
    fn a_gamma_page_keeps_its_gamma_and_its_values() {
        let mut source = page(ColorMode::Rgb, BitDepth::Eight, |x, _| {
            let v = 16 + x as u16 * 40;
            [v, v, v, 0]
        });
        source.color.gamma = Some(45455);
        source.color.chromaticities = Some([31270, 32900, 64000, 33000, 30000, 60000, 15000, 6000]);
        let gray = grayscale(&source, None).unwrap();
        assert_eq!((gray.color.gamma, gray.srgb_intent, gray.color.chromaticities), (Some(45455), None, None));
        for x in 0..6 {
            assert_eq!(gray.sample(x, 0, 0), source.sample(x, 0, 0), "the shadows stay where they were");
        }
        assert!(declarations(&header_of(&source)).is_empty(), "nothing about the description changes");
    }

    #[test]
    fn a_region_is_grey_where_its_mask_is_set() {
        let tinted = page(ColorMode::Rgb, BitDepth::Eight, |x, _| if x == 0 { [200, 120, 120, 0] } else { [90, 90, 90, 0] });
        let mut mask = Mask::empty(crate::mask::Rect::new(40, 50, 6, 4));
        for y in 0..4 {
            for x in 1..6 {
                mask.set(40 + x, 50 + y, true);
            }
        }
        assert!(grayscale(&tinted, None).is_none());
        let gray = Tone::Grayscale.apply_within(&tinted, &mask).unwrap();
        assert_eq!(gray.sample(3, 2, 0), 90);
        mask.set(40, 50, true);
        assert!(Tone::Grayscale.apply_within(&tinted, &mask).is_none(), "a tint under the mask keeps it colour");
    }

    #[test]
    fn a_profiled_page_is_read_through_its_profile_and_described_as_srgb() {
        let mut source = page(ColorMode::Rgb, BitDepth::Eight, |x, _| {
            let v = x as u16 * 50;
            [v, v, v, 0]
        });
        source.icc = Some(fixtures::srgb_profile());
        let gray = grayscale(&source, None).unwrap();
        assert_eq!(gray.srgb_intent, Some(0));
        assert_eq!((gray.color.gamma, gray.icc.as_ref()), (None, None));
        for x in 0..6 {
            assert!((gray.sample(x, 0, 0) as i32 - source.sample(x, 0, 0) as i32).abs() <= 1, "an sRGB profile moves nothing");
        }
        assert!(declarations(&header_of(&source)).iter().any(|d| matches!(d, Declaration::MetadataNormalized { field, .. } if field == "ICC")));
    }
}
