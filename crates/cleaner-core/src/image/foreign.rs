//! Decoders for JPEG, WebP, GIF and BMP.
//!
//! JPEG is a native source: ingest copies its compressed bytes verbatim into
//! the chapter's page directory. Preview and processing decode those bytes on
//! demand. WebP, GIF and BMP are converted to PNG in that same directory.
//!
//! Decoding uses the four pure Rust codecs enabled in `Cargo.toml`. AVIF,
//! HEIC and JPEG XL remain unsupported. Multi-frame images use the first frame.
//! ICC bytes travel with decoded samples; extraction failures are propagated.
//! See `docs/color-fidelity-audit.md` for the limits of the decoder's colour
//! handling, especially CMYK JPEGs.

use std::io::Cursor;

use image::{DynamicImage, ImageDecoder};

use super::{BitDepth, ColorMode, Header, ImageError, Raster};

/// Metadata without decompressing the JPEG's pixels.
pub fn jpeg_header(bytes: &[u8]) -> Result<Header, ImageError> {
    use zune_core::{bytestream::ZCursor, colorspace::ColorSpace};
    let mut decoder = zune_jpeg::JpegDecoder::new(ZCursor::new(bytes));
    decoder.decode_headers().map_err(jpeg_error)?;
    let info = decoder
        .info()
        .ok_or_else(|| ImageError::Foreign("JPEG has no header".into()))?;
    let mode = match decoder.input_colorspace() {
        Some(ColorSpace::Luma) => ColorMode::Gray,
        Some(ColorSpace::CMYK | ColorSpace::YCCK) => ColorMode::Cmyk,
        Some(ColorSpace::RGB | ColorSpace::YCbCr) => ColorMode::Rgb,
        other => {
            return Err(ImageError::Foreign(format!(
                "unsupported JPEG colour type: {other:?}"
            )))
        }
    };
    let mut color = super::ColorDescription::default();
    super::exif::carry_portable(&mut color, bytes);
    Ok(Header { palette: None,
        width: info.width as u32,
        height: info.height as u32,
        mode,
        depth: BitDepth::Eight,
        icc: decoder.icc_profile().filter(|bytes| !bytes.is_empty()),
        color,
        srgb_intent: None,
        trns: None,
    })
}

fn jpeg_error(error: impl std::fmt::Display) -> ImageError {
    ImageError::Foreign(format!("JPEG: {error}"))
}

/// Normalized native JPEG samples. CMYK uses zero=no ink, independently of
/// Adobe's inverted on-disk convention. YCCK is explicitly converted from raw
/// components: asking zune for CMYK output does not implement this conversion.
fn decode_jpeg(bytes: &[u8]) -> Result<Raster, ImageError> {
    use zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions};
    let header = jpeg_header(bytes)?;
    if header.mode != ColorMode::Cmyk {
        let mut page = raster_of(
            image::codecs::jpeg::JpegDecoder::new(Cursor::new(bytes))?,
            false,
        )?;
        page.color = header.color;
        return Ok(page);
    }
    let mut decoder = zune_jpeg::JpegDecoder::new(ZCursor::new(bytes));
    decoder.decode_headers().map_err(jpeg_error)?;
    let space = decoder
        .input_colorspace()
        .ok_or_else(|| jpeg_error("missing color space"))?;
    decoder.set_options(DecoderOptions::default().jpeg_set_out_colorspace(space));
    let mut data = decoder.decode().map_err(jpeg_error)?;
    let adobe = jpeg_adobe_transform(bytes);
    for pixel in data.chunks_exact_mut(4) {
        if space == ColorSpace::YCCK {
            let y = f64::from(pixel[0]);
            let cb = f64::from(pixel[1]) - 128.0;
            let cr = f64::from(pixel[2]) - 128.0;
            pixel[0] = (y + 1.402 * cr).round().clamp(0.0, 255.0) as u8;
            pixel[1] = (y - 0.344136 * cb - 0.714136 * cr)
                .round()
                .clamp(0.0, 255.0) as u8;
            pixel[2] = (y + 1.772 * cb).round().clamp(0.0, 255.0) as u8;
            pixel[3] = 255 - pixel[3];
        } else if adobe == Some(0) {
            for value in pixel {
                *value = 255 - *value;
            }
        }
    }
    Ok(Raster {
        width: header.width,
        height: header.height,
        mode: header.mode,
        depth: header.depth,
        icc: header.icc,
        palette: None,
        trns: None,
        srgb_intent: None,
        color: header.color,
        data,
    })
}

