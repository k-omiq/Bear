//! The incremental encoders rule 8 asks for.
//!
//! Rule 8 is one sentence long about the mechanism:
//!
//! > **Stitched**: incremental row-wise encoder or tiled TIFF.
//!
//! This module is both halves of it. A caller supplies a [`Canvas`], which
//! is everything a file needs about an image *except* its pixels, and a function
//! that fills one row at a time. Nothing here ever holds the image.
//!
//! That matters for the stitched case, where the image is the whole strip and
//! rule 1 forbids ever having it:
//!
//! > Nothing concatenates pixels, at any point, including export.
//!
//! It is used for the per-page case too, and there the win is smaller but real:
//! [`crate::export::export_page`] builds the encoded file as a `Vec<u8>` which
//! the caller then writes, so the file exists twice; the streaming form encodes
//! straight into the sink.
//!
//! ## Why this is not in `crate::image`
//!
//! `image::encode` takes a whole [`Raster`] and returns a whole `Vec<u8>`, which
//! is the right shape for a page and the wrong one for a strip. A row-wise
//! encoder is a different interface rather than an option on that one, and it is
//! the export path's own need. `crate::image` was also being edited in parallel
//! when this landed; the two PNG paths share the `iCCP`/`sRGB` ordering rule
//! and nothing else, and if they are ever unified it should be by moving
//! `image::encode` onto this rather than the other way round.

use std::io::{Seek, Write};

use png::{ColorType, Encoder, Info};
use tiff::encoder::{TiffEncoder, colortype};
use tiff::tags::Tag;

use crate::image::{BitDepth, ColorMode, Format, ImageError, Raster};

use super::ExportError;

/// TIFF tag 34675, `ICC Profile`. Not in the crate's `Tag` enum, and stated
/// here for the same reason `image::tiff_io` states it: a profile written under
/// the wrong tag is a profile silently lost.
const TAG_ICC_PROFILE: Tag = Tag::Unknown(34675);

/// Rows per TIFF strip.
///
/// A strip is the unit TIFF can be written incrementally in, so this is the
/// number of rows that are resident at once on that path. Bounded rather than
/// tuned, and deliberately independent of the image: one strip of a 12 000-row
/// strip costs the same as one strip of a page. Nothing was measured to pick
/// it, and nothing should be read into the value beyond "small, and a whole
/// number of rows".
pub const TIFF_STRIP_ROWS: u32 = 64;

/// Everything a file needs about an image except its pixels.
///
/// The point of the type: an encoder that takes rows needs the header *before*
/// the first row, and taking a [`Raster`] for that would mean having the image
/// - which is exactly what the stitched path must not do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    pub mode: ColorMode,
    pub depth: BitDepth,
    pub icc: Option<Vec<u8>>,
    pub palette: Option<Vec<u8>>,
    pub trns: Option<Vec<u8>>,
    pub srgb_intent: Option<u8>,
    pub color: crate::image::ColorDescription,
}

impl Canvas {
    /// The canvas a raster would be written on. Metadata only; the samples are
    /// not read.
    pub fn of(raster: &Raster) -> Canvas {
        Canvas {
            width: raster.width,
            height: raster.height,
            mode: raster.mode,
            depth: raster.depth,
            icc: raster.icc.clone(),
            palette: raster.palette.clone(),
            trns: raster.trns.clone(),
            srgb_intent: raster.srgb_intent,
            color: raster.color.clone(),
        }
    }

    /// Bytes per row, including sub-byte padding to a byte boundary - the same
    /// arithmetic as [`Raster::stride`], because a row handed to an encoder has
    /// to be laid out exactly as a raster's row is.
    pub fn stride(&self) -> usize {
        let bits = self.width as usize * self.mode.samples() * self.depth.bits() as usize;
        bits.div_ceil(8)
    }

    /// An empty raster of one row, as somewhere to address samples by
    /// coordinate rather than by bit offset.
    ///
    /// Sub-byte depths are why this exists: a 4-bit page placed at an odd x
    /// offset in the strip starts mid-byte, so a row cannot be assembled with
    /// `copy_from_slice` at all. Going through
    /// [`Raster::set_sample`](crate::image::Raster::set_sample) is the only
    /// form that is correct for every depth, and correctness at the seam is the
    /// whole subject of rule 8.
    pub fn row_buffer(&self) -> Raster {
        Raster {
            width: self.width,
            height: 1,
            mode: self.mode,
            depth: self.depth,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None,
            color: Default::default(),
            data: vec![0; self.stride()],
        }
    }
}

