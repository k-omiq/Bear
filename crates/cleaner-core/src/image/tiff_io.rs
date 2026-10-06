//! TIFF, which is here for the one thing PNG has no colour type for: CMYK.
//!
//! It is also the right choice for stitched longstrip output, where a
//! single file runs past what PNG is comfortable with.
//!
//! One conversion happens at this boundary and nowhere else. A [`Raster`] holds
//! 16-bit samples as **big-endian bytes**, because that is what PNG produces and
//! what TIFF's own `MM` byte order uses; the `tiff` crate works in native-order
//! `u16`, so this module swaps on the way in and out. Everything above sees one
//! layout.

use std::io::{Cursor, Seek, Write};

use tiff::decoder::{Decoder, DecodingResult};
use tiff::encoder::{TiffEncoder, colortype};
use tiff::tags::Tag;

use super::{BitDepth, ColorMode, Format, Header, ImageError, Raster};

impl From<tiff::TiffError> for ImageError {
    fn from(e: tiff::TiffError) -> Self {
        ImageError::Tiff(e.to_string())
    }
}

/// TIFF tag 34675, `ICC Profile`. Not in the crate's `Tag` enum.
const TAG_ICC_PROFILE: Tag = Tag::Unknown(34675);

/// The first IFD, without reading a strip.
pub fn header(reader: Cursor<&[u8]>) -> Result<Header, ImageError> {
    let source_bytes = *reader.get_ref();
    let mut decoder = Decoder::new(reader)?;
    let (width, height) = decoder.dimensions()?;
    let (mode, depth) = decoder_mode_and_depth(&mut decoder)?;
    let icc = decoder.find_tag(TAG_ICC_PROFILE)?.map(|value| value.into_u8_vec()).transpose()?;
    let mut color = description(&mut decoder, icc.is_some())?;
    super::exif::carry_portable(&mut color, source_bytes);
    Ok(Header { palette: None, width, height, mode, depth, icc, color, srgb_intent: None, trns: None })
}

fn description(decoder: &mut Decoder<Cursor<&[u8]>>, has_icc: bool) -> Result<super::metadata::ColorDescription, ImageError> {
    let associated_alpha = decoder.find_tag(Tag::ExtraSamples)?
        .map(|value| value.into_u16_vec()).transpose()?.is_some_and(|values| values.first() == Some(&1));
    let mut color = super::metadata::ColorDescription { associated_alpha, ..Default::default() };
    if !has_icc {
        for tag in [291, 301, 318, 319] {
            if decoder.find_tag(Tag::Unknown(tag))?.is_some() { color.unsupported_tiff_color.push(tag); }
        }
    }
    Ok(color)
}

fn decoder_mode_and_depth(decoder: &mut Decoder<Cursor<&[u8]>>) -> Result<(ColorMode, BitDepth), ImageError> {
    let color = decoder.colortype()?;
    // tiff 0.11 reports grayscale plus an explicit alpha sample as Multiband.
    if let tiff::ColorType::Multiband { bit_depth, num_samples: 2 } = color {
        let extra = decoder.find_tag(Tag::ExtraSamples)?.map(|value| value.into_u16_vec()).transpose()?;
        if decoder.get_tag(Tag::PhotometricInterpretation)?.into_u16()? == 1
            && extra.as_deref().is_some_and(|values| matches!(values, [1] | [2])) {
            return mode_and_depth(tiff::ColorType::GrayA(bit_depth));
        }
    }
    mode_and_depth(color)
}

/// The one place TIFF's colour type is mapped, so the header read and the full
/// decode cannot disagree about what a file is.
fn mode_and_depth(color: tiff::ColorType) -> Result<(ColorMode, BitDepth), ImageError> {
    Ok(match color {
        tiff::ColorType::Gray(8) => (ColorMode::Gray, BitDepth::Eight),
        tiff::ColorType::Gray(16) => (ColorMode::Gray, BitDepth::Sixteen),
        tiff::ColorType::GrayA(8) => (ColorMode::GrayAlpha, BitDepth::Eight),
        tiff::ColorType::GrayA(16) => (ColorMode::GrayAlpha, BitDepth::Sixteen),
        tiff::ColorType::RGB(8) => (ColorMode::Rgb, BitDepth::Eight),
        tiff::ColorType::RGB(16) => (ColorMode::Rgb, BitDepth::Sixteen),
        tiff::ColorType::RGBA(8) => (ColorMode::Rgba, BitDepth::Eight),
        tiff::ColorType::RGBA(16) => (ColorMode::Rgba, BitDepth::Sixteen),
        tiff::ColorType::CMYK(8) => (ColorMode::Cmyk, BitDepth::Eight),
        tiff::ColorType::CMYK(16) => (ColorMode::Cmyk, BitDepth::Sixteen),
        other => return Err(ImageError::Tiff(format!("unsupported colour type {other:?}"))),
    })
}

