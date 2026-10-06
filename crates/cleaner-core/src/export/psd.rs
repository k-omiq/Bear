//! A layered Photoshop document, written from the pieces this crate already
//! holds.
//!
//! The layered shape is fixed:
//!
//! ```text
//! Background        raw source pixels, unmodified
//! Cleaned/          group
//!   region-0001     patch pixels + layer mask, positioned at the bbox
//!   region-0002     ...
//! ```
//!
//! and the merged image every PSD carries beside its layers is the composite  -
//! the same page the flattened export writes - so a reader that ignores layers
//! sees the cleaned page and a reader that honours them finds the original
//! *present*, not merely recoverable.
//!
//! ## Why this is written here rather than vendored
//!
//! Vendoring `ag-psd` was the earlier plan, and what it would have cost was
//! listed: the crate panics on at least five conditions, drops out-of-bounds
//! writes silently so an encode failure produces an empty channel that reports
//! success, has no grayscale path, discards 16-bit outright, and has never been
//! seen opening in Photoshop. Hardening that is a fork of a crate whose author
//! says it was not hand-audited. The format itself, for what this crate needs
//! of it - raw channel data, a group, a layer mask per region, an ICC resource -
//! is a few hundred lines of big-endian bookkeeping with no allocation the
//! caller cannot predict, and writing it directly gives every mode and depth
//! the rest of the crate has (grayscale, alpha, CMYK, 16-bit) for the same
//! price as one.
//!
//! ## What it writes
//!
//! - **Raw channel data everywhere** (compression 0). RLE would shrink 8-bit
//!   files and is the one thing `ag-psd` got wrong in a way that hid; raw has
//!   nothing to get wrong, and every length in the file is arithmetic on the
//!   dimensions, so nothing is buffered to be measured. The 2 GB ceiling is
//!   checked before any bytes are written.
//! - **16-bit layers under `Lr16`.** Photoshop stores a 16-bit document's layer
//!   records in the `Lr16` additional-info block and leaves the standard layer
//!   info empty; an 8-bit document uses the standard block. Both are written
//!   the way Photoshop writes them, because the reader that matters is
//!   Photoshop's.
//! - **CMYK inverted**, because PSD stores ink as `255 − value`: a TIFF's
//!   `0 = no ink` becomes `255 = no ink` on the way in, and nothing else about
//!   the samples changes.
//! - **ICC in image resource 1039**, verbatim.
//! - A merged image whose layer count is written negative when the mode has
//!   alpha, which is the file's way of saying the extra channel is the merged
//!   result's transparency rather than a spot channel.
//!
//! ## What it refuses
//!
//! PSD has no indexed mode with layers and no depth below 8 bits, and `.psd`
//! stops at 30 000 px a side. Both are decided from a header, before a byte is
//! written ([`refusal`]); the adapter puts every source's manifest entry to the
//! same function before it creates a directory.

use std::io::Write;

use crate::image::{BitDepth, ColorMode, Raster};
use crate::mask::{Mask, Rect};
use crate::patch::Patch;

use super::ExportError;

/// The `.psd` limit. `.psb` goes to 300 000 and is not written.
pub const PSD_MAX_DIMENSION: u32 = 30_000;
/// PSD's file size ceiling, as a signed 32-bit byte count.
pub const PSD_MAX_FILE_BYTES: u64 = i32::MAX as u64;

const SIGNATURE: &[u8; 4] = b"8BPS";
const VERSION_PSD: u16 = 1;
const RESOURCE_ICC_PROFILE: u16 = 1039;
const CHANNEL_ALPHA: i16 = -1;
const CHANNEL_USER_MASK: i16 = -2;
const COMPRESSION_RAW: u16 = 0;
/// Bit 3: "Photoshop 5.0 and later, tells if bit 4 has useful information".
const FLAG_HAS_BIT4: u8 = 0x08;
const FLAG_HIDDEN: u8 = 0x02;
/// Bit 4: pixel data irrelevant to the appearance of the document - a group.
const FLAG_IRRELEVANT_PIXELS: u8 = 0x10;
const SECTION_OPEN_FOLDER: u32 = 1;
const SECTION_DIVIDER: u32 = 3;

pub const BACKGROUND_LAYER: &str = "Background";
pub const CLEANED_GROUP: &str = "Cleaned";
/// Photoshop's own name for the hidden layer that closes a group.
const DIVIDER_LAYER: &str = "</Layer group>";

/// Why a page cannot be written as a PSD. Decided from a header.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PsdRefusal {
    #[error("PSD has no {mode:?} mode at {bits}-bit")]
    Unrepresentable { mode: ColorMode, bits: u8 },
    #[error("{width}×{height} is over PSD's {PSD_MAX_DIMENSION} px limit")]
    TooLarge { width: u32, height: u32 },
    #[error("PSD would be {bytes} bytes, over its {PSD_MAX_FILE_BYTES} byte limit")]
    FileTooLarge { bytes: u64 },
    #[error("PSD cannot carry {count} layers")]
    TooManyLayers { count: usize },
}

impl PsdRefusal {
    pub fn reason_key(&self) -> &'static str {
        match self {
            PsdRefusal::Unrepresentable { .. } => "notice.export.refusedLayeredMode",
            PsdRefusal::TooLarge { .. } => "notice.export.refusedLayeredSize",
            PsdRefusal::FileTooLarge { .. } => "notice.export.refusedLayeredFileSize",
            PsdRefusal::TooManyLayers { .. } => "notice.export.refusedLayeredCount",
        }
    }
}

/// Whether a page of this shape can be a PSD at all.
pub fn refusal(width: u32, height: u32, mode: ColorMode, depth: BitDepth) -> Option<PsdRefusal> {
    if mode_code(mode).is_none() || !matches!(depth, BitDepth::Eight | BitDepth::Sixteen) {
        return Some(PsdRefusal::Unrepresentable {
            mode,
            bits: depth.bits(),
        });
    }
    if width > PSD_MAX_DIMENSION || height > PSD_MAX_DIMENSION {
        return Some(PsdRefusal::TooLarge { width, height });
    }
    let minimum_bytes = u64::from(width)
        * u64::from(height)
        * mode.samples() as u64
        * u64::from(depth.bits() / 8)
        * 2;
    if minimum_bytes > PSD_MAX_FILE_BYTES {
        return Some(PsdRefusal::FileTooLarge { bytes: minimum_bytes });
    }
    None
}

