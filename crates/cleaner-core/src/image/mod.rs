//! Decoding and encoding, with the source's colour mode, bit depth and ICC
//! profile carried through untouched.
//!
//! The fidelity contract this module exists to make enforceable:
//!
//! > Outside the union of all applied masks grown by `edit_margin`, the exported
//! > file's decoded pixels are bit-identical to the source file's decoded
//! > pixels, and the file's colour mode, bit depth and embedded ICC profile are
//! > unchanged from the source.
//!
//! So a [`Raster`] holds samples **exactly as the decoder produced them** -
//! packed sub-byte rows for indexed and bitonal sources, big-endian pairs for
//! 16-bit - and never a normalised internal form. Anything that needs pixels as
//! numbers converts on the way in and writes back through a mask; nothing
//! rewrites the buffer wholesale.

use std::io::Cursor;

mod convert;
pub mod color;
pub mod exif;
mod png_io;
mod tiff_io;

pub mod metadata;
pub use metadata::ColorDescription;
pub mod fixtures;
pub mod foreign;
pub mod orientation;
pub mod import_info;
pub mod proxy;

/// How samples are laid out. Not a superset of anything: these are the
/// modes named, and a mode this list does not have is rejected at ingest
/// rather than promoted.
///
/// The serialised names are the ones the format matrix uses, so a
/// `.mtclean` manifest opened in a text editor reads in the vocabulary of
/// the document that governs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ColorMode {
    #[serde(rename = "L")]
    Gray,
    #[serde(rename = "LA")]
    GrayAlpha,
    #[serde(rename = "RGB")]
    Rgb,
    #[serde(rename = "RGBA")]
    Rgba,
    /// Palette indices. The palette itself lives in [`Raster::palette`].
    #[serde(rename = "P")]
    Indexed,
    #[serde(rename = "CMYK")]
    Cmyk,
}

impl ColorMode {
    /// Samples per pixel. Indexed is one sample - the index.
    pub fn samples(self) -> usize {
        match self {
            ColorMode::Gray | ColorMode::Indexed => 1,
            ColorMode::GrayAlpha => 2,
            ColorMode::Rgb => 3,
            ColorMode::Rgba | ColorMode::Cmyk => 4,
        }
    }

    /// Whether an engine above rung 0 may write into this mode at all.
    /// Indexed cannot represent continuous tone, and CMYK→RGB→CMYK is not
    /// invertible.
    pub fn allows_model_engines(self) -> bool {
        !matches!(self, ColorMode::Indexed | ColorMode::Cmyk)
    }

    /// Which sample carries alpha, if any.
    ///
    /// Asked often enough to be worth stating once: every engine copies alpha
    /// through rather than producing it, because the fidelity contract
    /// keeps alpha out of every statistic - so a rung that filled
    /// or smoothed it would be inventing transparency from a colour
    /// measurement.
    pub fn alpha_channel(self) -> Option<usize> {
        match self {
            ColorMode::GrayAlpha => Some(1),
            ColorMode::Rgba => Some(3),
            _ => None,
        }
    }
}

/// Bits per sample. Sub-byte depths exist for indexed and bitonal sources, and
/// bitonal is why the model rungs decline a 1-bit source.
///
/// Serialised as the number of bits rather than as a variant name: `bit_depth`
/// in a `.mtclean` manifest is a quantity, and `16` is what a reader expects
/// to find there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub enum BitDepth {
    One,
    Two,
    Four,
    Eight,
    Sixteen,
}

impl BitDepth {
    pub fn bits(self) -> u8 {
        match self {
            BitDepth::One => 1,
            BitDepth::Two => 2,
            BitDepth::Four => 4,
            BitDepth::Eight => 8,
            BitDepth::Sixteen => 16,
        }
    }

    /// The inverse of [`BitDepth::bits`]. A depth this list does not have is
    /// refused rather than rounded - the same rule `ColorMode` follows, for the
    /// same reason.
    pub fn from_bits(bits: u8) -> Option<BitDepth> {
        Some(match bits {
            1 => BitDepth::One,
            2 => BitDepth::Two,
            4 => BitDepth::Four,
            8 => BitDepth::Eight,
            16 => BitDepth::Sixteen,
            _ => return None,
        })
    }
}

