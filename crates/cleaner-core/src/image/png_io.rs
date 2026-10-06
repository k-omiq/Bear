//! PNG, with `Transformations::IDENTITY` on both sides.
//!
//! The decoder default is already IDENTITY, which is the whole reason
//! libvips was dropped: indexed stays indexed and 16-bit stays 16-bit,
//! rather than being expanded to RGB8 at load.
//! It is set explicitly anyway, because a default that the fidelity contract
//! depends on should be visible at the place that depends on it.

use std::io::Cursor;

use png::{BitDepth as PngDepth, ColorType, Decoder, Encoder, Info, Transformations};

use super::{BitDepth, ColorMode, Format, Header, ImageError, Raster};

impl From<png::DecodingError> for ImageError {
    fn from(e: png::DecodingError) -> Self {
        ImageError::Png(e.to_string())
    }
}

impl From<png::EncodingError> for ImageError {
    fn from(e: png::EncodingError) -> Self {
        ImageError::Png(e.to_string())
    }
}

fn mode_of(color_type: ColorType) -> Option<ColorMode> {
    Some(match color_type {
        ColorType::Grayscale => ColorMode::Gray,
        ColorType::GrayscaleAlpha => ColorMode::GrayAlpha,
        ColorType::Rgb => ColorMode::Rgb,
        ColorType::Rgba => ColorMode::Rgba,
        ColorType::Indexed => ColorMode::Indexed,
    })
}

fn color_type_of(mode: ColorMode) -> Option<ColorType> {
    Some(match mode {
        ColorMode::Gray => ColorType::Grayscale,
        ColorMode::GrayAlpha => ColorType::GrayscaleAlpha,
        ColorMode::Rgb => ColorType::Rgb,
        ColorMode::Rgba => ColorType::Rgba,
        ColorMode::Indexed => ColorType::Indexed,
        ColorMode::Cmyk => return None,
    })
}

fn depth_of(depth: PngDepth) -> BitDepth {
    match depth {
        PngDepth::One => BitDepth::One,
        PngDepth::Two => BitDepth::Two,
        PngDepth::Four => BitDepth::Four,
        PngDepth::Eight => BitDepth::Eight,
        PngDepth::Sixteen => BitDepth::Sixteen,
    }
}

fn png_depth_of(depth: BitDepth) -> PngDepth {
    match depth {
        BitDepth::One => PngDepth::One,
        BitDepth::Two => PngDepth::Two,
        BitDepth::Four => PngDepth::Four,
        BitDepth::Eight => PngDepth::Eight,
        BitDepth::Sixteen => PngDepth::Sixteen,
    }
}

/// `IHDR` and the ancillary chunks before `IDAT`, without touching a pixel.
pub fn header(bytes: &[u8]) -> Result<Header, ImageError> {
    let mut decoder = Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(Transformations::IDENTITY);
    let reader = decoder.read_info()?;
    let info = reader.info();
    Ok(Header { palette: info.palette.as_ref().map(|p| p.to_vec()),
        width: info.width,
        height: info.height,
        mode: mode_of(info.color_type).ok_or(ImageError::Png("unsupported colour type".into()))?,
        depth: depth_of(info.bit_depth),
        icc: info.icc_profile.as_ref().map(|p| p.to_vec()),
        color: super::ColorDescription::from_png(bytes)?,
        srgb_intent: info.srgb.map(|intent| intent as u8),
        trns: info.trns.as_ref().map(|t| trns_as_stored(mode_of(info.color_type).expect("validated PNG color type"), t)),
    })
}

pub fn decode(bytes: &[u8]) -> Result<Raster, ImageError> {
    let mut decoder = Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(Transformations::IDENTITY);
    let mut reader = decoder.read_info()?;

    let mut data = vec![0; reader.output_buffer_size().unwrap_or(0)];
    let frame = reader.next_frame(&mut data)?;
    // `next_frame` reports the bytes it actually wrote, which is smaller than
    // the buffer for a sub-byte-depth image whose rows pad.
    data.truncate(frame.buffer_size());

    let info = reader.info();
    let mode = mode_of(info.color_type).ok_or(ImageError::Png("unsupported colour type".into()))?;

    Ok(Raster {
        width: info.width,
        height: info.height,
        mode,
        depth: depth_of(info.bit_depth),
        icc: info.icc_profile.as_ref().map(|p| p.to_vec()),
        palette: info.palette.as_ref().map(|p| p.to_vec()),
        trns: info.trns.as_ref().map(|t| trns_as_stored(mode, t)),
        srgb_intent: info.srgb.map(|intent| intent as u8),
        color: super::ColorDescription::from_png(bytes)?,
        data,
    })
}

/// `tRNS` in the chunk's own layout, which is what [`Raster::trns`] holds and
/// [`encode`] writes back. Below 16 bits the decoder hands a Gray or RGB key
/// over as one byte a channel, the low byte of each sample; the chunk stores
/// two, big-endian, whose high byte is then 0. Written back in the decoder's
/// form the chunk was too short to read, and the next decode dropped the key.
/// A palette's `tRNS`, one byte an entry, is the same in both.
fn trns_as_stored(mode: ColorMode, decoded: &[u8]) -> Vec<u8> {
    let channels = match mode {
        ColorMode::Gray => 1,
        ColorMode::Rgb => 3,
        _ => return decoded.to_vec(),
    };
    if decoded.len() == channels {
        decoded.iter().flat_map(|&low| [0, low]).collect()
    } else {
        decoded.to_vec()
    }
}

pub fn encode(raster: &Raster) -> Result<Vec<u8>, ImageError> {
    encode_with(raster, None)
}

/// [`encode`] at another DEFLATE effort. Every setting is lossless; only the
/// time spent and the size of the file differ. `None` leaves the encoder's own
/// defaults, which is what [`encode`] has always written.
pub fn encode_with(raster: &Raster, compression: Option<png::Compression>) -> Result<Vec<u8>, ImageError> {
    let color_type = color_type_of(raster.mode).ok_or(ImageError::Unrepresentable {
        format: Format::Png,
        mode: raster.mode,
        depth: raster.depth,
    })?;

    let mut info = Info::with_size(raster.width, raster.height);
    info.color_type = color_type;
    info.bit_depth = png_depth_of(raster.depth);
    info.palette = raster.palette.clone().map(Into::into);
    info.trns = raster.trns.clone().map(Into::into);
    info.icc_profile = raster.icc.clone().map(Into::into);

    let mut out = Vec::new();
    {
        let sink = super::metadata::PngSink::new(&mut out, &raster.color, raster.icc.as_deref(), raster.srgb_intent, raster.mode, raster.depth)?;
        let mut encoder = Encoder::with_info(sink, info)?;
        if let Some(compression) = compression { encoder.set_compression(compression); }
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&raster.data)?;
        super::metadata::write_png_trailing(&mut writer, &raster.color)?;
        writer.finish()?;
    }
    Ok(out)
}