/// What one page becomes.
#[derive(Debug, Clone, Copy)]
pub struct Document<'a> {
    /// The bottom layer, whole-page. The raw source for a layered export; the
    /// composite for a flattened one.
    pub background: &'a Raster,
    /// One layer each, bottom to top, inside the `Cleaned` group. Empty means
    /// no group at all.
    pub regions: &'a [&'a Patch],
    /// The merged image - what a reader that ignores layers sees.
    pub merged: &'a Raster,
}

/// Check the complete PSD layout without writing any channel data.
pub fn preflight_document(doc: &Document) -> Result<(), ExportError> {
    let page = doc.merged;
    if let Some(refusal) = refusal(page.width, page.height, page.mode, page.depth) {
        return Err(refusal.into());
    }
    let layers = layers_of(doc);
    let records: Vec<Vec<u8>> = layers
        .iter()
        .map(|layer| record(layer, page.depth))
        .collect::<Result<_, _>>()?;
    let profile = resolved_profile(page)?;
    check_layout(page, &layers, &records, profile.as_deref())?;
    Ok(())
}

/// Write the document. Returns the bytes written.
///
/// Every length in the file is computed before the first byte goes out, so the
/// sink needs no `Seek`, and nothing larger than one channel plane of one
/// layer is ever built in memory.
pub fn write_psd<W: Write>(sink: &mut W, doc: &Document) -> Result<u64, ExportError> {
    let page = doc.merged;
    if let Some(refusal) = refusal(page.width, page.height, page.mode, page.depth) {
        return Err(refusal.into());
    }
    if !doc.background.same_geometry(page) {
        return Err(ExportError::Sink(format!(
            "background is {}×{} {:?} where the merged image is {}×{} {:?}",
            doc.background.width,
            doc.background.height,
            doc.background.mode,
            page.width,
            page.height,
            page.mode
        )));
    }
    for region in doc.regions {
        if region.pixels.mode != page.mode || region.pixels.depth != page.depth {
            return Err(ExportError::Sink(format!(
                "region {} is {:?}/{:?} where the page is {:?}/{:?}",
                region.id, region.pixels.mode, region.pixels.depth, page.mode, page.depth
            )));
        }
        if !region.is_well_formed() {
            return Err(ExportError::Sink(format!(
                "region {} does not cover its mask",
                region.id
            )));
        }
    }

    let profile = resolved_profile(page)?;
    let layers = layers_of(doc);
    let depth = page.depth;
    let records: Vec<Vec<u8>> = layers
        .iter()
        .map(|layer| record(layer, depth))
        .collect::<Result<_, _>>()?;
    let (file_bytes, count, resources_len, body_len, body_padded, section_len) =
        check_layout(page, &layers, &records, profile.as_deref())?;
    let sixteen = depth == BitDepth::Sixteen;
    let resources_len_field = psd_u32(resources_len)?;
    let section_len_field = psd_u32(section_len)?;
    let body_len_field = psd_u32(body_padded)?;
    let resources = image_resources(profile.as_deref())?;
    let mut out = Counted { sink, written: 0 };

    /* Header --------------------------------------------------------- */
    out.bytes(SIGNATURE)?;
    out.be16(VERSION_PSD)?;
    out.bytes(&[0; 6])?;
    out.be16(page.mode.samples() as u16)?;
    out.be32(page.height)?;
    out.be32(page.width)?;
    out.be16(u16::from(depth.bits()))?;
    out.be16(mode_code(page.mode).expect("refused above"))?;

    /* Colour mode data: only indexed and duotone carry any ----------- */
    out.be32(0)?;

    /* Image resources ------------------------------------------------- */
    out.be32(resources_len_field)?;
    out.bytes(&resources)?;

    /* Layer and mask information -------------------------------------- */
    // Negative: "the first alpha channel contains the transparency data for
    // the merged result".
    let signed_count = if page.mode.alpha_channel().is_some() {
        -count
    } else {
        count
    };

    out.be32(section_len_field)?;

    if sixteen {
        out.be32(0)?;
        out.be32(0)?;
        out.bytes(b"8BIMLr16")?;
        out.be32(body_len_field)?;
    } else {
        out.be32(body_len_field)?;
    }
    out.be16(signed_count as u16)?;
    for bytes in &records {
        out.bytes(bytes)?;
    }
    for layer in &layers {
        for channel in &layer.channels {
            out.be16(COMPRESSION_RAW)?;
            channel
                .plane
                .write(&mut out, depth, page.mode == ColorMode::Cmyk)?;
        }
    }
    out.zeros(body_padded - body_len)?;
    if !sixteen {
        out.be32(0)?; // global layer mask info
    }

    /* Merged image data ------------------------------------------------ */
    out.be16(COMPRESSION_RAW)?;
    for channel in 0..page.mode.samples() {
        Plane::Pixels {
            raster: page,
            channel,
        }
        .write(&mut out, depth, page.mode == ColorMode::Cmyk)?;
    }

    out.sink.flush()?;
    debug_assert_eq!(out.written, file_bytes);
    Ok(out.written)
}

fn resolved_profile(page: &Raster) -> Result<Option<Vec<u8>>, ExportError> {
    if page.color.associated_alpha { return Err(crate::image::ImageError::Color("PSD cannot preserve associated-alpha native samples; export TIFF".into()).into()); }
    let profile = crate::image::metadata::equivalent_icc(&page.color, page.icc.as_deref(), page.srgb_intent, page.mode)?;
    if page.trns.is_some() { return Err(crate::image::ImageError::Color("PSD cannot retain PNG color-key transparency; export PNG".into()).into()); }
    Ok(profile)
}