/// Four-component JPEG without APP14 has no reliable CMYK/YCCK discriminator.
/// Follow libjpeg's conventional non-inverted CMYK interpretation and disclose it.
pub fn assumes_unmarked_cmyk(bytes: &[u8]) -> bool {
    SourceFormat::sniff(bytes) == Some(SourceFormat::Jpeg)
        && jpeg_adobe_transform(bytes).is_none()
        && jpeg_header(bytes).is_ok_and(|header| header.mode == ColorMode::Cmyk)
}

/// Read marker boundaries, never search arbitrary compressed image bytes.
fn jpeg_adobe_transform(bytes: &[u8]) -> Option<u8> {
    let mut offset = 2;
    while offset + 4 <= bytes.len() {
        if bytes[offset] != 0xff {
            return None;
        }
        while bytes.get(offset) == Some(&0xff) {
            offset += 1;
        }
        let marker = *bytes.get(offset)?;
        offset += 1;
        if marker == 0xda || marker == 0xd9 {
            break;
        }
        if marker == 0x01 || (0xd0..=0xd8).contains(&marker) {
            continue;
        }
        let length = u16::from_be_bytes(bytes.get(offset..offset + 2)?.try_into().ok()?) as usize;
        if length < 2 {
            return None;
        }
        let segment = bytes.get(offset + 2..offset.checked_add(length)?)?;
        if marker == 0xee && segment.starts_with(b"Adobe") && segment.len() >= 12 {
            return Some(segment[11]);
        }
        offset += length;
    }
    None
}

/// A format a page can arrive in and cannot leave in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFormat {
    Jpeg,
    Webp,
    Gif,
    Bmp,
}

impl SourceFormat {
    /// Sniffed from the leading bytes, for the reason [`super::Format::sniff`]
    /// gives: a file's name is a claim and its magic is evidence. A folder of
    /// `.jpg` files that are really PNGs is a real thing that comes out of
    /// batch converters, and it must not decide the decoder.
    pub fn sniff(bytes: &[u8]) -> Option<SourceFormat> {
        if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            Some(SourceFormat::Jpeg)
        } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
            Some(SourceFormat::Webp)
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            Some(SourceFormat::Gif)
        } else if bytes.starts_with(b"BM") {
            Some(SourceFormat::Bmp)
        } else {
            None
        }
    }

    /// What the conversion notice calls this format. Not a translated string:
    /// `JPEG` is `JPEG` in every catalogue, and it is interpolated into a
    /// sentence that is translated.
    pub fn label(self) -> &'static str {
        match self {
            SourceFormat::Jpeg => "JPEG",
            SourceFormat::Webp => "WebP",
            SourceFormat::Gif => "GIF",
            SourceFormat::Bmp => "BMP",
        }
    }
}

/// Decode one of the four, ICC profile included where the file carried one.
///
/// The profile matters more here than anywhere else in the module: a JPEG from
/// a scanner very often carries one, and dropping it on the way into the PNG
/// would change how every later page looks without anything in the manifest
/// recording that it happened.
///
/// A multi-frame file - an animated GIF or WebP - yields its **first frame**.
/// A page is one image; the alternative is to refuse a file whose first frame
/// is exactly the page the user meant.
pub fn decode(bytes: &[u8]) -> Result<Raster, ImageError> {
    match SourceFormat::sniff(bytes).ok_or(ImageError::UnknownFormat)? {
        SourceFormat::Jpeg => decode_jpeg(bytes),
        SourceFormat::Webp => raster_of(
            image::codecs::webp::WebPDecoder::new(Cursor::new(bytes))?,
            true,
        ),
        SourceFormat::Gif => match super::import_info::static_indexed_gif(bytes)? {
            Some(page) => Ok(page),
            None => raster_of(
                image::codecs::gif::GifDecoder::new(Cursor::new(bytes))?,
                false,
            ),
        },
        SourceFormat::Bmp => raster_of(
            image::codecs::bmp::BmpDecoder::new(Cursor::new(bytes))?,
            false,
        ),
    }
}

impl From<image::ImageError> for ImageError {
    fn from(error: image::ImageError) -> ImageError {
        ImageError::Foreign(error.to_string())
    }
}

