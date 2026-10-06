//! The colour-mode fixtures the fidelity contract is tested against.
//!
//! They are built here rather than committed as files so the test is
//! legible: what makes a fixture interesting - the free palette slot,
//! the 16-bit values that do not survive an 8-bit round-trip, the ICC profile -
//! is visible in the code that constructs it.
//!
//! Every fixture carries the same picture: a horizontal ramp, a vertical ramp,
//! and a dark rectangle standing in for a text block. The ramps are what catch a
//! silent bit-depth reduction; the rectangle is what a synthetic patch is
//! applied over in the mask-subset half of the test.
//!
//! This set was once called "the nine colour-mode fixtures", then listed at
//! eleven. Eleven is right, and there are twelve here: bitonal is added
//! because the model rungs and the quality metric have bitonal rules that
//! nothing else would exercise.

use std::sync::OnceLock;

use super::{BitDepth, ColorMode, Raster};

pub struct Fixture {
    pub name: &'static str,
    pub raster: Raster,
}

const W: u32 = 64;
const H: u32 = 48;

/// A real sRGB profile, built by Little CMS rather than hand-rolled, so the
/// fixture's bytes are a profile a reader can parse and not merely bytes that
/// survive being copied.
///
/// **Built once per process**, which is the only thing here that needed saying
/// twice. Every other function in this module is a pure function of `W`, `H`
/// and a channel index, so two calls return the same bytes; an ICC profile is
/// not, because `lcms2` stamps the *creation date-time* into bytes 24..36 as it
/// serialises. Two calls either side of a second boundary therefore differed in
/// one byte - enough to fail an equality that is about the profile and not about
/// the clock, and enough for `all()`'s three ICC fixtures to disagree with each
/// other about the profile they are all supposed to carry.
pub fn srgb_profile() -> Vec<u8> {
    static PROFILE: OnceLock<Vec<u8>> = OnceLock::new();
    PROFILE
        .get_or_init(|| {
            lcms2::Profile::new_srgb().icc().expect("lcms2 can serialise its own sRGB profile")
        })
        .clone()
}

/// A grayscale profile, so the 16-bit gray path is not tested with an RGB tag.
/// Built once per process for the reason [`srgb_profile`] gives.
pub fn gray_profile() -> Vec<u8> {
    static PROFILE: OnceLock<Vec<u8>> = OnceLock::new();
    PROFILE
        .get_or_init(|| {
            let curve = lcms2::ToneCurve::new(2.2);
            lcms2::Profile::new_gray(&lcms2::CIExyY { x: 0.3127, y: 0.3290, Y: 1.0 }, &curve)
                .expect("lcms2 can build a gray profile")
                .icc()
                .expect("lcms2 can serialise it")
        })
        .clone()
}

/// Synthetic, bidirectional CMYK output profile generated independently with LCMS.
/// Its declared ink model is RGB=(1-CMY)*(1-K); it is a test/reference assumption,
/// not a scanner calibration. Fixture sources and license are checked in.
pub fn cmyk_profile() -> Vec<u8> {
    include_bytes!("../../tests/fixtures/color-reference/synthetic-cmyk.icc").to_vec()
}

/// The historic invalid RGB-profile/CMYK-samples fixture, for negative tests.
pub fn mismatched_cmyk() -> Raster {
    let mut raster = by_name("cmyk8").raster;
    raster.icc = Some(srgb_profile());
    raster
}

/// `value` at (x, y) for a single channel, in 0..=255.
fn sample(x: u32, y: u32, channel: usize) -> u8 {
    // A dark rectangle in the middle third: the stand-in for a text block.
    if (W / 3..2 * W / 3).contains(&x) && (H / 3..2 * H / 3).contains(&y) {
        return 16 + channel as u8 * 8;
    }
    let horizontal = (x * 255 / (W - 1)) as u8;
    let vertical = (y * 255 / (H - 1)) as u8;
    match channel {
        0 => horizontal,
        1 => vertical,
        2 => horizontal.wrapping_add(vertical),
        _ => 255,
    }
}