impl From<BitDepth> for u8 {
    fn from(depth: BitDepth) -> u8 {
        depth.bits()
    }
}

impl TryFrom<u8> for BitDepth {
    type Error = String;

    fn try_from(bits: u8) -> Result<BitDepth, String> {
        BitDepth::from_bits(bits).ok_or_else(|| format!("{bits} is not a bit depth this reads"))
    }
}

/// A decoded image, plus everything about the file that has to survive a
/// round-trip.
#[derive(Debug, Clone)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub mode: ColorMode,
    pub depth: BitDepth,
    /// The embedded profile, as opaque bytes. Never parsed on the fidelity path -
    /// `icc_bytes` is carried from ingest to write, and only the managed
    /// conversions ever look inside.
    pub icc: Option<Vec<u8>>,
    /// `PLTE`, three bytes per entry. Present only for [`ColorMode::Indexed`].
    pub palette: Option<Vec<u8>>,
    /// `tRNS` as the file stores it: one byte per palette entry for
    /// [`ColorMode::Indexed`], and for [`ColorMode::Gray`] and
    /// [`ColorMode::Rgb`] the colour key, one big-endian 16-bit sample per
    /// channel ([`Raster::colour_key`]) at every depth.
    pub trns: Option<Vec<u8>>,
    /// The `sRGB` rendering intent, if the source declared one. Carried
    /// separately because the `png` encoder writes `iCCP` **only** when this is
    /// absent, so a source with both chunks would silently lose its profile.
    pub srgb_intent: Option<u8>,
    pub color: ColorDescription,
    /// Samples in the decoder's own layout: row-major, packed for sub-byte
    /// depths, big-endian for 16-bit.
    pub data: Vec<u8>,
}

impl Raster {
    /// The colour a Gray or RGB image's `tRNS` chunk makes fully transparent,
    /// one sample per channel at the raster's own depth: every pixel of
    /// exactly this colour is see-through though the mode has no alpha
    /// channel. `None` for any other mode, or without the chunk.
    pub fn colour_key(&self) -> Option<Vec<u16>> {
        let channels = match self.mode {
            ColorMode::Gray => 1,
            ColorMode::Rgb => 3,
            _ => return None,
        };
        let trns = self.trns.as_ref().filter(|trns| trns.len() == 2 * channels)?;
        Some(trns.chunks_exact(2).map(|pair| u16::from_be_bytes([pair[0], pair[1]])).collect())
    }

    /// Bytes per row, including sub-byte padding to a byte boundary.
    pub fn stride(&self) -> usize {
        let bits = self.width as usize * self.mode.samples() * self.depth.bits() as usize;
        bits.div_ceil(8)
    }

    /// One sample, as it is stored: an index for [`ColorMode::Indexed`], a
    /// level otherwise, never rescaled to a common range. Rescaling is what
    /// makes a 16-bit source quietly become an 8-bit one.
    pub fn sample(&self, x: u32, y: u32, channel: usize) -> u16 {
        let bits = self.depth.bits() as usize;
        let index = (x as usize * self.mode.samples()) + channel;
        let row = y as usize * self.stride();
        match bits {
            16 => {
                let at = row + index * 2;
                u16::from_be_bytes([self.data[at], self.data[at + 1]])
            }
            8 => self.data[row + index] as u16,
            _ => {
                // Packed, most significant bits first, as PNG stores them.
                let per_byte = 8 / bits;
                let byte = self.data[row + index / per_byte];
                let shift = 8 - bits * (index % per_byte + 1);
                ((byte >> shift) as u16) & ((1u16 << bits) - 1)
            }
        }
    }