/// One decoder's output as a [`Raster`].
///
/// The profile is read **before** the pixels, because `from_decoder` consumes
/// the decoder.
fn raster_of<D: ImageDecoder>(mut decoder: D, preserve_exif: bool) -> Result<Raster, ImageError> {
    let icc = decoder.icc_profile()?.filter(|bytes| !bytes.is_empty());
    // Conversion keeps the native frame dimensions and samples. Preserve EXIF
    // in its working PNG; later edited exports omit stale thumbnails/geometry.
    let exif = if preserve_exif {
        decoder.exif_metadata()?.filter(|bytes| !bytes.is_empty())
    } else {
        None
    };
    let mut color = super::metadata::ColorDescription::default();
    if let Some(exif) = exif {
        let data = exif.strip_prefix(b"Exif\0\0").unwrap_or(&exif).to_vec();
        if data.starts_with(b"II\x2a\0") || data.starts_with(b"MM\0\x2a") {
            color.safe_ancillary.push(super::metadata::AncillaryChunk {
                name: *b"eXIf",
                data,
                after_data: false,
            });
        }
    }
    let image = DynamicImage::from_decoder(decoder)?;
    let (width, height) = (image.width(), image.height());

    // Preserve supported integer samples; an unsupported decoder layout must
    // never silently become an 8-bit working source.
    let (mode, depth, data) = match image {
        DynamicImage::ImageLuma8(buffer) => {
            (ColorMode::Gray, BitDepth::Eight, buffer.into_raw())
        }
        DynamicImage::ImageLumaA8(buffer) => {
            (ColorMode::GrayAlpha, BitDepth::Eight, buffer.into_raw())
        }
        DynamicImage::ImageRgb8(buffer) => (ColorMode::Rgb, BitDepth::Eight, buffer.into_raw()),
        DynamicImage::ImageRgba8(buffer) => (ColorMode::Rgba, BitDepth::Eight, buffer.into_raw()),
        DynamicImage::ImageLuma16(buffer) => {
            (ColorMode::Gray, BitDepth::Sixteen, big_endian(buffer.into_raw()))
        }
        DynamicImage::ImageLumaA16(buffer) => {
            (ColorMode::GrayAlpha, BitDepth::Sixteen, big_endian(buffer.into_raw()))
        }
        DynamicImage::ImageRgb16(buffer) => {
            (ColorMode::Rgb, BitDepth::Sixteen, big_endian(buffer.into_raw()))
        }
        DynamicImage::ImageRgba16(buffer) => {
            (ColorMode::Rgba, BitDepth::Sixteen, big_endian(buffer.into_raw()))
        }
        other => return Err(ImageError::Color(format!("unsupported source sample encoding {:?}; original must be retained without quantization", other.color()))),
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
        color,
        data,
    })
}