/// Fills row `y` of the image, in the canvas's own layout.
///
/// Called exactly once per row, in order, so an implementation may hold a
/// cursor - which is what makes the stitched writer able to keep one page
/// resident and drop it when the rows move past.
pub type RowFn<'a> = &'a mut dyn FnMut(u32, &mut Raster) -> Result<(), ExportError>;

/// Encode an image one row at a time, straight into `sink`.
///
/// `Seek` is required because TIFF writes its directory after the data and
/// patches the offsets back in. A `File` and a `Cursor` both satisfy it, which
/// is every sink this codebase has.
pub fn write_rows<W: Write + Seek>(
    sink: &mut W,
    canvas: &Canvas,
    format: Format,
    row: RowFn<'_>,
) -> Result<(), ExportError> {
    match format {
        Format::Png => write_png_rows(sink, canvas, row),
        Format::Tiff => write_tiff_rows(sink, canvas, row),
        Format::Jpeg => Err(ImageError::Unrepresentable {
            format,
            mode: canvas.mode,
            depth: canvas.depth,
        }
        .into()),
    }
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

fn png_depth_of(depth: BitDepth) -> png::BitDepth {
    match depth {
        BitDepth::One => png::BitDepth::One,
        BitDepth::Two => png::BitDepth::Two,
        BitDepth::Four => png::BitDepth::Four,
        BitDepth::Eight => png::BitDepth::Eight,
        BitDepth::Sixteen => png::BitDepth::Sixteen,
    }
}

fn write_png_rows<W: Write + Seek>(
    sink: &mut W,
    canvas: &Canvas,
    row: RowFn<'_>,
) -> Result<(), ExportError> {
    let color_type = color_type_of(canvas.mode).ok_or(ImageError::Unrepresentable {
        format: Format::Png,
        mode: canvas.mode,
        depth: canvas.depth,
    })?;

    let mut info = Info::with_size(canvas.width, canvas.height);
    info.color_type = color_type;
    info.bit_depth = png_depth_of(canvas.depth);
    info.palette = canvas.palette.clone().map(Into::into);
    info.trns = canvas.trns.clone().map(Into::into);
    info.icc_profile = canvas.icc.clone().map(Into::into);

    let sink = crate::image::metadata::PngSink::new(sink, &canvas.color, canvas.icc.as_deref(), canvas.srgb_intent, canvas.mode, canvas.depth)?;
    let encoder = Encoder::with_info(sink, info).map_err(ImageError::from)?;
    let mut writer = encoder.write_header().map_err(ImageError::from)?;

    {
        let mut stream = writer.stream_writer().map_err(ImageError::from)?;
        let mut buffer = canvas.row_buffer();
        for y in 0..canvas.height {
            row(y, &mut buffer)?;
            stream.write_all(&buffer.data).map_err(|e| ImageError::Png(e.to_string()))?;
        }
        stream.finish().map_err(ImageError::from)?;
    }
    crate::image::metadata::write_png_trailing(&mut writer, &canvas.color)?;
    writer.finish().map_err(ImageError::from)?;
    Ok(())
}

fn write_tiff_rows<W: Write + Seek>(
    sink: &mut W,
    canvas: &Canvas,
    row: RowFn<'_>,
) -> Result<(), ExportError> {
    let mut canvas = canvas.clone();
    canvas.icc = crate::image::metadata::equivalent_icc(&canvas.color, canvas.icc.as_deref(), canvas.srgb_intent, canvas.mode)?;
    if canvas.trns.is_some() { return Err(ImageError::Color("TIFF cannot retain PNG color-key transparency; export PNG".into()).into()); }
    let canvas = &canvas;
    let mut encoder = TiffEncoder::new(sink).map_err(ImageError::from)?;
    match (canvas.mode, canvas.depth) {
        (ColorMode::Gray, BitDepth::Eight) => strips::<W, colortype::Gray8>(&mut encoder, canvas, row),
        (ColorMode::Gray, BitDepth::Sixteen) => strips::<W, colortype::Gray16>(&mut encoder, canvas, row),
        (ColorMode::GrayAlpha, BitDepth::Eight) => strips::<W, colortype::Gray8>(&mut encoder, canvas, row),
        (ColorMode::GrayAlpha, BitDepth::Sixteen) => strips::<W, colortype::Gray16>(&mut encoder, canvas, row),
        (ColorMode::Rgb, BitDepth::Eight) => strips::<W, colortype::RGB8>(&mut encoder, canvas, row),
        (ColorMode::Rgb, BitDepth::Sixteen) => strips::<W, colortype::RGB16>(&mut encoder, canvas, row),
        (ColorMode::Rgba, BitDepth::Eight) => strips::<W, colortype::RGBA8>(&mut encoder, canvas, row),
        (ColorMode::Rgba, BitDepth::Sixteen) => strips::<W, colortype::RGBA16>(&mut encoder, canvas, row),
        (ColorMode::Cmyk, BitDepth::Eight) => strips::<W, colortype::CMYK8>(&mut encoder, canvas, row),
        (ColorMode::Cmyk, BitDepth::Sixteen) => strips::<W, colortype::CMYK16>(&mut encoder, canvas, row),
        // The same list `image::tiff_io::encode` refuses: the crate has no
        // `RGBPalette` and no `GrayA` encoder colour type, and all bit depths
        // must be equal. Declared unrepresentable rather than promoted.
        (mode, depth) => {
            Err(ImageError::Unrepresentable { format: Format::Tiff, mode, depth }.into())
        }
    }
}

/// One strip at a time, so the resident cost is [`TIFF_STRIP_ROWS`] rows rather
/// than the image.
fn strips<W: Write + Seek, C: colortype::ColorType>(
    encoder: &mut TiffEncoder<&mut W>,
    canvas: &Canvas,
    row: RowFn<'_>,
) -> Result<(), ExportError>
where
    [C::Inner]: tiff::encoder::TiffValue,
    C::Inner: FromBeBytes,
{
    let mut image = encoder
        .new_image::<C>(canvas.width, canvas.height)
        .map_err(ImageError::from)?;
    if canvas.mode == ColorMode::GrayAlpha {
        image.extra_samples(&[if canvas.color.associated_alpha { tiff::tags::ExtraSamples::AssociatedAlpha } else { tiff::tags::ExtraSamples::UnassociatedAlpha }]).map_err(ImageError::from)?;
    } else if canvas.mode == ColorMode::Rgba {
        image.encoder().write_tag(Tag::ExtraSamples, &[if canvas.color.associated_alpha {1u16} else {2u16}][..]).map_err(ImageError::from)?;
    }
    image.rows_per_strip(TIFF_STRIP_ROWS).map_err(ImageError::from)?;
    if let Some(icc) = &canvas.icc {
        image.encoder().write_tag(TAG_ICC_PROFILE, icc.as_slice()).map_err(ImageError::from)?;
    }

    let stride = canvas.stride();
    let mut buffer = canvas.row_buffer();
    let mut strip: Vec<u8> = Vec::with_capacity(stride * TIFF_STRIP_ROWS as usize);
    let mut y = 0;
    while y < canvas.height {
        let rows = TIFF_STRIP_ROWS.min(canvas.height - y);
        strip.clear();
        for line in y..y + rows {
            row(line, &mut buffer)?;
            strip.extend_from_slice(&buffer.data);
        }
        image.write_strip(&C::Inner::from_be_bytes(&strip)).map_err(ImageError::from)?;
        y += rows;
    }
    image.finish().map_err(ImageError::from)?;
    Ok(())
}

/// The raster's big-endian byte buffer as the `tiff` crate's native sample
/// type. Only `u8` and `u16` are reachable from the match above.
pub trait FromBeBytes: Sized {
    fn from_be_bytes(bytes: &[u8]) -> Vec<Self>;
}

impl FromBeBytes for u8 {
    fn from_be_bytes(bytes: &[u8]) -> Vec<u8> {
        bytes.to_vec()
    }
}

impl FromBeBytes for u16 {
    fn from_be_bytes(bytes: &[u8]) -> Vec<u16> {
        bytes.chunks_exact(2).map(|pair| u16::from_be_bytes([pair[0], pair[1]])).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{decode, encode, fixtures, lossless_format_for};
    use std::io::Cursor;

    /// Row by row and all at once produce the same file, on every fixture.
    ///
    /// This is what lets the streaming path inherit the whole of the round-trip
    /// test in `image::tests` rather than restate it: if the bytes are equal,
    /// every property asserted of one holds of the other. The interesting
    /// fixtures are the sub-byte ones, where a row is not a whole number of
    /// samples.
    #[test]
    fn streaming_a_raster_row_by_row_writes_the_same_file_as_encoding_it_whole() {
        for fixture in fixtures::all() {
            let raster = &fixture.raster;
            let format = lossless_format_for(raster);
            let whole = encode(raster, format).unwrap();

            let mut streamed = Cursor::new(Vec::new());
            write_rows(&mut streamed, &Canvas::of(raster), format, &mut |y, row| {
                let stride = raster.stride();
                let at = y as usize * stride;
                row.data.copy_from_slice(&raster.data[at..at + stride]);
                Ok(())
            })
            .unwrap_or_else(|e| panic!("{}: {e}", fixture.name));

            let streamed = streamed.into_inner();
            let a = decode(&whole).unwrap();
            let b = decode(&streamed).unwrap();
            assert_eq!(b.mode, a.mode, "{}: mode", fixture.name);
            assert_eq!(b.depth, a.depth, "{}: depth", fixture.name);
            assert_eq!(b.icc, a.icc, "{}: icc", fixture.name);
            assert_eq!(b.palette, a.palette, "{}: palette", fixture.name);
            assert_eq!(b.data, a.data, "{}: samples", fixture.name);
        }
    }

    /// The `iCCP`/`sRGB` trap, on the streaming path too. `image::png_io` has
    /// this test; a second encoder is a second place to lose the profile.
    #[test]
    fn a_streamed_png_uses_authoritative_icc() {
        let mut raster = fixtures::by_name("rgb8-icc").raster;
        raster.srgb_intent = Some(0);

        let mut out = Cursor::new(Vec::new());
        write_rows(&mut out, &Canvas::of(&raster), Format::Png, &mut |y, row| {
            let stride = raster.stride();
            row.data.copy_from_slice(&raster.data[y as usize * stride..][..stride]);
            Ok(())
        })
        .unwrap();

        let back = decode(&out.into_inner()).unwrap();
        assert_eq!(back.icc, raster.icc, "the profile was dropped");
        assert_eq!(back.srgb_intent, None, "conflicting sRGB must be removed");
    }

    /// An image taller than one strip writes every strip, including a last one
    /// that is shorter than [`TIFF_STRIP_ROWS`]. 48 rows is under one strip;
    /// this stacks the fixture to run past it.
    #[test]
    fn a_tiff_taller_than_one_strip_writes_every_row() {
        let source = fixtures::by_name("cmyk8").raster;
        let repeats = 3;
        let canvas = Canvas { height: source.height * repeats, ..Canvas::of(&source) };
        assert!(canvas.height > TIFF_STRIP_ROWS, "the fixture would fit in one strip");
        assert_ne!(canvas.height % TIFF_STRIP_ROWS, 0, "the last strip would be full");

        let mut out = Cursor::new(Vec::new());
        write_rows(&mut out, &canvas, Format::Tiff, &mut |y, row| {
            let stride = source.stride();
            let line = (y % source.height) as usize;
            row.data.copy_from_slice(&source.data[line * stride..][..stride]);
            Ok(())
        })
        .unwrap();

        let back = decode(&out.into_inner()).unwrap();
        assert_eq!(back.height, canvas.height);
        assert_eq!(back.mode, ColorMode::Cmyk);
        for repeat in 0..repeats {
            let at = (repeat * source.height) as usize * source.stride();
            assert_eq!(
                &back.data[at..at + source.data.len()],
                &source.data[..],
                "copy {repeat} of the fixture came back different"
            );
        }
    }
}
