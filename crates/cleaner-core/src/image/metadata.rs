//! Exact source declarations and the common lossless-writer policy.
//! Raw PNG integers are retained rather than reconstructed decoder defaults.
use super::{ColorMode, ImageError};
use lcms2::{CIExyY, CIExyYTRIPLE, ColorSpaceSignature, Profile, ToneCurve};
use std::io::{self, Write};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColorDescription {
    /// TIFF associated alpha: native color codes are premultiplied, never
    /// silently relabeled as the straight-alpha samples used by PNG/PSD.
    pub associated_alpha: bool,
    /// Non-ICC TIFF transfer/color declarations not yet representable by the
    /// managed resolver. Keep native imports; explicitly refuse processing.
    pub unsupported_tiff_color: Vec<u16>,
    pub gamma: Option<u32>,
    pub chromaticities: Option<[u32; 8]>,
    pub significant_bits: Option<Vec<u8>>,
    pub cicp: Option<[u8; 4]>,
    pub mastering_display: Option<Vec<u8>>,
    pub content_light: Option<Vec<u8>>,
    pub safe_ancillary: Vec<AncillaryChunk>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AncillaryChunk {
    pub name: [u8; 4],
    pub data: Vec<u8>,
    pub after_data: bool,
}
impl ColorDescription {
    pub fn validate_interpretation(&self) -> Result<(), ImageError> {
        if !self.unsupported_tiff_color.is_empty() {
            return Err(ImageError::Color(format!("TIFF color tags {:?} require a supported equivalent ICC interpretation; native original and unchanged TIFF passthrough remain available", self.unsupported_tiff_color)));
        }
        Ok(())
    }
    pub fn after_edit(&self) -> Self {
        let mut color = self.clone();
        color.significant_bits = None;
        color.content_light = None;
        color
    }
    pub fn same_interpretation(&self, other: &Self) -> bool {
        self.associated_alpha == other.associated_alpha
            && self.unsupported_tiff_color == other.unsupported_tiff_color
            && self.gamma == other.gamma
            && self.chromaticities == other.chromaticities
            && self.cicp == other.cicp
            && self.mastering_display == other.mastering_display
    }
    pub fn from_png(bytes: &[u8]) -> Result<Self, ImageError> {
        let mut out = Self::default();
        let mut at = 8usize;
        let mut after_data = false;
        let mut kept = 0usize;
        while at.checked_add(12).is_some_and(|end| end <= bytes.len()) {
            let len = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            let end = at
                .checked_add(12)
                .and_then(|n| n.checked_add(len))
                .filter(|end| *end <= bytes.len())
                .ok_or_else(|| ImageError::Color("truncated PNG chunk".into()))?;
            let name: [u8; 4] = bytes[at + 4..at + 8].try_into().unwrap();
            let data = &bytes[at + 8..end - 4];
            let known = matches!(
                &name,
                b"eXIf" | b"gAMA" | b"cHRM" | b"sBIT" | b"cICP" | b"mDCV" | b"cLLI"
            );
            let safe = name[0].is_ascii_lowercase()
                && name[3].is_ascii_lowercase()
                && !matches!(&name, b"tRNS" | b"iCCP" | b"eXIf");
            if known || safe {
                kept += len;
                if kept > 16 * 1024 * 1024 {
                    return Err(ImageError::Color(
                        "PNG metadata exceeds 16 MiB limit".into(),
                    ));
                }
                let crc = u32::from_be_bytes(bytes[end - 4..end].try_into().unwrap());
                if crc32fast::hash(&bytes[at + 4..end - 4]) != crc {
                    return Err(ImageError::Color("PNG metadata CRC mismatch".into()));
                }
            }
            match &name {
                b"eXIf" => {
                    if let Some(data) = super::exif::portable(data) {
                        out.safe_ancillary.push(AncillaryChunk {
                            name,
                            data,
                            after_data: false,
                        });
                    }
                }
                b"gAMA" if len == 4 => {
                    let n = u32::from_be_bytes(data.try_into().unwrap());
                    if n == 0 {
                        return Err(ImageError::Color("zero PNG gamma".into()));
                    }
                    out.gamma = Some(n);
                }
                b"cHRM" if len == 32 => {
                    out.chromaticities = Some(std::array::from_fn(|i| {
                        u32::from_be_bytes(data[i * 4..i * 4 + 4].try_into().unwrap())
                    }))
                }
                b"sBIT" => out.significant_bits = Some(data.to_vec()),
                b"cICP" if len == 4 => out.cicp = Some(data.try_into().unwrap()),
                b"mDCV" if len == 24 => out.mastering_display = Some(data.to_vec()),
                b"cLLI" if len == 8 => out.content_light = Some(data.to_vec()),
                b"IDAT" => after_data = true,
                b"IEND" => break,
                _ if known => {
                    return Err(ImageError::Color(format!(
                        "invalid {} length",
                        String::from_utf8_lossy(&name)
                    )))
                }
                _ if safe => out.safe_ancillary.push(AncillaryChunk {
                    name,
                    data: data.to_vec(),
                    after_data,
                }),
                _ => (),
            }
            at = end;
        }
        Ok(out)
    }
}

pub fn validate_profile(icc: Option<&[u8]>, mode: ColorMode) -> Result<(), ImageError> {
    let Some(bytes) = icc else {
        return Ok(());
    };
    if bytes.len() < 132
        || bytes.get(36..40) != Some(b"acsp")
        || u32::from_be_bytes(bytes[0..4].try_into().unwrap()) as usize != bytes.len()
    {
        return Err(ImageError::Color(
            "invalid ICC header or profile length; original remains available".into(),
        ));
    }
    let profile = Profile::new_icc(bytes)
        .map_err(|e| ImageError::Color(format!("invalid ICC profile: {e}")))?;
    let expected = match mode {
        ColorMode::Gray | ColorMode::GrayAlpha => ColorSpaceSignature::GrayData,
        ColorMode::Cmyk => ColorSpaceSignature::CmykData,
        _ => ColorSpaceSignature::RgbData,
    };
    if profile.color_space() != expected {
        return Err(ImageError::Color(format!("ICC {:?} does not describe {mode:?} samples; preserve original or supply a compatible profile",profile.color_space())));
    }
    Ok(())
}

/// Equivalent ICC for formats that cannot store PNG declarations. No profile is
/// invented for a completely untagged source. Unsupported declarations refuse.
pub fn equivalent_icc(
    color: &ColorDescription,
    icc: Option<&[u8]>,
    srgb: Option<u8>,
    mode: ColorMode,
) -> Result<Option<Vec<u8>>, ImageError> {
    color.validate_interpretation()?;
    validate_profile(icc, mode)?;
    if color.cicp.is_none() {
        if let Some(profile) = icc {
            return Ok(Some(profile.to_vec()));
        }
    }
    if color.gamma.is_none()
        && color.chromaticities.is_none()
        && color.cicp.is_none()
        && srgb.is_none()
    {
        return Ok(None);
    }
    if mode == ColorMode::Cmyk {
        return Err(ImageError::Color(
            "RGB/gray declarations cannot describe CMYK".into(),
        ));
    }
    let xy = |x: f64, y: f64| CIExyY { x, y, Y: 1.0 };
    let standard = [31270, 32900, 64000, 33000, 30000, 60000, 15000, 6000];
    let mut points = if srgb.is_some() {
        standard
    } else {
        color.chromaticities.unwrap_or(standard)
    };
    let mut srgb_curve = srgb.is_some();
    let mut rec709 = false;
    if let Some([primaries, transfer, matrix, range]) = color.cicp {
        if matrix != 0 || range != 1 {
            return Err(ImageError::Color(
                "unsupported cICP matrix/range; retain native PNG".into(),
            ));
        }
        points = match primaries {
            1 => [31270, 32900, 64000, 33000, 30000, 60000, 15000, 6000],
            12 => [31270, 32900, 68000, 32000, 26500, 69000, 15000, 6000],
            9 => [31270, 32900, 70800, 29200, 17000, 79700, 13100, 4600],
            _ => {
                return Err(ImageError::Color(format!(
                    "unsupported cICP primaries {primaries}; retain native PNG"
                )))
            }
        };
        srgb_curve = false;
        match transfer {
            13 => srgb_curve = true,
            1 | 14 | 15 => rec709 = true,
            _ => return Err(ImageError::Color(format!(
                "unsupported cICP transfer {transfer}; HDR requires a tested tone mapping policy"
            ))),
        }
    }
    let curve=if srgb_curve { ToneCurve::new_parametric(4,&[2.4,1.0/1.055,0.055/1.055,1.0/12.92,0.04045]) }
        else if rec709 { ToneCurve::new_parametric(4,&[1.0/0.45,1.0/1.099,0.099/1.099,1.0/4.5,0.081]) }
        else if let Some(gamma)=color.gamma { if gamma==0 { return Err(ImageError::Color("zero gamma".into())); } Ok(ToneCurve::new(100000.0/f64::from(gamma))) }
        else { return Err(ImageError::Color("chromaticities without a transfer declaration cannot be converted to an equivalent ICC profile".into())); }
        .map_err(|e|ImageError::Color(e.to_string()))?;
    let point = |i: usize| {
        xy(
            f64::from(points[i]) / 100000.0,
            f64::from(points[i + 1]) / 100000.0,
        )
    };
    let profile = match mode {
        ColorMode::Gray | ColorMode::GrayAlpha => Profile::new_gray(&point(0), &curve),
        _ => Profile::new_rgb(
            &point(0),
            &CIExyYTRIPLE {
                Red: point(2),
                Green: point(4),
                Blue: point(6),
            },
            &[&curve, &curve, &curve],
        ),
    }
    .map_err(|e| ImageError::Color(e.to_string()))?;
    profile
        .icc()
        .map(Some)
        .map_err(|e| ImageError::Color(e.to_string()))
}

fn chunk(out: &mut Vec<u8>, name: [u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(&name);
    out.extend_from_slice(data);
    let mut crc = crc32fast::Hasher::new();
    crc.update(&name);
    crc.update(data);
    out.extend_from_slice(&crc.finalize().to_be_bytes());
}
/// Insert declarations immediately after IHDR so even indexed outputs obey
/// pre-PLTE ordering. Both buffered and streaming encoders use this adapter.
pub struct PngSink<'a, W: Write> {
    sink: &'a mut W,
    left: usize,
    prefix: Vec<u8>,
}
impl<'a, W: Write> PngSink<'a, W> {
    pub fn new(
        sink: &'a mut W,
        color: &ColorDescription,
        icc: Option<&[u8]>,
        srgb: Option<u8>,
        mode: ColorMode,
        depth: super::BitDepth,
    ) -> Result<Self, ImageError> {
        color.validate_interpretation()?;
        if color.associated_alpha {
            return Err(ImageError::Color(
                "PNG cannot preserve associated-alpha native samples; export TIFF".into(),
            ));
        }
        validate_profile(icc, mode)?;
        let mut prefix = Vec::new();
        let authoritative_srgb = icc.is_none() && color.cicp.is_none() && srgb.is_some();
        if let Some(n) = color.gamma {
            if n == 0 {
                return Err(ImageError::Color("zero gamma".into()));
            }
            let n = if authoritative_srgb { 45455u32 } else { n };
            chunk(&mut prefix, *b"gAMA", &n.to_be_bytes());
        }
        if let Some(points) = color.chromaticities {
            let points = if authoritative_srgb {
                [31270, 32900, 64000, 33000, 30000, 60000, 15000, 6000]
            } else {
                points
            };
            chunk(
                &mut prefix,
                *b"cHRM",
                &points
                    .into_iter()
                    .flat_map(u32::to_be_bytes)
                    .collect::<Vec<_>>(),
            );
        }
        if icc.is_none() {
            if let Some(intent) = srgb {
                if intent > 3 {
                    return Err(ImageError::Color("invalid sRGB rendering intent".into()));
                }
                chunk(&mut prefix, *b"sRGB", &[intent]);
            }
        }
        if let Some(bits) = &color.significant_bits {
            let count = if mode == ColorMode::Indexed {
                3
            } else {
                mode.samples()
            };
            let max = if mode == ColorMode::Indexed {
                8
            } else {
                depth.bits()
            };
            if bits.len() != count || bits.iter().any(|n| *n == 0 || *n > max) {
                return Err(ImageError::Color(
                    "invalid significant bits for output samples".into(),
                ));
            }
            chunk(&mut prefix, *b"sBIT", bits);
        }
        for (name, value) in [
            (*b"cICP", color.cicp.as_ref().map(|x| x.as_slice())),
            (*b"mDCV", color.mastering_display.as_deref()),
            (*b"cLLI", color.content_light.as_deref()),
        ] {
            if let Some(value) = value {
                chunk(&mut prefix, name, value);
            }
        }
        for extra in color.safe_ancillary.iter().filter(|x| !x.after_data) {
            chunk(&mut prefix, extra.name, &extra.data);
        }
        Ok(Self {
            sink,
            left: 33,
            prefix,
        })
    }
}
impl<W: Write> Write for PngSink<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = buf.len().min(self.left);
        self.sink.write_all(&buf[..n])?;
        self.left -= n;
        if self.left == 0 && !self.prefix.is_empty() {
            self.sink.write_all(&self.prefix)?;
            self.prefix.clear();
        }
        self.sink.write_all(&buf[n..])?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.sink.flush()
    }
}
pub fn write_png_trailing<W: Write>(
    writer: &mut png::Writer<W>,
    color: &ColorDescription,
) -> Result<(), ImageError> {
    for extra in color.safe_ancillary.iter().filter(|x| x.after_data) {
        writer.write_chunk(png::chunk::ChunkType(extra.name), &extra.data)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::rows::{write_rows, Canvas};
    use crate::image::{self, fixtures, Format};
    use std::io::Cursor;

    fn chunks(bytes: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
        let mut at = 8;
        let mut out = Vec::new();
        while at + 12 <= bytes.len() {
            let len = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            out.push((
                bytes[at + 4..at + 8].try_into().unwrap(),
                bytes[at + 8..at + 8 + len].to_vec(),
            ));
            at += len + 12;
        }
        out
    }
    #[test]
    fn raw_integer_color_chunks_survive_both_writers_and_keep_palette_order() {
        let mut raster = fixtures::by_name("indexed-p").raster;
        raster.color = ColorDescription {
            associated_alpha: false,
            unsupported_tiff_color: Vec::new(),
            gamma: Some(45454),
            chromaticities: Some([31270, 32900, 68000, 32000, 26500, 69000, 15000, 6000]),
            significant_bits: Some(vec![7, 6, 5]),
            cicp: Some([12, 13, 0, 1]),
            mastering_display: Some(vec![0; 24]),
            content_light: Some(vec![0; 8]),
            safe_ancillary: vec![
                AncillaryChunk {
                    name: *b"vpAg",
                    data: vec![1, 2, 3],
                    after_data: false,
                },
                AncillaryChunk {
                    name: *b"vpBg",
                    data: vec![4, 5],
                    after_data: true,
                },
            ],
        };
        let whole = image::encode(&raster, Format::Png).unwrap();
        let mut sink = Cursor::new(Vec::new());
        write_rows(
            &mut sink,
            &Canvas::of(&raster),
            Format::Png,
            &mut |y, row| {
                let at = y as usize * raster.stride();
                row.data
                    .copy_from_slice(&raster.data[at..at + raster.stride()]);
                Ok(())
            },
        )
        .unwrap();
        for bytes in [&whole, sink.get_ref()] {
            let back = image::decode(bytes).unwrap();
            assert_eq!(back.color, raster.color);
            assert_eq!(back.data, raster.data);
            assert_eq!(back.trns, raster.trns);
            let actual = chunks(bytes);
            let at = |name| actual.iter().position(|(n, _)| *n == name).unwrap();
            assert!(at(*b"cICP") < at(*b"PLTE"));
            assert!(at(*b"sBIT") < at(*b"PLTE"));
            assert!(at(*b"vpBg") > at(*b"IDAT"));
            assert_eq!(actual[at(*b"gAMA")].1, 45454u32.to_be_bytes());
        }
    }
    #[test]
    fn decoder_defaults_do_not_invent_gamma_and_edits_remove_stale_summaries() {
        let mut raster = fixtures::by_name("rgb8").raster;
        raster.srgb_intent = Some(0);
        let bytes = image::encode(&raster, Format::Png).unwrap();
        assert_eq!(image::decode(&bytes).unwrap().color.gamma, None);
        raster.color.significant_bits = Some(vec![4; 3]);
        raster.color.content_light = Some(vec![0; 8]);
        let edited = raster.color.after_edit();
        assert_eq!(edited.significant_bits, None);
        assert_eq!(edited.content_light, None);
    }
    #[test]
    fn invalid_or_wrong_space_profiles_never_reach_any_encoder() {
        let mut raster = fixtures::by_name("rgb8").raster;
        for bad in [vec![0; 128], fixtures::gray_profile()] {
            raster.icc = Some(bad);
            for format in [Format::Png, Format::Tiff] {
                assert!(image::encode(&raster, format).is_err());
                let mut sink = Cursor::new(Vec::new());
                assert!(
                    write_rows(&mut sink, &Canvas::of(&raster), format, &mut |_, _| Ok(()))
                        .is_err()
                );
                assert!(sink.get_ref().is_empty());
            }
        }
    }
    #[test]
    fn gamma_to_tiff_generates_equivalent_profile_without_changing_samples() {
        let mut raster = fixtures::by_name("rgb8").raster;
        raster.color.gamma = Some(100000);
        let bytes = image::encode(&raster, Format::Tiff).unwrap();
        let back = image::decode(&bytes).unwrap();
        assert_eq!(back.data, raster.data);
        validate_profile(back.icc.as_deref(), back.mode).unwrap();
        let source = Profile::new_icc(back.icc.as_ref().unwrap()).unwrap();
        let transform = lcms2::Transform::<[u8; 3], [u8; 3]>::new(
            &source,
            lcms2::PixelFormat::RGB_8,
            &Profile::new_srgb(),
            lcms2::PixelFormat::RGB_8,
            lcms2::Intent::RelativeColorimetric,
        )
        .unwrap();
        let mut actual = [[0; 3]];
        transform.transform_pixels(&[[128; 3]], &mut actual);
        // Independently evaluated IEC 61966-2-1 EOTF inverse for 128/255 linear.
        let expected =
            (255.0 * (1.055 * (128.0f64 / 255.0).powf(1.0 / 2.4) - 0.055)).round() as i16;
        for value in actual[0] {
            assert!((i16::from(value) - expected).abs() <= 1);
        }
    }
}