fn check_layout(
    page: &Raster,
    layers: &[Layer<'_>],
    records: &[Vec<u8>],
    profile: Option<&[u8]>,
) -> Result<(u64, i16, u64, u64, u64, u64), ExportError> {
    let depth = page.depth;
    let resources_len = profile.map_or(0, |profile| {
        12 + profile.len() as u64 + (profile.len() % 2) as u64
    });
    let channel_bytes: u64 = layers
        .iter()
        .flat_map(|layer| layer.channels.iter())
        .map(|channel| channel.data_len(depth))
        .sum();
    let body_len = 2 + records.iter().map(|r| r.len() as u64).sum::<u64>() + channel_bytes;
    let sixteen = depth == BitDepth::Sixteen;
    let body_padded = pad_to(body_len, if sixteen { 4 } else { 2 });
    let section_len = if sixteen {
        // Empty layer info, empty global mask, then `8BIM` `Lr16` length body.
        4 + 4 + 12 + body_padded
    } else {
        4 + body_padded + 4
    };
    let merged_bytes = u64::from(page.width)
        * u64::from(page.height)
        * page.mode.samples() as u64
        * u64::from(depth.bits() / 8);
    let file_bytes = 26 + 4 + 4 + resources_len + 4 + section_len + 2 + merged_bytes;
    if file_bytes > PSD_MAX_FILE_BYTES {
        return Err(PsdRefusal::FileTooLarge { bytes: file_bytes }.into());
    }
    let count = i16::try_from(layers.len())
        .map_err(|_| ExportError::from(PsdRefusal::TooManyLayers { count: layers.len() }))?;
    psd_u32(resources_len)?;
    psd_u32(section_len)?;
    psd_u32(body_padded)?;
    image_resources(profile)?;
    Ok((file_bytes, count, resources_len, body_len, body_padded, section_len))
}

fn mode_code(mode: ColorMode) -> Option<u16> {
    Some(match mode {
        ColorMode::Gray | ColorMode::GrayAlpha => 1,
        ColorMode::Rgb | ColorMode::Rgba => 3,
        ColorMode::Cmyk => 4,
        ColorMode::Indexed => return None,
    })
}

fn pad_to(len: u64, multiple: u64) -> u64 {
    len.div_ceil(multiple) * multiple
}

fn psd_u32(len: u64) -> Result<u32, ExportError> {
    u32::try_from(len).map_err(|_| PsdRefusal::FileTooLarge { bytes: len }.into())
}

/// The `8BIM` blocks of the image resources section.
fn image_resources(icc: Option<&[u8]>) -> Result<Vec<u8>, ExportError> {
    let mut out = Vec::new();
    if let Some(profile) = icc {
        out.extend_from_slice(b"8BIM");
        out.extend_from_slice(&RESOURCE_ICC_PROFILE.to_be_bytes());
        // An empty Pascal name: length byte, then padding to an even size.
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&psd_u32(profile.len() as u64)?.to_be_bytes());
        out.extend_from_slice(profile);
        if profile.len() % 2 == 1 {
            out.push(0);
        }
    }
    Ok(out)
}

/* ------------------------------------------------------------------ */
/* Layers                                                             */
/* ------------------------------------------------------------------ */

/// Where a channel's samples come from.
#[derive(Debug, Clone, Copy)]
enum Plane<'a> {
    Pixels {
        raster: &'a Raster,
        channel: usize,
    },
    Mask(&'a Mask),
    /// A group or divider layer: the channel exists, with no samples.
    Empty,
}

impl Plane<'_> {
    fn samples(&self) -> u64 {
        match self {
            Plane::Pixels { raster, .. } => u64::from(raster.width) * u64::from(raster.height),
            Plane::Mask(mask) => u64::from(mask.bounds.w) * u64::from(mask.bounds.h),
            Plane::Empty => 0,
        }
    }

    /// One plane, in row-major order, big-endian at 16 bits. Built one plane
    /// at a time and dropped, which bounds what this module holds to one
    /// channel of one layer.
    fn write<W: Write>(
        &self,
        out: &mut Counted<W>,
        depth: BitDepth,
        invert: bool,
    ) -> Result<(), ExportError> {
        let bytes = match self {
            Plane::Pixels { raster, channel } => plane_of(raster, *channel, invert),
            Plane::Mask(mask) => match depth {
                BitDepth::Sixteen => mask.bits.iter().flat_map(|&b| [b, b]).collect(),
                _ => mask.bits.clone(),
            },
            Plane::Empty => Vec::new(),
        };
        out.bytes(&bytes)
    }
}

/// One channel of a raster, de-interleaved. `invert` is the CMYK rule: PSD
/// stores `max − value`, which for both depths is the bitwise complement.
fn plane_of(raster: &Raster, channel: usize, invert: bool) -> Vec<u8> {
    let samples = raster.mode.samples();
    let per_sample = usize::from(raster.depth.bits()) / 8;
    let count = raster.width as usize * raster.height as usize;
    let mut out = Vec::with_capacity(count * per_sample);
    for i in 0..count {
        let at = (i * samples + channel) * per_sample;
        for byte in &raster.data[at..at + per_sample] {
            out.push(if invert { !byte } else { *byte });
        }
    }
    out
}

#[derive(Debug, Clone, Copy)]
struct Channel<'a> {
    id: i16,
    plane: Plane<'a>,
}

impl Channel<'_> {
    /// The compression word plus the samples.
    fn data_len(&self, depth: BitDepth) -> u64 {
        2 + self.plane.samples() * u64::from(depth.bits() / 8)
    }
}

#[derive(Debug, Clone)]
struct Layer<'a> {
    name: String,
    rect: Rect,
    channels: Vec<Channel<'a>>,
    mask: Option<&'a Mask>,
    hidden: bool,
    opacity: u8,
    /// `lsct` kind, for a group or a divider.
    section: Option<u32>,
}

/// Bottom to top: the background, then the group's divider, its regions and
/// the group itself, which is how Photoshop lists a folder.
fn layers_of<'a>(doc: &Document<'a>) -> Vec<Layer<'a>> {
    let page = doc.merged;
    let mut layers = vec![pixel_layer(
        BACKGROUND_LAYER.to_owned(),
        doc.background,
        Rect::new(0, 0, page.width, page.height),
        None,
        false,
        255,
    )];
    if doc.regions.is_empty() {
        return layers;
    }
    layers.push(empty_layer(
        DIVIDER_LAYER.to_owned(),
        page.mode,
        SECTION_DIVIDER,
    ));
    for (n, region) in doc.regions.iter().enumerate() {
        layers.push(pixel_layer(
            format!("region-{:04}", n + 1),
            &region.pixels,
            region.mask.bounds,
            Some(&region.mask),
            !region.visible,
            ((u16::from(region.layer_style().opacity) * 255 + 50) / 100) as u8,
        ));
    }
    layers.push(empty_layer(
        CLEANED_GROUP.to_owned(),
        page.mode,
        SECTION_OPEN_FOLDER,
    ));
    layers
}