/// The same picture at 16 bits, with the low byte carrying detail that an
/// 8-bit round-trip would erase.
fn sample16(x: u32, y: u32, channel: usize) -> u16 {
    let high = sample(x, y, channel) as u16;
    (high << 8) | ((x as u16 * 7 + y as u16 * 13 + channel as u16 * 3) & 0xff)
}

fn eight_bit(mode: ColorMode) -> Vec<u8> {
    let mut data = Vec::with_capacity((W * H) as usize * mode.samples());
    for y in 0..H {
        for x in 0..W {
            for channel in 0..mode.samples() {
                data.push(sample(x, y, channel));
            }
        }
    }
    data
}

fn sixteen_bit(mode: ColorMode) -> Vec<u8> {
    let mut data = Vec::with_capacity((W * H) as usize * mode.samples() * 2);
    for y in 0..H {
        for x in 0..W {
            for channel in 0..mode.samples() {
                data.extend_from_slice(&sample16(x, y, channel).to_be_bytes());
            }
        }
    }
    data
}

fn base(mode: ColorMode, depth: BitDepth, data: Vec<u8>) -> Raster {
    Raster { width: W, height: H, mode, depth, icc: None, palette: None, trns: None, srgb_intent: None, color: Default::default(), data }
}

/// Indexed, with **a free palette slot**. §4 requires it: the fill's median
/// tone is added to the palette when there is room, and only promotes the image
/// when there is not, so a fixture with a full palette would test the wrong
/// branch.
fn indexed() -> Raster {
    // 16 used entries plus one spare, at 8-bit depth so the index is a byte.
    let used = 16u8;
    let mut palette = Vec::with_capacity(17 * 3);
    for i in 0..=used {
        let level = (i as u32 * 255 / used as u32) as u8;
        palette.extend_from_slice(&[level, level.wrapping_add(20), level.wrapping_sub(20)]);
    }
    let mut data = Vec::with_capacity((W * H) as usize);
    for y in 0..H {
        for x in 0..W {
            // Never emits the last index, which is therefore free.
            data.push(sample(x, y, 0) % used);
        }
    }
    Raster {
        width: W,
        height: H,
        mode: ColorMode::Indexed,
        depth: BitDepth::Eight,
        icc: None,
        palette: Some(palette),
        trns: None,
        srgb_intent: None,
        color: Default::default(),
        data,
    }
}