pub fn decode(reader: Cursor<&[u8]>) -> Result<Raster, ImageError> {
    let source_bytes = *reader.get_ref();
    let mut decoder = Decoder::new(reader)?;
    let (width, height) = decoder.dimensions()?;
    let (mode, depth) = decoder_mode_and_depth(&mut decoder)?;

    let icc = decoder
        .find_tag(TAG_ICC_PROFILE)?
        .map(|value| value.into_u8_vec())
        .transpose()?;

    let mut color_description = description(&mut decoder, icc.is_some())?;
    super::exif::carry_portable(&mut color_description, source_bytes);
    let data = match decoder.read_image()? {
        DecodingResult::U8(samples) => samples,
        DecodingResult::U16(samples) => {
            let mut bytes = Vec::with_capacity(samples.len() * 2);
            for sample in samples {
                bytes.extend_from_slice(&sample.to_be_bytes());
            }
            bytes
        }
        other => {
            return Err(ImageError::Tiff(format!(
                "unsupported sample type {}",
                match other {
                    DecodingResult::U32(_) => "u32",
                    DecodingResult::U64(_) => "u64",
                    DecodingResult::F32(_) => "f32",
                    DecodingResult::F64(_) => "f64",
                    _ => "unknown",
                }
            )));
        }
    };

    Ok(Raster {
        width,
        height,
        mode,
        depth,
        icc,
        palette: None,
        trns: None,
        srgb_intent: None,
        color: color_description,
        data,
    })
}

pub fn encode(raster: &Raster) -> Result<Vec<u8>, ImageError> {
    let mut raster = raster.clone();
    raster.icc = super::metadata::equivalent_icc(&raster.color, raster.icc.as_deref(), raster.srgb_intent, raster.mode)?;
    if raster.trns.is_some() { return Err(ImageError::Color("TIFF cannot retain PNG color-key transparency; export PNG".into())); }
    let raster = &raster;
    let mut out = Cursor::new(Vec::new());
    {
        let mut encoder = TiffEncoder::new(&mut out)?;
        match (raster.mode, raster.depth) {
            (ColorMode::Gray, BitDepth::Eight) => write::<colortype::Gray8, _>(&mut encoder, raster)?,
            (ColorMode::Gray, BitDepth::Sixteen) => write::<colortype::Gray16, _>(&mut encoder, raster)?,
            (ColorMode::GrayAlpha, BitDepth::Eight) => write::<colortype::Gray8, _>(&mut encoder, raster)?,
            (ColorMode::GrayAlpha, BitDepth::Sixteen) => write::<colortype::Gray16, _>(&mut encoder, raster)?,
            (ColorMode::Rgb, BitDepth::Eight) => write::<colortype::RGB8, _>(&mut encoder, raster)?,
            (ColorMode::Rgb, BitDepth::Sixteen) => write::<colortype::RGB16, _>(&mut encoder, raster)?,
            (ColorMode::Rgba, BitDepth::Eight) => write::<colortype::RGBA8, _>(&mut encoder, raster)?,
            (ColorMode::Rgba, BitDepth::Sixteen) => write::<colortype::RGBA16, _>(&mut encoder, raster)?,
            (ColorMode::Cmyk, BitDepth::Eight) => write::<colortype::CMYK8, _>(&mut encoder, raster)?,
            (ColorMode::Cmyk, BitDepth::Sixteen) => write::<colortype::CMYK16, _>(&mut encoder, raster)?,
            // Sub-byte depths and palettes have no native encoder here, and all bit
            // depths must be equal. Declared unrepresentable rather than
            // silently promoted. Nothing routes here in practice,
            // because `lossless_format_for` sends everything but CMYK to PNG.
            (mode, depth) => return Err(ImageError::Unrepresentable { format: Format::Tiff, mode, depth }),
        }
    }
    Ok(out.into_inner())
}

/// One generic body for every colour type, so the ICC tag is written in exactly
/// one place and cannot be forgotten for a mode added later.
fn write<C, W>(encoder: &mut TiffEncoder<W>, raster: &Raster) -> Result<(), ImageError>
where
    C: colortype::ColorType,
    W: Write + Seek,
    [C::Inner]: tiff::encoder::TiffValue,
    C::Inner: SampleFromBytes,
{
    let mut image = encoder.new_image::<C>(raster.width, raster.height)?;
    if raster.mode == ColorMode::GrayAlpha {
        image.extra_samples(&[if raster.color.associated_alpha { tiff::tags::ExtraSamples::AssociatedAlpha } else { tiff::tags::ExtraSamples::UnassociatedAlpha }])?;
    } else if raster.mode == ColorMode::Rgba {
        image.encoder().write_tag(Tag::ExtraSamples, &[if raster.color.associated_alpha {1u16} else {2u16}][..])?;
    }
    if let Some(icc) = &raster.icc {
        image.encoder().write_tag(TAG_ICC_PROFILE, icc.as_slice())?;
    }
    let samples = C::Inner::from_be_bytes(&raster.data);
    image.write_data(&samples)?;
    Ok(())
}