/// Colour channel ids in sample order, with alpha as `-1` where the mode has
/// one.
fn channel_ids(mode: ColorMode) -> Vec<i16> {
    (0..mode.samples())
        .map(|sample| {
            if mode.alpha_channel() == Some(sample) {
                CHANNEL_ALPHA
            } else {
                sample as i16
            }
        })
        .collect()
}

fn pixel_layer<'a>(
    name: String,
    raster: &'a Raster,
    rect: Rect,
    mask: Option<&'a Mask>,
    hidden: bool,
    opacity: u8,
) -> Layer<'a> {
    let mut channels: Vec<Channel<'a>> = channel_ids(raster.mode)
        .into_iter()
        .enumerate()
        .map(|(channel, id)| Channel {
            id,
            plane: Plane::Pixels { raster, channel },
        })
        .collect();
    if let Some(mask) = mask {
        channels.push(Channel {
            id: CHANNEL_USER_MASK,
            plane: Plane::Mask(mask),
        });
    }
    Layer {
        name,
        rect,
        channels,
        mask,
        hidden,
        opacity,
        section: None,
    }
}

/// A group or divider: no pixels, every channel present and empty, which is
/// how Photoshop writes its own.
fn empty_layer<'a>(name: String, mode: ColorMode, section: u32) -> Layer<'a> {
    let mut ids = channel_ids(mode);
    if !ids.contains(&CHANNEL_ALPHA) {
        ids.insert(0, CHANNEL_ALPHA);
    }
    Layer {
        name,
        rect: Rect::new(0, 0, 0, 0),
        channels: ids
            .into_iter()
            .map(|id| Channel {
                id,
                plane: Plane::Empty,
            })
            .collect(),
        mask: None,
        hidden: false,
        opacity: 255,
        section: Some(section),
    }
}

/// One layer record, whole. Small - a few dozen bytes plus the name.
fn record(layer: &Layer, depth: BitDepth) -> Result<Vec<u8>, ExportError> {
    let mut r = Vec::new();
    push_rect(&mut r, layer.rect);
    r.extend_from_slice(&(layer.channels.len() as u16).to_be_bytes());
    for channel in &layer.channels {
        r.extend_from_slice(&channel.id.to_be_bytes());
        r.extend_from_slice(&psd_u32(channel.data_len(depth))?.to_be_bytes());
    }
    r.extend_from_slice(b"8BIM");
    r.extend_from_slice(if layer.section.is_some() {
        b"pass"
    } else {
        b"norm"
    });
    r.push(layer.opacity);
    r.push(0); // clipping: base
    let mut flags = FLAG_HAS_BIT4;
    if layer.hidden {
        flags |= FLAG_HIDDEN;
    }
    if layer.section.is_some() {
        flags |= FLAG_IRRELEVANT_PIXELS;
    }
    r.push(flags);
    r.push(0); // filler

    let mut extra = Vec::new();
    match layer.mask {
        Some(mask) => {
            extra.extend_from_slice(&20u32.to_be_bytes());
            push_rect(&mut extra, mask.bounds);
            extra.push(0); // default colour: outside the rectangle is hidden
            extra.push(0); // flags: absolute position, enabled, not inverted
            extra.extend_from_slice(&[0, 0]);
        }
        None => extra.extend_from_slice(&0u32.to_be_bytes()),
    }
    extra.extend_from_slice(&0u32.to_be_bytes()); // blending ranges
    extra.extend_from_slice(&pascal_name(&layer.name));
    extra.extend_from_slice(&unicode_name(&layer.name)?);
    if let Some(kind) = layer.section {
        extra.extend_from_slice(b"8BIMlsct");
        extra.extend_from_slice(&12u32.to_be_bytes());
        extra.extend_from_slice(&kind.to_be_bytes());
        extra.extend_from_slice(b"8BIMpass");
    }
    r.extend_from_slice(&psd_u32(extra.len() as u64)?.to_be_bytes());
    r.extend_from_slice(&extra);
    Ok(r)
}

/// `top, left, bottom, right`, which is the order every rectangle in the
/// format takes.
fn push_rect(out: &mut Vec<u8>, rect: Rect) {
    out.extend_from_slice(&(rect.y as i32).to_be_bytes());
    out.extend_from_slice(&(rect.x as i32).to_be_bytes());
    out.extend_from_slice(&(rect.bottom() as i32).to_be_bytes());
    out.extend_from_slice(&(rect.right() as i32).to_be_bytes());
}

/// A Pascal string padded to a multiple of 4, the record's own rule. The
/// byte-length form is capped at 255 and the Unicode block beside it carries
/// the whole name.
fn pascal_name(name: &str) -> Vec<u8> {
    let bytes: Vec<u8> = name.bytes().take(255).collect();
    let mut out = vec![bytes.len() as u8];
    out.extend_from_slice(&bytes);
    while out.len() % 4 != 0 {
        out.push(0);
    }
    out
}

/// `8BIM` `luni`: the name as UTF-16BE, which is the one Photoshop shows.
fn unicode_name(name: &str) -> Result<Vec<u8>, ExportError> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let mut data = Vec::with_capacity(4 + units.len() * 2);
    data.extend_from_slice(&psd_u32(units.len() as u64)?.to_be_bytes());
    for unit in units {
        data.extend_from_slice(&unit.to_be_bytes());
    }
    if data.len() % 2 == 1 {
        data.push(0);
    }
    let mut out = Vec::with_capacity(12 + data.len());
    out.extend_from_slice(b"8BIMluni");
    out.extend_from_slice(&psd_u32(data.len() as u64)?.to_be_bytes());
    out.extend_from_slice(&data);
    Ok(out)
}

/* ------------------------------------------------------------------ */
/* A sink that counts                                                 */
/* ------------------------------------------------------------------ */

struct Counted<'a, W: Write> {
    sink: &'a mut W,
    written: u64,
}