/// [`Raster::data`] is big-endian at 16 bits, because that is how PNG stores a
/// 16-bit sample and the whole buffer is "as the decoder produced it" for
/// exactly one decoder. The `image` crate's is native-endian, so this is the
/// one place a byte order is imposed rather than carried.
fn big_endian(samples: Vec<u16>) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_be_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{encode, fixtures, Format};

    /// A real JPEG of one of this crate's own fixtures.
    ///
    /// Built through the `image` crate's encoder rather than checked in as a
    /// blob, so the test exercises the same library the decoder comes from and
    /// nothing has to be regenerated when a fixture changes. Note that the PNG
    /// *decoder* is deliberately not among this crate's enabled `image`
    /// features - the fixture's samples are handed over directly.
    fn jpeg_of(name: &str) -> Vec<u8> {
        let raster = &fixtures::by_name(name).raster;
        let buffer = image::RgbImage::from_raw(raster.width, raster.height, raster.data.clone())
            .expect("the fixture is 8-bit RGB");
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(buffer)
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
            .unwrap();
        bytes
    }

    #[test]
    fn native_cmyk_and_ycck_match_independent_libjpeg_components() {
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/color-reference");
        for name in [
            "cmyk-adobe",
            "cmyk-plain",
            "ycck-adobe",
            "ycck-subsampled",
            "ycck-markerless",
        ] {
            let expected = std::fs::read(root.join(format!("{name}.jpg.cmyk"))).unwrap();
            for tagged in [false, true] {
                let suffix = if tagged { "-icc" } else { "" };
                let bytes = std::fs::read(root.join(format!("{name}{suffix}.jpg"))).unwrap();
                let header = jpeg_header(&bytes).unwrap();
                let raster = decode(&bytes).unwrap();
                assert_eq!(header.mode, ColorMode::Cmyk);
                assert_eq!(raster.mode, header.mode);
                assert_eq!((raster.width, raster.height), (16, 8));
                assert_eq!(raster.data.len(), expected.len());
                let max_difference = raster
                    .data
                    .iter()
                    .zip(&expected)
                    .map(|(actual, reference)| actual.abs_diff(*reference))
                    .max()
                    .unwrap();
                // Upsampling adds chroma rounding before the YCCK matrix; two
                // chroma codes can amplify to four color codes, plus one luma.
                let tolerance = if name == "ycck-subsampled" { 5 } else { 2 };
                assert!(
                    max_difference <= tolerance,
                    "{name}: maximum difference {max_difference}"
                );
                assert_eq!(raster.icc.is_some(), tagged);
                let back = crate::image::decode(&encode(&raster, Format::Tiff).unwrap()).unwrap();
                assert_eq!(back.data, raster.data);
                assert_eq!(back.icc, raster.icc);
                assert!(encode(&raster, Format::Png).is_err());
            }
        }
    }

    #[test]
    fn independent_reference_fixture_hashes_are_stable() {
        use sha2::{Digest, Sha256};
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/color-reference");
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
        for (name, expected) in manifest["sha256"].as_object().unwrap() {
            let bytes = std::fs::read(root.join(name)).unwrap();
            assert_eq!(
                format!("{:x}", Sha256::digest(bytes)),
                expected.as_str().unwrap(),
                "{name}"
            );
        }
        let profile = lcms2::Profile::new_icc(&fixtures::cmyk_profile()).unwrap();
        assert_eq!(profile.color_space(), lcms2::ColorSpaceSignature::CmykData);
    }

    #[test]
    fn the_four_magics_are_told_apart() {
        assert_eq!(
            SourceFormat::sniff(&jpeg_of("rgb8")),
            Some(SourceFormat::Jpeg)
        );
        assert_eq!(SourceFormat::sniff(b"GIF89a...."), Some(SourceFormat::Gif));
        assert_eq!(SourceFormat::sniff(b"BM......"), Some(SourceFormat::Bmp));
        assert_eq!(
            SourceFormat::sniff(b"RIFF\0\0\0\0WEBPVP8 "),
            Some(SourceFormat::Webp)
        );
        // A PNG is not a *source* format: it is already one we write.
        let png = encode(&fixtures::by_name("rgb8").raster, Format::Png).unwrap();
        assert_eq!(SourceFormat::sniff(&png), None);
    }

    #[test]
    fn a_jpeg_decodes_to_a_raster_the_png_encoder_accepts() {
        let raster = decode(&jpeg_of("rgb8")).unwrap();
        assert_eq!(raster.mode, ColorMode::Rgb);
        assert_eq!(raster.depth, BitDepth::Eight);
        assert_eq!(raster.data.len(), raster.stride() * raster.height as usize);
        // The point of the whole module: what comes out is writable as PNG.
        let png = encode(&raster, Format::Png).unwrap();
        assert!(crate::image::decode(&png).unwrap().same_geometry(&raster));
    }

    #[test]
    fn a_truncated_source_is_an_error_rather_than_a_short_buffer() {
        let whole = jpeg_of("rgb8");
        assert!(decode(&whole[..whole.len() / 3]).is_err());
    }

    #[test]
    fn jpeg_header_and_decode_keep_the_embedded_profile() {
        use image::ImageEncoder;
        let source = fixtures::by_name("rgb8-icc").raster;
        let mut bytes = Vec::new();
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 95);
        encoder
            .set_icc_profile(source.icc.clone().unwrap())
            .unwrap();
        encoder
            .encode(
                &source.data,
                source.width,
                source.height,
                image::ExtendedColorType::Rgb8,
            )
            .unwrap();
        assert_eq!(jpeg_header(&bytes).unwrap().icc, source.icc);
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded.icc, source.icc);
        let lossless = crate::image::decode(&encode(&decoded, Format::Png).unwrap()).unwrap();
        assert_eq!(lossless.data, decoded.data);
        assert_eq!(lossless.icc, decoded.icc);
    }
}