/// Turning the raster's big-endian byte buffer back into the crate's native
/// sample type. Only `u8` and `u16` are reachable from [`encode`]'s match.
pub trait SampleFromBytes: Sized {
    fn from_be_bytes(bytes: &[u8]) -> Vec<Self>;
}

impl SampleFromBytes for u8 {
    fn from_be_bytes(bytes: &[u8]) -> Vec<u8> {
        bytes.to_vec()
    }
}

impl SampleFromBytes for u16 {
    fn from_be_bytes(bytes: &[u8]) -> Vec<u16> {
        bytes.chunks_exact(2).map(|pair| u16::from_be_bytes([pair[0], pair[1]])).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::{self, Target};
    fn reference(name: &str) -> Vec<u8> {
        std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/color-reference").join(name)).unwrap()
    }
    #[test]
    fn independent_alpha_tiffs_keep_native_samples_and_explicit_association() {
        for name in ["associated-rgba8", "associated-rgba16", "associated-graya8", "associated-graya16", "straight-rgba8", "straight-rgba16"] {
            let bytes = reference(&format!("{name}.tif"));
            let page = decode(Cursor::new(&bytes)).unwrap();
            let associated = name.starts_with("associated");
            assert_eq!(page.color.associated_alpha, associated, "{name}");
            assert_eq!(page.data, reference(&format!("{name}.native")), "{name}: independent tifffile native samples");
            assert_eq!(export::export_page(&bytes, &[], Target::SameAsSource).unwrap().bytes, bytes);
            let buffered = encode(&page).unwrap();
            let mut streaming = Cursor::new(Vec::new());
            export::rows::write_rows(&mut streaming, &export::Canvas::of(&page), Format::Tiff, &mut |y, row| {
                let start = y as usize * page.stride(); row.data.copy_from_slice(&page.data[start..start+page.stride()]); Ok(())
            }).unwrap();
            for encoded in [&buffered, streaming.get_ref()] {
                let mut decoder = Decoder::new(Cursor::new(encoded)).unwrap();
                assert_eq!(decoder.get_tag_u16_vec(Tag::ExtraSamples).unwrap(), [if associated {1} else {2}], "{name}");
                let back = decode(Cursor::new(encoded)).unwrap();
                assert_eq!(back.data, page.data, "{name}");
                assert_eq!(back.color.associated_alpha, associated);
            }
            if associated {
                assert!(crate::image::encode(&page, Format::Png).unwrap_err().to_string().contains("associated-alpha"));
                let plan = export::plan_page(&bytes, &[], Target::Explicit(Format::Png)).unwrap();
                assert_eq!(plan.format, Format::Tiff);
                assert!(!plan.declared.is_empty());
                let mut psd = Vec::new();
                assert!(export::export_page_psd_to(&bytes, &[], false, &mut psd).is_err());
                assert!(psd.is_empty());
            }
        }
    }
    #[test]
    fn associated_preview_matches_independent_pillow_rgba_without_rewriting_native_samples() {
        let bytes = reference("associated-rgba8.tif");
        let page = decode(Cursor::new(&bytes)).unwrap();
        let shown = crate::image::proxy::display(&page).unwrap();
        let expected = reference("associated-rgba8.rgba");
        for (actual, expected) in shown.data.iter().zip(expected) {
            assert!((*actual as i16 - expected as i16).abs() <= 1, "{actual} vs {expected}");
        }
        assert_eq!(page.data, reference("associated-rgba8.native"));
    }
    #[test]
    fn legacy_non_icc_color_declarations_are_retained_natively_and_refuse_uncalibrated_processing() {
        for tag in [301,318,319] {
            let bytes = reference(&format!("legacy-color-{tag}.tif"));
            let page = decode(Cursor::new(&bytes)).unwrap();
            assert_eq!(page.color.unsupported_tiff_color, [tag]);
            assert!(crate::ingest::source_ref(std::path::Path::new("native.tif"), &bytes).is_ok());
            assert!(crate::ingest::decode_whole(&bytes).is_ok());
            assert_eq!(export::export_page(&bytes, &[], Target::SameAsSource).unwrap().bytes, bytes);
            assert!(crate::image::proxy::display(&page).unwrap_err().to_string().contains("TIFF color tags"));
            assert!(crate::image::encode(&page, Format::Png).is_err());
            assert!(encode(&page).is_err());
            let mut psd = Vec::new();
            assert!(export::export_page_psd_to(&bytes, &[], false, &mut psd).is_err());
            assert!(psd.is_empty());
        }
    }
}