impl<W: Write> Counted<'_, W> {
    fn bytes(&mut self, bytes: &[u8]) -> Result<(), ExportError> {
        self.sink.write_all(bytes)?;
        self.written += bytes.len() as u64;
        Ok(())
    }

    fn be16(&mut self, value: u16) -> Result<(), ExportError> {
        self.bytes(&value.to_be_bytes())
    }

    fn be32(&mut self, value: u32) -> Result<(), ExportError> {
        self.bytes(&value.to_be_bytes())
    }

    fn zeros(&mut self, count: u64) -> Result<(), ExportError> {
        for _ in 0..count {
            self.bytes(&[0])?;
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::composite::composite;
    use crate::image::fixtures;
    use crate::patch::{Engine, Provenance};
    use sha2::{Digest, Sha256};

    /* A reader written from the specification, not from the writer. ---- */

    #[derive(Debug)]
    pub struct ParsedLayer {
        pub name: String,
        pub unicode_name: Option<String>,
        pub rect: Rect,
        pub blend: String,
        pub hidden: bool,
        pub section: Option<u32>,
        pub mask_rect: Option<Rect>,
        /// `(id, samples)` in record order, compression word stripped.
        pub channels: Vec<(i16, Vec<u8>)>,
    }

    #[derive(Debug)]
    pub struct Parsed {
        pub channels: u16,
        pub width: u32,
        pub height: u32,
        pub depth: u16,
        pub mode: u16,
        pub icc: Option<Vec<u8>>,
        pub signed_layer_count: i16,
        pub layers: Vec<ParsedLayer>,
        pub merged: Vec<Vec<u8>>,
    }

    struct Cursor<'a> {
        bytes: &'a [u8],
        at: usize,
    }

    impl<'a> Cursor<'a> {
        fn take(&mut self, n: usize) -> &'a [u8] {
            let out = &self.bytes[self.at..self.at + n];
            self.at += n;
            out
        }
        fn u8(&mut self) -> u8 {
            self.take(1)[0]
        }
        fn u16(&mut self) -> u16 {
            u16::from_be_bytes(self.take(2).try_into().unwrap())
        }
        fn i16(&mut self) -> i16 {
            self.u16() as i16
        }
        fn u32(&mut self) -> u32 {
            u32::from_be_bytes(self.take(4).try_into().unwrap())
        }
        fn i32(&mut self) -> i32 {
            self.u32() as i32
        }
        fn rect(&mut self) -> Rect {
            let top = self.i32();
            let left = self.i32();
            let bottom = self.i32();
            let right = self.i32();
            Rect::new(
                left as i64,
                top as i64,
                (right - left) as u32,
                (bottom - top) as u32,
            )
        }
    }

    pub fn parse(bytes: &[u8]) -> Parsed {
        let mut c = Cursor { bytes, at: 0 };
        assert_eq!(c.take(4), b"8BPS");
        assert_eq!(c.u16(), 1, "version");
        assert_eq!(c.take(6), &[0; 6]);
        let channels = c.u16();
        let height = c.u32();
        let width = c.u32();
        let depth = c.u16();
        let mode = c.u16();

        let colour_mode_len = c.u32() as usize;
        c.take(colour_mode_len);

        let resources_len = c.u32() as usize;
        let resources_end = c.at + resources_len;
        let mut icc = None;
        while c.at < resources_end {
            assert_eq!(c.take(4), b"8BIM");
            let id = c.u16();
            let name_len = c.u8() as usize;
            c.take(name_len);
            if name_len.is_multiple_of(2) {
                c.u8();
            }
            let size = c.u32() as usize;
            let data = c.take(size).to_vec();
            if size % 2 == 1 {
                c.u8();
            }
            if id == RESOURCE_ICC_PROFILE {
                icc = Some(data);
            }
        }
        assert_eq!(c.at, resources_end, "image resources overran their length");

        let section_len = c.u32() as usize;
        let section_end = c.at + section_len;
        let mut layer_info_len = c.u32() as usize;
        let mut signed_layer_count = 0i16;
        let mut layers = Vec::new();
        if layer_info_len == 0 && depth == 16 {
            let global_len = c.u32() as usize;
            c.take(global_len);
            assert_eq!(c.take(4), b"8BIM");
            assert_eq!(c.take(4), b"Lr16");
            layer_info_len = c.u32() as usize;
        }
        if layer_info_len > 0 {
            let info_end = c.at + layer_info_len;
            signed_layer_count = c.i16();
            let count = signed_layer_count.unsigned_abs() as usize;
            let mut records = Vec::new();
            for _ in 0..count {
                let rect = c.rect();
                let channel_count = c.u16() as usize;
                let mut ids = Vec::new();
                for _ in 0..channel_count {
                    ids.push((c.i16(), c.u32() as usize));
                }
                assert_eq!(c.take(4), b"8BIM");
                let blend = String::from_utf8(c.take(4).to_vec()).unwrap();
                let _opacity = c.u8();
                let _clipping = c.u8();
                let flags = c.u8();
                c.u8();
                let extra_len = c.u32() as usize;
                let extra_end = c.at + extra_len;
                let mask_len = c.u32() as usize;
                let mask_rect = if mask_len >= 20 {
                    let rect = c.rect();
                    c.take(mask_len - 16);
                    Some(rect)
                } else {
                    c.take(mask_len);
                    None
                };
                let ranges_len = c.u32() as usize;
                c.take(ranges_len);
                let name_len = c.u8() as usize;
                let name = String::from_utf8(c.take(name_len).to_vec()).unwrap();
                let padded = (1 + name_len).div_ceil(4) * 4;
                c.take(padded - 1 - name_len);
                let mut unicode_name = None;
                let mut section = None;
                while c.at < extra_end {
                    assert_eq!(c.take(4), b"8BIM");
                    let key = c.take(4).to_vec();
                    let len = c.u32() as usize;
                    let data = c.take(len);
                    match &key[..] {
                        b"luni" => {
                            let n = u32::from_be_bytes(data[..4].try_into().unwrap()) as usize;
                            let units: Vec<u16> = (0..n)
                                .map(|i| u16::from_be_bytes([data[4 + i * 2], data[5 + i * 2]]))
                                .collect();
                            unicode_name = Some(String::from_utf16(&units).unwrap());
                        }
                        b"lsct" => {
                            section = Some(u32::from_be_bytes(data[..4].try_into().unwrap()))
                        }
                        _ => {}
                    }
                }
                assert_eq!(c.at, extra_end, "layer extra data overran");
                records.push((
                    rect,
                    ids,
                    blend,
                    flags,
                    mask_rect,
                    name,
                    unicode_name,
                    section,
                ));
            }
            for (rect, ids, blend, flags, mask_rect, name, unicode_name, section) in records {
                let mut channels = Vec::new();
                for (id, len) in ids {
                    assert_eq!(c.u16(), 0, "compression");
                    channels.push((id, c.take(len - 2).to_vec()));
                }
                layers.push(ParsedLayer {
                    name,
                    unicode_name,
                    rect,
                    blend,
                    hidden: flags & FLAG_HIDDEN != 0,
                    section,
                    mask_rect,
                    channels,
                });
            }
            assert!(c.at <= info_end, "layer info overran its length");
            c.at = info_end;
            if depth != 16 {
                let global_len = c.u32() as usize;
                c.take(global_len);
            }
        }
        assert_eq!(
            c.at, section_end,
            "layer and mask section overran its length"
        );

        assert_eq!(c.u16(), 0, "merged compression");
        let per_sample = depth as usize / 8;
        let plane = width as usize * height as usize * per_sample;
        let merged = (0..channels).map(|_| c.take(plane).to_vec()).collect();
        assert_eq!(c.at, bytes.len(), "trailing bytes");

        Parsed {
            channels,
            width,
            height,
            depth,
            mode,
            icc,
            signed_layer_count,
            layers,
            merged,
        }
    }

    /* Fixtures --------------------------------------------------------- */

    fn provenance() -> Provenance {
        Provenance {
            engine: Engine::Fill,
            engine_version: "test".into(),
            model_sha256: None,
            execution_provider: "cpu".into(),
            params_snapshot: serde_json::json!({}),
            mask_sha256: "0".repeat(64),
            source_sha256: "0".repeat(64),
            cloud: None,
            created: 0,
        }
    }

    /// A patch over `bounds` whose every sample is the complement of the
    /// page's, so it is visible in every channel of every fixture. Its mask
    /// is the rectangle less a one-pixel border.
    pub fn patch_over(page: &Raster, bounds: Rect, id: &str, order: u32) -> Patch {
        let mut pixels = Raster {
            width: bounds.w,
            height: bounds.h,
            mode: page.mode,
            depth: page.depth,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None,
            color: Default::default(),
            data: vec![
                0;
                bounds.w as usize * page.mode.samples() * page.depth.bits() as usize / 8
                    * bounds.h as usize
            ],
        };
        let ceiling = if page.depth == BitDepth::Sixteen {
            u16::MAX
        } else {
            255
        };
        for y in 0..bounds.h {
            for x in 0..bounds.w {
                for channel in 0..page.mode.samples() {
                    let source = page.sample(bounds.x as u32 + x, bounds.y as u32 + y, channel);
                    pixels.set_sample(x, y, channel, ceiling - source);
                }
            }
        }
        let mut mask = Mask::empty(bounds);
        for y in bounds.y + 1..bounds.bottom() - 1 {
            for x in bounds.x + 1..bounds.right() - 1 {
                mask.set(x, y, true);
            }
        }
        Patch {
            id: id.into(),
            ink: mask.clone(),
            mask,
            pixels,
            order,
            visible: true,
            provenance: provenance(),
        }
    }

    fn layered(page: &Raster, regions: &[&Patch]) -> Vec<u8> {
        let owned: Vec<Patch> = regions.iter().map(|p| (*p).clone()).collect();
        let merged = composite(page, &owned).unwrap();
        let mut out = Vec::new();
        let written = write_psd(
            &mut out,
            &Document {
                background: page,
                regions,
                merged: &merged,
            },
        )
        .unwrap();
        assert_eq!(written, out.len() as u64, "the count is the file");
        out
    }

    fn plane_expected(raster: &Raster, channel: usize) -> Vec<u8> {
        plane_of(raster, channel, raster.mode == ColorMode::Cmyk)
    }

    /* The claims ------------------------------------------------------- */

    /// Every fixture PSD can carry, written layered, reads back with the
    /// structure the layered shape draws: background, divider, regions, group  -
    /// and the background's channels are the source's own samples.
    #[test]
    fn every_carriable_fixture_round_trips_its_structure() {
        for fixture in fixtures::all() {
            let page = &fixture.raster;
            if refusal(page.width, page.height, page.mode, page.depth).is_some() {
                continue;
            }
            let a = patch_over(page, Rect::new(4, 4, 12, 10), "a", 0);
            let b = patch_over(page, Rect::new(30, 20, 20, 16), "b", 1);
            let bytes = layered(page, &[&a, &b]);
            let psd = parse(&bytes);

            assert_eq!(psd.width, page.width, "{}", fixture.name);
            assert_eq!(psd.height, page.height, "{}", fixture.name);
            assert_eq!(psd.depth, u16::from(page.depth.bits()), "{}", fixture.name);
            assert_eq!(psd.mode, mode_code(page.mode).unwrap(), "{}", fixture.name);
            assert_eq!(psd.channels, page.mode.samples() as u16, "{}", fixture.name);
            assert_eq!(psd.icc, page.icc, "{}: icc", fixture.name);

            let names: Vec<&str> = psd.layers.iter().map(|l| l.name.as_str()).collect();
            assert_eq!(
                names,
                [
                    BACKGROUND_LAYER,
                    DIVIDER_LAYER,
                    "region-0001",
                    "region-0002",
                    CLEANED_GROUP
                ],
                "{}",
                fixture.name
            );
            assert_eq!(psd.layers[1].section, Some(SECTION_DIVIDER));
            assert_eq!(psd.layers[4].section, Some(SECTION_OPEN_FOLDER));
            assert_eq!(psd.layers[4].blend, "pass");
            assert_eq!(psd.layers[0].blend, "norm");
            assert_eq!(
                psd.layers[0].unicode_name.as_deref(),
                Some(BACKGROUND_LAYER)
            );

            let background = &psd.layers[0];
            assert_eq!(background.rect, Rect::new(0, 0, page.width, page.height));
            assert_eq!(background.mask_rect, None);
            for (sample, id) in channel_ids(page.mode).into_iter().enumerate() {
                let (found, data) = &background.channels[sample];
                assert_eq!(*found, id, "{}: channel order", fixture.name);
                assert_eq!(
                    data,
                    &plane_expected(page, sample),
                    "{}: background channel {id}",
                    fixture.name
                );
            }

            let region = &psd.layers[2];
            assert_eq!(region.rect, a.mask.bounds, "{}", fixture.name);
            assert_eq!(region.mask_rect, Some(a.mask.bounds), "{}", fixture.name);
            assert!(!region.hidden);
            let (mask_id, mask_data) = region.channels.last().unwrap();
            assert_eq!(*mask_id, CHANNEL_USER_MASK);
            let expected_mask: Vec<u8> = match page.depth {
                BitDepth::Sixteen => a.mask.bits.iter().flat_map(|&b| [b, b]).collect(),
                _ => a.mask.bits.clone(),
            };
            assert_eq!(mask_data, &expected_mask, "{}: layer mask", fixture.name);
            assert_eq!(
                region.channels[0].1,
                plane_expected(&a.pixels, 0),
                "{}: region pixels",
                fixture.name
            );

            let merged = composite(page, &[a.clone(), b.clone()]).unwrap();
            for channel in 0..page.mode.samples() {
                assert_eq!(
                    psd.merged[channel],
                    plane_expected(&merged, channel),
                    "{}: merged {channel}",
                    fixture.name
                );
            }
            let has_alpha = page.mode.alpha_channel().is_some();
            assert_eq!(
                psd.signed_layer_count < 0,
                has_alpha,
                "{}: transparency flag",
                fixture.name
            );
        }
    }

    #[test]
    fn generated_color_profile_participates_in_layout_preflight_and_output() {
        let mut page = fixtures::by_name("rgb8").raster;
        page.color.gamma = Some(100000);
        let doc = Document { background: &page, regions: &[], merged: &page };
        preflight_document(&doc).unwrap();
        let mut bytes = Vec::new();
        let written = write_psd(&mut bytes, &doc).unwrap();
        assert_eq!(written as usize, bytes.len());
        page.icc = Some(parse(&bytes).icc.expect("generated profile resource"));
        page.color = Default::default();
        let managed = crate::image::color::ManagedColor::for_raster(&page).unwrap();
        let half_linear = managed.to_srgb([0.5; 4]);
        assert!(half_linear.iter().all(|value| (*value - 0.735357).abs() < 0.003));
    }

    #[test]
    fn layered_text_shape_uses_sparse_write_support_as_its_mask() {
        let page = fixtures::by_name("rgb8-icc").raster;
        let mut patch = patch_over(&page, Rect::new(4, 4, 12, 10), "shape", 0);
        patch.ink = Mask::filled(patch.mask.bounds);
        patch.mask = Mask::empty(patch.mask.bounds);
        patch.mask.set(5, 5, true);
        patch.mask.set(14, 12, true);
        patch.provenance.params_snapshot = serde_json::json!({"geometry_policy": "text_shape"});
        let parsed = parse(&layered(&page, &[&patch]));
        let layer = &parsed.layers[2];
        assert_eq!(layer.mask_rect, Some(patch.mask.bounds));
        assert_eq!(layer.channels.last().unwrap().1, patch.mask.bits);
        assert_eq!(parsed.icc, page.icc);
    }

    /// A flattened document is one Background layer holding the composite,
    /// no group.
    #[test]
    fn a_flattened_document_is_one_background_layer_of_the_composite() {
        let page = fixtures::by_name("rgb8").raster;
        let a = patch_over(&page, Rect::new(4, 4, 12, 10), "a", 0);
        let merged = composite(&page, std::slice::from_ref(&a)).unwrap();
        let mut out = Vec::new();
        write_psd(
            &mut out,
            &Document {
                background: &merged,
                regions: &[],
                merged: &merged,
            },
        )
        .unwrap();
        let psd = parse(&out);
        assert_eq!(psd.layers.len(), 1);
        assert_eq!(psd.layers[0].name, BACKGROUND_LAYER);
        assert_eq!(psd.layers[0].channels[1].1, plane_expected(&merged, 1));
        assert_ne!(
            psd.layers[0].channels[1].1,
            plane_expected(&page, 1),
            "the patch changed nothing"
        );
    }

    /// A hidden region is a hidden layer, and still in the file: the point of
    /// a layered export is that nothing is thrown away.
    #[test]
    fn a_hidden_region_is_written_hidden() {
        let page = fixtures::by_name("l8").raster;
        let mut a = patch_over(&page, Rect::new(4, 4, 12, 10), "a", 0);
        a.visible = false;
        let bytes = layered(&page, &[&a]);
        let psd = parse(&bytes);
        assert!(psd.layers[2].hidden);
        assert!(!psd.layers[0].hidden);
        assert_eq!(
            psd.merged[0],
            plane_expected(&page, 0),
            "a hidden region reached the merged image"
        );
    }

    /// PSD stores CMYK as `255 − value`, so a TIFF's "no ink" and a PSD's agree
    /// once inverted - and the merged image is inverted the same way.
    #[test]
    fn cmyk_is_inverted_on_the_way_in() {
        let page = fixtures::by_name("cmyk8").raster;
        let bytes = layered(&page, &[]);
        let psd = parse(&bytes);
        assert_eq!(psd.mode, 4);
        let stored = &psd.layers[0].channels[3].1;
        let expected: Vec<u8> = (0..page.width * page.height)
            .map(|i| !page.sample(i % page.width, i / page.width, 3) as u8)
            .collect();
        assert_eq!(stored, &expected);
        assert_eq!(psd.merged[3], expected);
    }

    #[test]
    fn what_psd_cannot_carry_is_refused_from_a_header() {
        use ColorMode::*;
        assert_eq!(refusal(10, 10, Gray, BitDepth::Eight), None);
        assert_eq!(refusal(10, 10, Cmyk, BitDepth::Sixteen), None);
        assert_eq!(
            refusal(10, 10, Indexed, BitDepth::Eight),
            Some(PsdRefusal::Unrepresentable {
                mode: Indexed,
                bits: 8
            })
        );
        assert_eq!(
            refusal(10, 10, Gray, BitDepth::One),
            Some(PsdRefusal::Unrepresentable {
                mode: Gray,
                bits: 1
            })
        );
        assert_eq!(
            refusal(PSD_MAX_DIMENSION + 1, 10, Rgb, BitDepth::Eight),
            Some(PsdRefusal::TooLarge {
                width: PSD_MAX_DIMENSION + 1,
                height: 10
            })
        );
        assert_eq!(
            refusal(PSD_MAX_DIMENSION, PSD_MAX_DIMENSION, Gray, BitDepth::Eight),
            None
        );

        let page = fixtures::by_name("indexed-p").raster;
        let mut out = Vec::new();
        let err = write_psd(
            &mut out,
            &Document {
                background: &page,
                regions: &[],
                merged: &page,
            },
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ExportError::NotLayerable(PsdRefusal::Unrepresentable { .. })
        ));
        assert!(out.is_empty(), "a refusal wrote bytes");
    }

    #[test]
    fn oversized_psd_is_refused_from_a_header() {
        assert!(matches!(
            refusal(14_000, 14_000, ColorMode::Rgb, BitDepth::Sixteen),
            Some(PsdRefusal::FileTooLarge { .. })
        ));
    }

    #[test]
    fn too_many_psd_layers_are_refused_before_writing() {
        let page = fixtures::by_name("rgb8").raster;
        let patch = patch_over(&page, Rect::new(1, 1, 1, 1), "region", 0);
        let regions = vec![&patch; i16::MAX as usize - 2];
        let document = Document { background: &page, regions: &regions, merged: &page };
        assert!(matches!(
            preflight_document(&document),
            Err(ExportError::NotLayerable(PsdRefusal::TooManyLayers { .. }))
        ));
    }

    #[test]
    fn a_large_sixteen_bit_layered_page_is_refused_before_writing() {
        let mut page = fixtures::by_name("rgb16").raster;
        page.width = PSD_MAX_DIMENSION;
        page.height = PSD_MAX_DIMENSION;
        page.mode = ColorMode::Rgba;
        page.data.clear();
        let bounds = Rect::new(0, 0, 1, 1);
        let mask = Mask::empty(bounds);
        let patch = Patch {
            id: "region".into(),
            ink: mask.clone(),
            mask,
            pixels: Raster {
                width: 1,
                height: 1,
                mode: ColorMode::Rgba,
                depth: BitDepth::Sixteen,
                icc: None,
                palette: None,
                trns: None,
                srgb_intent: None,
                color: Default::default(),
                data: vec![0; 8],
            },
            order: 0,
            visible: true,
            provenance: provenance(),
        };
        let mut out = Vec::new();
        let err = write_psd(
            &mut out,
            &Document {
                background: &page,
                regions: &[&patch],
                merged: &page,
            },
        )
        .unwrap_err();
        let ExportError::NotLayerable(PsdRefusal::FileTooLarge { bytes }) = err else {
            panic!("expected a typed PSD size refusal: {err:?}");
        };
        assert!(bytes > PSD_MAX_FILE_BYTES);
        assert!(out.is_empty(), "a refusal wrote bytes");
    }

    #[test]
    fn ordinary_layered_fixture_keeps_its_bytes() {
        let page = fixtures::by_name("rgb8").raster;
        let a = patch_over(&page, Rect::new(4, 4, 12, 10), "a", 0);
        let b = patch_over(&page, Rect::new(30, 20, 20, 16), "b", 1);
        let bytes = layered(&page, &[&a, &b]);
        assert_eq!(bytes.len(), 20_938);
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            "ce1662a0dd10f3eef5e033607dc187e429c6b4c341c1127192579b9d6bff4a5c"
        );
    }

    /// **The external oracle.** Our reader agreeing with our writer proves the
    /// two share a reading of the specification and nothing more. This writes
    /// every carriable fixture, layered and flattened, into
    /// `$PSD_ORACLE_DIR` for `scripts/check-psd.py` to open with `psd-tools`;
    /// Photoshop itself is still the reader that matters and is not here.
    ///
    /// ```sh
    /// PSD_ORACLE_DIR=/tmp/psd cargo test -p cleaner-core --lib export::psd -- --ignored
    /// python3 scripts/check-psd.py /tmp/psd
    /// ```
    #[test]
    #[ignore = "writes files for an external reader; run by hand with PSD_ORACLE_DIR set"]
    fn write_fixtures_for_the_external_oracle() {
        let dir = std::path::PathBuf::from(
            std::env::var("PSD_ORACLE_DIR").expect("PSD_ORACLE_DIR names the output directory"),
        );
        std::fs::create_dir_all(&dir).unwrap();
        for fixture in fixtures::all() {
            let page = &fixture.raster;
            if refusal(page.width, page.height, page.mode, page.depth).is_some() {
                continue;
            }
            let a = patch_over(page, Rect::new(4, 4, 12, 10), "a", 0);
            let b = patch_over(page, Rect::new(30, 20, 20, 16), "b", 1);
            std::fs::write(
                dir.join(format!("{}-layered.psd", fixture.name)),
                layered(page, &[&a, &b]),
            )
            .unwrap();
            let merged = composite(page, &[a.clone(), b.clone()]).unwrap();
            let mut flat = Vec::new();
            write_psd(
                &mut flat,
                &Document {
                    background: &merged,
                    regions: &[],
                    merged: &merged,
                },
            )
            .unwrap();
            std::fs::write(dir.join(format!("{}-flat.psd", fixture.name)), flat).unwrap();
            // What the oracle should see: the merged image and the mask, as
            // raw planes, so the check is against numbers rather than against
            // a second reading of the format.
            let mut expected = Vec::new();
            for channel in 0..page.mode.samples() {
                expected.extend(plane_of(&merged, channel, false));
            }
            std::fs::write(dir.join(format!("{}.merged", fixture.name)), expected).unwrap();
            std::fs::write(dir.join(format!("{}.mask-a", fixture.name)), &a.mask.bits).unwrap();
        }
    }

    /// The name block rules: a Pascal name pads to four, a Unicode name to two.
    #[test]
    fn names_are_padded_the_way_the_record_expects() {
        assert_eq!(pascal_name("abc"), [3, b'a', b'b', b'c']);
        assert_eq!(pascal_name("abcd"), [4, b'a', b'b', b'c', b'd', 0, 0, 0]);
        assert_eq!(pascal_name(""), [0, 0, 0, 0]);
        let luni = unicode_name("ab").unwrap();
        assert_eq!(&luni[..8], b"8BIMluni");
        assert_eq!(u32::from_be_bytes(luni[8..12].try_into().unwrap()), 8);
        assert_eq!(&luni[12..], [0, 0, 0, 2, 0, b'a', 0, b'b']);
    }
}