    /// Write one sample back in the same layout.
    pub fn set_sample(&mut self, x: u32, y: u32, channel: usize, value: u16) {
        let bits = self.depth.bits() as usize;
        let index = (x as usize * self.mode.samples()) + channel;
        let row = y as usize * self.stride();
        match bits {
            16 => {
                let at = row + index * 2;
                self.data[at..at + 2].copy_from_slice(&value.to_be_bytes());
            }
            8 => self.data[row + index] = value as u8,
            _ => {
                let per_byte = 8 / bits;
                let at = row + index / per_byte;
                let shift = 8 - bits * (index % per_byte + 1);
                let mask = (((1u16 << bits) - 1) as u8) << shift;
                self.data[at] = (self.data[at] & !mask) | (((value as u8) << shift) & mask);
            }
        }
    }

    /// A run of whole rows, as a raster of its own.
    ///
    /// Rows and not a rectangle, and that is the reason this is cheap enough to
    /// exist: a row range is a contiguous slice of `data` at every depth,
    /// including the sub-byte ones where an arbitrary `x` would land mid-byte
    /// and need repacking. It is also the only crop rule 3 asks for - a
    /// detection segment is a range of strip **rows**.
    ///
    /// Everything but the height and the samples travels unchanged, so the
    /// crop's mode, depth, palette and `tRNS` are the page's and a mask fitted
    /// on it means the same thing.
    pub fn rows(&self, y0: u32, y1: u32) -> Raster {
        let y0 = y0.min(self.height);
        let y1 = y1.clamp(y0, self.height);
        let stride = self.stride();
        Raster {
            width: self.width,
            height: y1 - y0,
            mode: self.mode,
            depth: self.depth,
            icc: self.icc.clone(),
            palette: self.palette.clone(),
            trns: self.trns.clone(),
            srgb_intent: self.srgb_intent,
            color: self.color.clone(),
            data: self.data[y0 as usize * stride..y1 as usize * stride].to_vec(),
        }
    }

    /// Whether two rasters describe the same image, ignoring metadata. Used by
    /// the fidelity test to locate *which* pixels moved.
    pub fn same_geometry(&self, other: &Raster) -> bool {
        self.width == other.width
            && self.height == other.height
            && self.mode == other.mode
            && self.depth == other.depth
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Png,
    Tiff,
    /// Accepted verbatim at import. Edited pixels are written losslessly.
    Jpeg,
}

impl Format {
    /// Sniffed from the leading bytes rather than from the extension: a file's
    /// name is a claim and its magic is evidence.
    pub fn sniff(bytes: &[u8]) -> Option<Format> {
        if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
            Some(Format::Png)
        } else if bytes.starts_with(b"II\x2a\x00") || bytes.starts_with(b"MM\x00\x2a") {
            Some(Format::Tiff)
        } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            Some(Format::Jpeg)
        } else {
            None
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    #[error("unrecognised image format")]
    UnknownFormat,
    #[error("{format:?} cannot carry a {mode:?} image at {depth:?}")]
    Unrepresentable { format: Format, mode: ColorMode, depth: BitDepth },
    #[error("png: {0}")]
    Png(String),
    #[error("color metadata: {0}")]
    Color(String),
    #[error("{0}")]
    Foreign(String),
    #[error("tiff: {0}")]
    Tiff(String),
}

/// What a header alone says. `SourceRef` is built from this, so listing a
/// 200-page folder never decodes a pixel.
#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    pub palette: Option<Vec<u8>>,
    pub width: u32,
    pub height: u32,
    pub mode: ColorMode,
    pub depth: BitDepth,
    pub icc: Option<Vec<u8>>,
    pub color: ColorDescription,
    pub srgb_intent: Option<u8>,
    pub trns: Option<Vec<u8>>,
}

pub fn png_header(bytes: &[u8]) -> Result<Header, ImageError> {
    png_io::header(bytes)
}

pub fn tiff_header(bytes: &[u8]) -> Result<Header, ImageError> {
    tiff_io::header(Cursor::new(bytes))
}

pub fn header(bytes: &[u8]) -> Result<Header, ImageError> {
    match Format::sniff(bytes).ok_or(ImageError::UnknownFormat)? {
        Format::Png => png_header(bytes),
        Format::Tiff => tiff_header(bytes),
        Format::Jpeg => foreign::jpeg_header(bytes),
    }
}