/// One bit per pixel, packed. The rows pad to a byte boundary, which is the
/// part a naive `data.len() == width * height` assumption gets wrong.
fn bitonal() -> Raster {
    let stride = (W as usize).div_ceil(8);
    let mut data = vec![0u8; stride * H as usize];
    for y in 0..H {
        for x in 0..W {
            if sample(x, y, 0) > 127 {
                data[y as usize * stride + (x as usize / 8)] |= 0x80 >> (x % 8);
            }
        }
    }
    Raster {
        width: W,
        height: H,
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

pub fn all() -> Vec<Fixture> {
    let with_icc = |mut raster: Raster, icc: Vec<u8>| {
        raster.icc = Some(icc);
        raster
    };

    vec![
        Fixture { name: "l8", raster: base(ColorMode::Gray, BitDepth::Eight, eight_bit(ColorMode::Gray)) },
        Fixture { name: "la8", raster: base(ColorMode::GrayAlpha, BitDepth::Eight, eight_bit(ColorMode::GrayAlpha)) },
        Fixture { name: "rgb8", raster: base(ColorMode::Rgb, BitDepth::Eight, eight_bit(ColorMode::Rgb)) },
        Fixture { name: "rgba8", raster: base(ColorMode::Rgba, BitDepth::Eight, eight_bit(ColorMode::Rgba)) },
        Fixture { name: "l16", raster: base(ColorMode::Gray, BitDepth::Sixteen, sixteen_bit(ColorMode::Gray)) },
        Fixture { name: "la16", raster: base(ColorMode::GrayAlpha, BitDepth::Sixteen, sixteen_bit(ColorMode::GrayAlpha)) },
        Fixture { name: "rgb16", raster: base(ColorMode::Rgb, BitDepth::Sixteen, sixteen_bit(ColorMode::Rgb)) },
        Fixture {
            name: "rgb16-icc",
            raster: with_icc(base(ColorMode::Rgb, BitDepth::Sixteen, sixteen_bit(ColorMode::Rgb)), srgb_profile()),
        },
        Fixture {
            name: "rgb8-icc",
            raster: with_icc(base(ColorMode::Rgb, BitDepth::Eight, eight_bit(ColorMode::Rgb)), srgb_profile()),
        },
        Fixture {
            name: "l16-icc",
            raster: with_icc(base(ColorMode::Gray, BitDepth::Sixteen, sixteen_bit(ColorMode::Gray)), gray_profile()),
        },
        Fixture { name: "indexed-p", raster: indexed() },
        Fixture { name: "bitonal", raster: bitonal() },
        Fixture {
            name: "cmyk8",
            raster: with_icc(base(ColorMode::Cmyk, BitDepth::Eight, eight_bit(ColorMode::Cmyk)), cmyk_profile()),
        },
    ]
}

/// One fixture by name. Panics: this is test material, and a typo in a fixture
/// name should stop the test rather than skip it.
pub fn by_name(name: &str) -> Fixture {
    all()
        .into_iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("no fixture named {name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_indexed_fixture_really_has_a_free_slot() {
        let raster = indexed();
        let entries = raster.palette.as_ref().unwrap().len() / 3;
        let highest_used = *raster.data.iter().max().unwrap() as usize;
        assert!(highest_used + 1 < entries, "used {highest_used}, palette holds {entries}");
    }

    #[test]
    fn the_bitonal_fixture_pads_its_rows() {
        let raster = bitonal();
        assert_eq!(raster.stride(), 8, "64 px at 1 bpp is 8 bytes");
        assert_eq!(raster.data.len(), 8 * H as usize);
    }

    #[test]
    fn sixteen_bit_samples_carry_low_byte_detail() {
        // If this held only high bytes, an 8-bit round-trip would pass the
        // fidelity test while destroying the source.
        let raster = base(ColorMode::Gray, BitDepth::Sixteen, sixteen_bit(ColorMode::Gray));
        assert!(raster.data.chunks_exact(2).any(|pair| pair[1] != 0));
    }

    #[test]
    fn the_fixture_profiles_are_profiles_a_reader_can_parse() {
        for bytes in [srgb_profile(), gray_profile(), cmyk_profile()] {
            assert!(lcms2::Profile::new_icc(&bytes).is_ok(), "lcms2 could not read back its own profile");
        }
    }

    /// A fixture that differs between two calls is not a fixture. `lcms2`
    /// stamps the creation date-time into bytes 24..36 as it serialises, so
    /// building a profile per call made every equality over one a race against
    /// the second hand - and made `all()`'s three ICC fixtures able to disagree
    /// with each other inside one process.
    #[test]
    fn a_profile_is_the_same_bytes_every_time_it_is_asked_for() {
        assert_eq!(srgb_profile(), srgb_profile());
        assert_eq!(gray_profile(), gray_profile());
        // The date-time field itself, named so a failure says which bytes.
        assert_eq!(srgb_profile()[24..36], srgb_profile()[24..36]);
        // And the fixtures built from it agree, which is the reason it matters.
        let rgb16 = by_name("rgb16-icc").raster.icc.expect("the fixture carries a profile");
        let rgb8 = by_name("rgb8-icc").raster.icc.expect("the fixture carries a profile");
        assert_eq!(rgb16, rgb8);
        assert_eq!(rgb16, srgb_profile());
    }
}