pub fn decode(bytes: &[u8]) -> Result<Raster, ImageError> {
    match Format::sniff(bytes).ok_or(ImageError::UnknownFormat)? {
        Format::Png => png_io::decode(bytes),
        Format::Tiff => tiff_io::decode(Cursor::new(bytes)),
        Format::Jpeg => foreign::decode(bytes),
    }
}

pub fn encode(raster: &Raster, format: Format) -> Result<Vec<u8>, ImageError> {
    match format {
        Format::Png => png_io::encode(raster),
        Format::Tiff => tiff_io::encode(raster),
        Format::Jpeg => Err(ImageError::Unrepresentable {
            format,
            mode: raster.mode,
            depth: raster.depth,
        }),
    }
}

/// PNG at the encoder's fast setting, for pixels that are drawn once and never
/// kept - the `tile://` previews. Still lossless: the samples are exactly what
/// [`encode`] writes, in a somewhat larger file that takes a fraction of the
/// time to produce.
pub fn encode_png_preview(raster: &Raster) -> Result<Vec<u8>, ImageError> {
    png_io::encode_with(raster, Some(png::Compression::Fast))
}

/// The format that can carry this raster with nothing declared away. PNG for
/// everything it can represent; TIFF for CMYK, which PNG has no colour type
/// for.
pub fn lossless_format_for(raster: &Raster) -> Format {
    if raster.color.associated_alpha {return Format::Tiff;}
    match raster.mode {
        ColorMode::Cmyk => Format::Tiff,
        _ => Format::Png,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Phase 0 spike 4, and the first half of the
    /// fidelity test: every fixture survives a round-trip with its
    /// mode, depth and profile intact. The second half - the mask-subset
    /// assertion under a non-empty patch set - needs the compositor and lives
    /// with it.
    #[test]
    fn every_fixture_round_trips_with_mode_depth_and_icc_intact() {
        for fixture in fixtures::all() {
            let source = &fixture.raster;
            let format = lossless_format_for(source);
            let encoded = encode(source, format)
                .unwrap_or_else(|e| panic!("{}: encode failed: {e}", fixture.name));
            let decoded = decode(&encoded)
                .unwrap_or_else(|e| panic!("{}: decode failed: {e}", fixture.name));

            assert_eq!(decoded.mode, source.mode, "{}: mode", fixture.name);
            assert_eq!(decoded.depth, source.depth, "{}: bit depth", fixture.name);
            assert_eq!(decoded.icc, source.icc, "{}: icc profile", fixture.name);
            assert_eq!(decoded.width, source.width, "{}: width", fixture.name);
            assert_eq!(decoded.height, source.height, "{}: height", fixture.name);
            assert_eq!(decoded.palette, source.palette, "{}: palette", fixture.name);
            assert_eq!(decoded.trns, source.trns, "{}: trns", fixture.name);
            assert_eq!(decoded.data, source.data, "{}: samples", fixture.name);
        }
    }

    /// **A colour key survives being written again.** A Gray or RGB page's
    /// `tRNS` key is kept in the chunk's own layout, two bytes a channel, so
    /// the page decoded, encoded and decoded again still names the same
    /// transparent colour at 8 and 16 bits. The decoder's own form keeps one
    /// byte a channel below 16 bits; written back as it came, that was a
    /// chunk too short to read, and the second decode dropped it. A palette's
    /// `tRNS`, one byte an entry, is unaffected.
    #[test]
    fn a_colour_key_survives_a_second_round_trip() {
        let keys: [(&str, Vec<u8>, Vec<u16>); 4] = [
            ("l8", vec![0, 130], vec![130]),
            ("l16", vec![0x12, 0x34], vec![0x1234]),
            ("rgb8", vec![0, 1, 0, 2, 0, 3], vec![1, 2, 3]),
            ("rgb16", vec![0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc], vec![0x1234, 0x5678, 0x9abc]),
        ];
        for (name, key, colour) in keys {
            let mut source = fixtures::by_name(name).raster;
            source.trns = Some(key.clone());
            let once = decode(&encode(&source, Format::Png).unwrap()).unwrap();
            let twice = decode(&encode(&once, Format::Png).unwrap()).unwrap();
            for (pass, decoded) in [("once", &once), ("twice", &twice)] {
                assert_eq!(decoded.trns.as_deref(), Some(&key[..]), "{name} {pass}: the key as the file stores it");
                assert_eq!(decoded.colour_key(), Some(colour.clone()), "{name} {pass}");
                assert_eq!(decoded.data, source.data, "{name} {pass}: samples");
            }
        }

        let mut indexed = fixtures::by_name("indexed-p").raster;
        let entries = indexed.palette.as_ref().unwrap().len() / 3;
        indexed.trns = Some((0..entries).map(|entry| (entry * 37 % 256) as u8).collect());
        let once = decode(&encode(&indexed, Format::Png).unwrap()).unwrap();
        let twice = decode(&encode(&once, Format::Png).unwrap()).unwrap();
        assert_eq!(once.trns, indexed.trns, "palette alpha, once");
        assert_eq!(twice.trns, indexed.trns, "palette alpha, twice");
        assert_eq!(twice.colour_key(), None, "a palette has no colour key");
    }

    /// The trap: the `png` encoder writes `iCCP` only
    /// when `sRGB` is absent, so a source carrying both chunks loses its profile
    /// on re-encode unless the encoder puts the `sRGB` chunk back by hand.
    #[test]
    fn a_source_with_both_srgb_and_icc_uses_authoritative_icc() {
        let mut raster = fixtures::by_name("rgb8-icc").raster;
        raster.srgb_intent = Some(0);

        let round_tripped = decode(&encode(&raster, Format::Png).unwrap()).unwrap();
        assert_eq!(round_tripped.icc, raster.icc, "the profile was dropped");
        assert_eq!(round_tripped.srgb_intent, None, "conflicting sRGB must be removed");
    }

    #[test]
    fn format_is_sniffed_from_magic_not_from_a_name() {
        let png = encode(&fixtures::by_name("l8").raster, Format::Png).unwrap();
        let tiff = encode(&fixtures::by_name("cmyk8").raster, Format::Tiff).unwrap();
        assert_eq!(Format::sniff(&png), Some(Format::Png));
        assert_eq!(Format::sniff(&tiff), Some(Format::Tiff));
        assert_eq!(Format::sniff(b"not an image at all"), None);
    }

    /// A detection segment is a range of rows, and a row range is a contiguous
    /// slice at every depth - including the sub-byte ones, where the assertion
    /// on `stride` is the whole point.
    #[test]
    fn a_row_range_is_a_raster_of_its_own_at_every_depth() {
        for fixture in fixtures::all() {
            let whole = fixture.raster;
            if whole.height < 4 {
                continue;
            }
            let band = whole.rows(1, 3);
            assert_eq!(band.height, 2, "{}", fixture.name);
            assert_eq!(band.width, whole.width, "{}", fixture.name);
            assert_eq!(band.stride(), whole.stride(), "{}", fixture.name);
            assert_eq!(band.mode, whole.mode, "{}", fixture.name);
            assert_eq!(band.depth, whole.depth, "{}", fixture.name);
            assert_eq!(band.palette, whole.palette, "{}", fixture.name);
            for x in 0..whole.width {
                for channel in 0..whole.mode.samples() {
                    assert_eq!(
                        band.sample(x, 0, channel),
                        whole.sample(x, 1, channel),
                        "{} at {x}",
                        fixture.name
                    );
                }
            }
        }
        // A range past the end is clamped rather than a panic: the caller is a
        // segment whose detection range reaches past its own page.
        let page = fixtures::by_name("l8").raster;
        assert_eq!(page.rows(0, page.height + 50).height, page.height);
        assert_eq!(page.rows(page.height, page.height + 4).height, 0);
    }

    #[test]
    fn indexed_and_cmyk_are_held_back_from_the_model_engines() {
        // Stated as a property of the mode so the router cannot
        // forget it per call site.
        assert!(!ColorMode::Indexed.allows_model_engines());
        assert!(!ColorMode::Cmyk.allows_model_engines());
        assert!(ColorMode::Gray.allows_model_engines());
        assert!(ColorMode::Rgb.allows_model_engines());
    }
}
