//! Turning a page and its patches back into a file.
//!
//! A page with **zero applied patches is copied byte-for-byte**, and
//! passthrough respects the chosen output format rather than leaving one folder
//! holding an untouched 400 KB JPEG beside a cleaned 4 MB PNG.
//!
//! §3: the interface never silently downgrades. When a target format cannot
//! carry the source's mode or depth, the export produces a **declaration** of
//! exactly what will change, and the caller is expected to have shown it before
//! the write.
//!
//! ## Rule 8
//!
//! Rule 8 is "Export streams too", in three parts, and two of them are built here:
//!
//! - **Per-page**, the default: each source is reconstructed at its own
//!   dimensions. [`export_page_to`] is the streaming form - it encodes straight
//!   into the sink instead of building the file as a `Vec<u8>` the caller then
//!   writes, and a passthrough copies the source through without a second
//!   buffer. [`export_page`] is the same thing over a `Vec`, kept because a
//!   caller that wants the bytes should not have to invent a sink.
//! - **Stitched**: [`stitched::export_stitched`], one file for the whole strip,
//!   written a row at a time through [`rows::write_rows`] with one page
//!   resident.
//! - **PSD**: [`psd::write_psd`], reached through [`export_page_psd_to`]. Per
//!   page only - a layered file holds the whole page as its background, which
//!   a strip cannot be.
//!   Written here rather than vendored; the module's own note says why.
//!
//! **A mask file beside the page** - `001.png` and `001_mask.png` - is
//! [`export_mask_to`]: the union of the page's **ink** masks - the lettering
//! each edit removed ([`Patch::ink`]), not the area it painted - as an 8-bit
//! grayscale PNG, white inside. The distinction is the whole file: a balloon
//! rung 0 filled has an applied mask the size of the balloon, and a typesetter
//! asking where the text was does not want the balloon. It is what the seam's
//! `masks: 'separate-layer'` means for a raster format, where the PSD path
//! means a layer mask per region - and *that* mask stays the applied one,
//! because a layer mask says what the layer contributes. Streamed through [`rows::write_rows`] like every
//! other raster here, so the stitched export can write the strip's mask the
//! same way with nothing but the patch set resident.
//!
//! [`cbz`] is a fourth thing and deliberately not a fourth encoder: a CBZ is
//! the per-page files, unaltered, inside a zip. It is the one path that holds a
//! page's encoded file whole rather than streaming it, because a stored zip
//! entry states its own CRC and length before its data; that module's note is
//! where the memory argument for it lives.
//!
//! Rule 8's remaining sentence - a patch spanning a join is split at the page
//! boundary, and the passthrough test is a per-page **intersection** with the
//! global patch set rather than patch ownership - is [`split`], and that module
//! is where the reasoning for it lives.

pub mod cbz;
pub mod gray;
pub mod psd;
pub mod rows;
pub mod split;
pub mod stitched;

use std::io::{Seek, Write};

use crate::composite::{CompositeError, composite, ordered_visible};
use crate::image::{BitDepth, ColorMode, Format, ImageError, Raster, decode};
use crate::mask::Mask;
use crate::patch::Patch;

pub use cbz::Cbz;
pub use gray::Tone;
pub use psd::{PSD_MAX_DIMENSION, PsdRefusal};
pub use rows::Canvas;
pub use split::{StripPatch, patches_on_page};
pub use stitched::{
    PageSource, StitchPlan, StitchRefusal, Stitched, export_stitched, plan_stitched,
};

/// What the caller asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Keep the source's own format. The default when the source is lossless.
    SameAsSource,
    Explicit(Format),
}

/// A change the export will make that the source did not ask for. Carries an
/// i18n key rather than a sentence, like everything else that crosses the seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Declaration {
    /// The requested format has no colour type for this mode, so another was
    /// used. Stated as a format change rather than a mode change because the
    /// mode is exactly what did *not* change - that is the point of falling
    /// back instead of converting.
    FormatChanged { requested: Format, used: Format, mode: ColorMode },
    MetadataNormalized { field: String, reason: String },
    AssumedInterpretation { interpretation: String },
}

impl Declaration {
    pub fn reason_key(&self) -> &'static str {
        match self {
            Declaration::FormatChanged { .. } => "export.declared.formatChanged",
            Declaration::MetadataNormalized { .. } => "export.declared.metadataNormalized",
            Declaration::AssumedInterpretation { .. } => "export.declared.assumedInterpretation",
        }
    }
}

#[derive(Debug)]
pub struct Export {
    pub bytes: Vec<u8>,
    pub format: Format,
    /// True when the source file's own bytes were returned untouched. Worth
    /// reporting: it is the strongest form of the fidelity contract, and §5
    /// says it should then be verified against the sha256 recorded at ingest.
    pub passthrough: bool,
    pub declared: Vec<Declaration>,
}

/// What a streamed export did. [`Export`] without the bytes, because on the
/// streaming path the bytes are already in the sink and holding them again is
/// the thing streaming exists to avoid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportSummary {
    pub format: Format,
    pub passthrough: bool,
    pub declared: Vec<Declaration>,
    /// Bytes written to the sink.
    pub written: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error(transparent)]
    Image(#[from] ImageError),
    #[error(transparent)]
    Composite(#[from] CompositeError),
    #[error(transparent)]
    NotStitchable(#[from] StitchRefusal),
    #[error(transparent)]
    NotLayerable(#[from] PsdRefusal),
    /// The sink, or a [`PageSource`], failed. Carried as a string because the
    /// failure belongs to the caller's world - a file, a socket, a manifest -
    /// and this crate has no business naming its types.
    #[error("{0}")]
    Sink(String),
}

impl From<std::io::Error> for ExportError {
    fn from(e: std::io::Error) -> ExportError {
        ExportError::Sink(e.to_string())
    }
}

/// Export one page.
///
/// `source_bytes` is the file as it sits on disk, not a decoded raster, because
/// the passthrough case must be able to return it without a re-encode - a
/// lossless re-encode satisfies §2 but is not the same file, and for a page
/// nothing touched, being the same file is the point.
pub fn export_page(source_bytes: &[u8], patches: &[Patch], target: Target) -> Result<Export, ExportError> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    let summary = export_page_to(source_bytes, patches, target, Tone::Source, &mut bytes)?;
    Ok(Export {
        bytes: bytes.into_inner(),
        format: summary.format,
        passthrough: summary.passthrough,
        declared: summary.declared,
    })
}

/// **Rule 8's per-page export**, streaming: the file is encoded straight into
/// `sink` rather than assembled and handed back.
///
/// The page is still reconstructed whole - rule 8 says "each source
/// reconstructed at its own dimensions", and a page is the unit the whole
/// memory budget is stated in. What does *not* happen is the encoded
/// file existing twice, once as a return value and once in the caller's write
/// buffer, which for a 200-page chapter of 4 MB PNGs is the difference between
/// two copies of a file and none.
///
/// A **passthrough writes the source's own bytes through**, without a
/// `to_vec()` on the way. §5's byte-for-byte copy is the strongest form of the
/// contract and copying is all it should cost.
///
/// `tone` can write a grey RGB page as grayscale ([`gray`]). Under
/// [`Tone::Grayscale`] an untouched RGB or RGBA PNG or TIFF is re-encoded
/// rather than copied, grey or not, so what the plan declared is what is
/// written; an untouched JPEG is still copied, because a JPEG is never
/// re-encoded.
///
/// `Seek` is required by the TIFF encoder, which writes its directory after the
/// data. A `File` and a `Cursor` both satisfy it.
pub fn export_page_to<W: Write + Seek>(
    source_bytes: &[u8],
    patches: &[Patch],
    target: Target,
    tone: Tone,
    sink: &mut W,
) -> Result<ExportSummary, ExportError> {
    let plan = plan_page(source_bytes, patches, target, tone)?;
    if plan.passthrough {
        sink.write_all(source_bytes)?;
        return Ok(ExportSummary {
            format: plan.format,
            passthrough: true,
            declared: plan.declared,
            written: source_bytes.len() as u64,
        });
    }

    let page = decode(source_bytes)?;
    let composited = composite(&page, patches)?;
    let composited = crate::image::orientation::from_bytes(source_bytes).raster(&composited);
    drop(page);
    let composited = tone.apply(&composited).unwrap_or(composited);

    let PagePlan { format, declared, .. } = plan;

    let start = sink.stream_position()?;
    let canvas = Canvas::of(&composited);
    let stride = composited.stride();
    rows::write_rows(sink, &canvas, format, &mut |y, row| {
        let at = y as usize * stride;
        row.data.copy_from_slice(&composited.data[at..at + stride]);
        Ok(())
    })?;

    Ok(ExportSummary {
        format,
        passthrough: false,
        declared,
        written: sink.stream_position()?.saturating_sub(start),
    })
}

/// **The page as a PSD**, straight into `sink`.
///
/// `layered` is the seam's `masks: 'separate-layer'`: the raw source as the
/// `Background` and every visible, non-empty patch as its own masked layer in
/// a `Cleaned` group, the layered shape. Flattened is one `Background` layer
/// holding the composite. Either way the merged image is the composite, so a
/// reader that ignores layers sees the cleaned page.
///
/// There is no passthrough here: a page nothing touched still becomes a PSD,
/// because the caller asked for one. And there is nothing to declare: a mode
/// PSD cannot carry is refused from the header rather than converted
/// ([`PsdRefusal`]), which the adapter decides from the manifest before it
/// creates a directory.
///
/// `tone` writes a grey page as a grayscale document. Flattened, that is the
/// composite alone. Layered, the untouched page and every region layer
/// (where its mask is set) must be grey too, so the layers always share the
/// document's mode. `options` are the user's Settings > Export choices:
/// raw or packed channels, and a hidden Lettering layer marking what the
/// mask file marks.
pub fn export_page_psd_to<W: Write>(
    source_bytes: &[u8],
    patches: &[Patch],
    layered: bool,
    tone: Tone,
    options: &psd::PsdOptions,
    sink: &mut W,
) -> Result<u64, ExportError> {
    let native = decode(source_bytes)?;
    let orientation = crate::image::orientation::from_bytes(source_bytes);
    let oriented_patches: Vec<Patch> = patches.iter().map(|p| orientation.patch(p, native.width, native.height)).collect();
    let patches = oriented_patches.as_slice();
    let page = orientation.raster(&native);
    if let Some(refusal) = psd::refusal(page.width, page.height, page.mode, page.depth) {
        return Err(refusal.into());
    }
    let composited = composite(&page, patches)?;
    let regions: Vec<&Patch> = if layered {
        ordered_visible(patches, None).into_iter().filter(|p| !p.mask.is_empty()).collect()
    } else {
        Vec::new()
    };
    let lettering = psd_lettering(options, patches, &page);
    let extras = psd::Extras { compression: options.compression, lettering: lettering.as_ref() };
    if let Some((background, composited, regions)) = gray_document(tone, layered, &page, &composited, &regions) {
        let regions: Vec<&Patch> = regions.iter().collect();
        let background = background.as_ref().unwrap_or(&composited);
        return psd::write_psd_with(sink, &psd::Document { background, regions: &regions, merged: &composited }, &extras);
    }
    let background = if layered { &page } else { &composited };
    psd::write_psd_with(sink, &psd::Document { background, regions: &regions, merged: &composited }, &extras)
}

/// The Lettering layer's mask, when the user asked for one: the visible
/// patches' lettering, on the oriented page.
fn psd_lettering(options: &psd::PsdOptions, patches: &[Patch], page: &Raster) -> Option<Mask> {
    if !options.lettering_layer {
        return None;
    }
    let masks: Vec<&Mask> = ordered_visible(patches, None).into_iter().map(Patch::lettering).collect();
    Some(Mask::union(&masks, page.width, page.height))
}

/// The document as grayscale, or `None` unless every part of it is grey: the
/// layered Background (`None` when flattened, where the composite is the
/// Background), the composite, and the region layers. A region's pixels are
/// read with the page's own colour description, which is the one they are in.
fn gray_document(tone: Tone, layered: bool, page: &Raster, composited: &Raster, regions: &[&Patch]) -> Option<(Option<Raster>, Raster, Vec<Patch>)> {
    if !tone.may_convert(page.mode) {
        return None;
    }
    let gray_composite = tone.apply(composited)?;
    if !layered {
        return Some((None, gray_composite, Vec::new()));
    }
    let gray_page = tone.apply(page)?;
    let gray_regions = regions
        .iter()
        .map(|region| {
            let described = Raster {
                icc: page.icc.clone(),
                srgb_intent: page.srgb_intent,
                color: page.color.clone(),
                ..region.pixels.clone()
            };
            let pixels = tone.apply_within(&described, &region.mask)?;
            Some(Patch { pixels, ..(*region).clone() })
        })
        .collect::<Option<Vec<Patch>>>()?;
    Some((Some(gray_page), gray_composite, gray_regions))
}

/// Every refusal [`export_page_psd_to`] makes about the document's layout,
/// from the header alone: no page is decoded.
pub fn preflight_page_psd(
    source_bytes: &[u8],
    patches: &[Patch],
    layered: bool,
    options: &psd::PsdOptions,
) -> Result<(), ExportError> {
    let header = crate::image::header(source_bytes)?;
    let orientation = crate::image::orientation::from_bytes(source_bytes);
    let (width, height) = orientation.size(header.width, header.height);
    let page = Raster {
        width,
        height,
        mode: header.mode,
        depth: header.depth,
        icc: header.icc,
        palette: header.palette,
        trns: header.trns,
        srgb_intent: header.srgb_intent,
        color: header.color,
        data: Vec::new(),
    };
    let oriented: Vec<Patch> = patches.iter().map(|p| orientation.patch(p, header.width, header.height)).collect();
    let regions: Vec<&Patch> = if layered {
        ordered_visible(&oriented, None).into_iter().filter(|p| !p.mask.is_empty()).collect()
    } else {
        Vec::new()
    };
    let lettering = psd_lettering(options, &oriented, &page);
    let extras = psd::Extras { compression: options.compression, lettering: lettering.as_ref() };
    psd::preflight_document_with(&psd::Document { background: &page, regions: &regions, merged: &page }, &extras)
}

/// **The mask file**: an 8-bit grayscale PNG of `width × height`, 255 wherever
/// any of `masks` is set and 0 elsewhere.
///
/// `masks` are in the coordinate system of the file - a page's own for the
/// per-page export, the strip's for a stitched one - and they are read
/// through, never combined: a row is built from the masks whose rectangle
/// reaches it, so the strip's mask costs one row plus the patch set.
///
/// Returns the bytes written.
pub fn export_mask_to<W: Write + Seek>(
    masks: &[&Mask],
    width: u32,
    height: u32,
    sink: &mut W,
) -> Result<u64, ExportError> {
    let canvas = Canvas {
        width,
        height,
        mode: ColorMode::Gray,
        depth: BitDepth::Eight,
        icc: None,
        palette: None,
        trns: None,
        srgb_intent: None,
        color: Default::default(),
    };
    let start = sink.stream_position()?;
    rows::write_rows(sink, &canvas, Format::Png, &mut |y, row| {
        row.data.fill(0);
        let y = i64::from(y);
        for mask in masks {
            let bounds = mask.bounds;
            if y < bounds.y || y >= bounds.bottom() {
                continue;
            }
            let x0 = bounds.x.max(0);
            let x1 = bounds.right().min(i64::from(width));
            let local = ((y - bounds.y) as usize) * bounds.w as usize;
            for x in x0..x1 {
                if mask.bits[local + (x - bounds.x) as usize] != 0 {
                    row.data[x as usize] = 255;
                }
            }
        }
        Ok(())
    })?;
    Ok(sink.stream_position()?.saturating_sub(start))
}

/// **Which format an export will produce, without producing it.**
///
/// A caller writing to a file has to name the file before it opens it, and the
/// name carries the extension. Predicting the answer with a second copy of the
/// rule is how the two start to disagree, so this is the *same* decision
/// [`export_page_to`] makes, reached from a header read rather than from a
/// decode. `export::tests::the_predicted_format_is_the_one_the_export_produces`
/// pins that on every fixture.
pub fn planned_format(
    source_bytes: &[u8],
    patches: &[Patch],
    target: Target,
) -> Result<Format, ExportError> {
    // The format never depends on the tone.
    Ok(plan_page(source_bytes, patches, target, Tone::Source)?.format)
}

/// The shared, read-only decision consumed by both naming and execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PagePlan {
    pub format: Format,
    pub passthrough: bool,
    pub declared: Vec<Declaration>,
}

pub fn plan_page(source_bytes: &[u8], patches: &[Patch], target: Target, tone: Tone) -> Result<PagePlan, ExportError> {
    let source_format = Format::sniff(source_bytes).ok_or(ImageError::UnknownFormat)?;
    let applied = patches.iter().any(|p| p.visible && !p.mask.is_empty());
    let wanted = match target {
        Target::SameAsSource => source_format,
        Target::Explicit(format) => format,
    };
    if !applied && wanted == source_format {
        // A tone that may convert the page re-encodes it, except a JPEG.
        let toned = tone == Tone::Grayscale
            && source_format != Format::Jpeg
            && tone.may_convert(crate::image::header(source_bytes)?.mode);
        if !toned {
            return Ok(PagePlan { format: source_format, passthrough: true, declared: Vec::new() });
        }
    }
    let header = crate::image::header(source_bytes)?;
    crate::image::metadata::validate_profile(header.icc.as_deref(), header.mode)?;
    let (format, mut declared) = if header.color.associated_alpha && wanted != Format::Tiff {
        (Format::Tiff, vec![Declaration::FormatChanged { requested: wanted, used: Format::Tiff, mode: header.mode }])
    } else { decide(wanted, header.mode, header.depth) };
    declared.extend(metadata_declarations(&header, applied, Writer::of(format)));
    if tone == Tone::Grayscale {
        declared.extend(gray::declarations(&header));
    }
    if crate::image::foreign::assumes_unmarked_cmyk(source_bytes) {
        declared.push(Declaration::AssumedInterpretation { interpretation: "four-channel JPEG without Adobe marker interpreted as non-inverted CMYK; markerless YCCK cannot be identified reliably".into() });
    }
    if crate::image::exif::payload(source_bytes).is_some() && (Format::sniff(source_bytes)!=Some(Format::Tiff) || crate::image::exif::payload(source_bytes).and_then(crate::image::exif::portable).is_some() || crate::image::orientation::from_bytes(source_bytes).0!=1) {
        declared.push(Declaration::MetadataNormalized { field: "EXIF".into(), reason: "portable descriptive fields retained in PNG; orientation, geometry, thumbnails and opaque EXIF fields omitted; complete metadata remains in original".into() });
    }
    if crate::image::orientation::from_bytes(source_bytes).0 != 1 {
        declared.push(Declaration::MetadataNormalized { field: "EXIF orientation".into(), reason: "samples oriented for output; stale EXIF geometry and thumbnails omitted".into() });
    }
    Ok(PagePlan { format, passthrough: false, declared })
}

/// Metadata decisions for PSD, whose format is fixed but color declarations still matter.
pub fn psd_declarations(source_bytes: &[u8], patches: &[Patch], tone: Tone) -> Result<Vec<Declaration>, ExportError> {
    let header = crate::image::header(source_bytes)?;
    crate::image::metadata::validate_profile(header.icc.as_deref(), header.mode)?;
    let mut declared = metadata_declarations(&header, patches.iter().any(|p| p.visible && !p.mask.is_empty()), Writer::Psd);
    if tone == Tone::Grayscale {
        declared.extend(gray::declarations(&header));
    }
    if crate::image::foreign::assumes_unmarked_cmyk(source_bytes) {
        declared.push(Declaration::AssumedInterpretation { interpretation: "four-channel JPEG without Adobe marker interpreted as non-inverted CMYK; markerless YCCK cannot be identified reliably".into() });
    }
    if crate::image::exif::payload(source_bytes).is_some() && (Format::sniff(source_bytes)!=Some(Format::Tiff) || crate::image::exif::payload(source_bytes).and_then(crate::image::exif::portable).is_some() || crate::image::orientation::from_bytes(source_bytes).0!=1) {
        declared.push(Declaration::MetadataNormalized { field: "EXIF".into(), reason: "portable descriptive fields retained in PNG; orientation, geometry, thumbnails and opaque EXIF fields omitted; complete metadata remains in original".into() });
    }
    if crate::image::orientation::from_bytes(source_bytes).0 != 1 {
        declared.push(Declaration::MetadataNormalized { field: "EXIF orientation".into(), reason: "samples and layers oriented; stale EXIF geometry and thumbnails omitted".into() });
    }
    Ok(declared)
}

/// The writer a declaration is for: what PNG description it can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Writer {
    Png,
    /// An equivalent ICC profile, and the `pHYs` resolution.
    Tiff,
    /// An equivalent ICC profile.
    Psd,
}

impl Writer {
    pub(crate) fn of(format: Format) -> Writer {
        if format == Format::Tiff { Writer::Tiff } else { Writer::Png }
    }
}

pub(crate) fn metadata_declarations(header: &crate::image::Header, applied: bool, writer: Writer) -> Vec<Declaration> {
    let equivalent_icc = writer != Writer::Png;
    let mut declared = Vec::new();
    if header.icc.is_some() && header.srgb_intent.is_some() {
        declared.push(Declaration::MetadataNormalized { field: "sRGB".into(), reason: "the higher-priority color description is authoritative".into() });
    }
    if header.icc.is_none() && header.color.cicp.is_none() && header.srgb_intent.is_some() {
        if header.color.gamma.is_some_and(|gamma| gamma != 45455) {
            declared.push(Declaration::MetadataNormalized { field: "gAMA".into(), reason: "normalized to the authoritative sRGB transfer".into() });
        }
        if header.color.chromaticities.is_some_and(|chrm| chrm != [31270,32900,64000,33000,30000,60000,15000,6000]) {
            declared.push(Declaration::MetadataNormalized { field: "cHRM".into(), reason: "normalized to the authoritative sRGB primaries".into() });
        }
    }
    if applied {
        for (present, field) in [(header.color.significant_bits.is_some(), "sBIT"), (header.color.content_light.is_some(), "cLLI")] {
            if present { declared.push(Declaration::MetadataNormalized { field: field.into(), reason: "editing or stitching invalidates the original summary".into() }); }
        }
    }
    if header.icc.is_none() && header.srgb_intent.is_none() && header.color.gamma.is_none() && header.color.cicp.is_none() {
        declared.push(Declaration::AssumedInterpretation { interpretation: match header.mode {
            ColorMode::Cmyk => "multiplicative-ink CMYK preview approximation; native numbers retained",
            ColorMode::Gray | ColorMode::GrayAlpha => "sRGB gray transfer for display",
            _ => "sRGB for display",
        }.into() });
    }
    if equivalent_icc {
        let mut omitted = Vec::new();
        if header.color.mastering_display.is_some() { omitted.push("mDCV".to_owned()); }
        if !applied && header.color.content_light.is_some() { omitted.push("cLLI".to_owned()); }
        if !applied && header.color.significant_bits.is_some() { omitted.push("sBIT".to_owned()); }
        let carried = |chunk: &&crate::image::metadata::AncillaryChunk| writer == Writer::Tiff && chunk.name == *b"pHYs";
        omitted.extend(header.color.safe_ancillary.iter().filter(|chunk| !carried(chunk)).map(|chunk| String::from_utf8_lossy(&chunk.name).into_owned()));
        for field in omitted {
            declared.push(Declaration::MetadataNormalized { field, reason: "this PNG ancillary field has no representation in the selected output; retained in original".into() });
        }
    }
    if equivalent_icc && (header.color.cicp.is_some() || (header.icc.is_none() && (header.color.gamma.is_some() || header.color.chromaticities.is_some() || header.srgb_intent.is_some()))) {
        declared.push(Declaration::MetadataNormalized { field: "ICC".into(), reason: "equivalent profile carries PNG color description in the output".into() });
    }
    declared
}

/// The format an export uses, and what it has to declare for using it.
///
/// A format that cannot carry the mode is a declaration, not a silent
/// conversion - and today there is exactly one such pair, CMYK into PNG, so the
/// honest answer is to fall back and say which format would work.
fn decide(wanted: Format, mode: ColorMode, depth: BitDepth) -> (Format, Vec<Declaration>) {
    if can_carry(wanted, mode, depth) {
        return (wanted, Vec::new());
    }
    let used = fallback_format(mode);
    (used, vec![Declaration::FormatChanged { requested: wanted, used, mode }])
}

/// The format that can carry a mode with nothing declared away. The same rule
/// as [`crate::image::lossless_format_for`], stated over a mode rather than
/// over a raster, because the prediction above has no raster.
fn fallback_format(mode: ColorMode) -> Format {
    match mode {
        ColorMode::Cmyk => Format::Tiff,
        _ => Format::Png,
    }
}

fn can_carry(format: Format, mode: ColorMode, depth: BitDepth) -> bool {
    match format {
        Format::Png => mode != ColorMode::Cmyk,
        Format::Jpeg => false,
        Format::Tiff => {
            mode != ColorMode::Indexed && depth >= BitDepth::Eight
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composite::{changed_pixels, permitted_region};
    use crate::image::{Raster, encode, fixtures, lossless_format_for};

    #[test]
    fn jpeg_passthrough_and_edited_lossless_output_preserve_color() {
        use image::ImageEncoder;
        let original = fixtures::by_name("rgb8-icc").raster;
        let mut source = Vec::new();
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut source, 95);
        encoder.set_icc_profile(original.icc.clone().unwrap()).unwrap();
        encoder
            .encode(&original.data, original.width, original.height, image::ExtendedColorType::Rgb8)
            .unwrap();
        let page = decode(&source).unwrap();
        let patches = vec![patch_over(&page, Rect::new(8, 8, 24, 20))];
        for target in [Target::SameAsSource, Target::Explicit(Format::Png), Target::Explicit(Format::Tiff)] {
            for edits in [&[][..], patches.as_slice()] {
                let output = export_page(&source, edits, target).unwrap();
                assert_eq!(planned_format(&source, edits, target).unwrap(), output.format);
                if target == Target::SameAsSource && edits.is_empty() {
                    assert_eq!(output.bytes, source);
                    assert_eq!(output.format, Format::Jpeg);
                }
                let decoded = decode(&output.bytes).unwrap();
                assert_eq!(decoded.icc, page.icc);
                assert_eq!(decoded.mode, page.mode);
                assert_eq!(decoded.depth, page.depth);
                let allowed = permitted_region(edits, page.width, page.height);
                for (x, y) in changed_pixels(&page, &decoded) {
                    assert!(
                        allowed.contains(x as i64, y as i64),
                        "a JPEG sample outside edits changed at {x},{y}"
                    );
                }
            }
        }
    }
    use crate::mask::{Mask, Rect};
    use crate::patch::{Engine, Provenance};

    fn provenance() -> Provenance {
        Provenance {
            engine: Engine::Fill,
            engine_version: "test".into(),
            model_sha256: None,
            execution_provider: "cpu".into(),
            params_snapshot: "{}".into(),
            mask_sha256: "0".repeat(64),
            source_sha256: "0".repeat(64),
            cloud: None,
            created: 0,
        }
    }

    /// Fills the masked area with a value the source does not have there, so
    /// the change is detectable on every fixture including the indexed one.
    fn patch_over(page: &Raster, bounds: Rect) -> Patch {
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
            data: vec![0; {
                let bits = bounds.w as usize * page.mode.samples() * page.depth.bits() as usize;
                bits.div_ceil(8) * bounds.h as usize
            }],
        };
        for y in 0..bounds.h {
            for x in 0..bounds.w {
                for channel in 0..page.mode.samples() {
                    let source = page.sample(bounds.x as u32 + x, bounds.y as u32 + y, channel);
                    let value = match page.mode {
                        ColorMode::Indexed => {
                            let entries = page.palette.as_ref().map_or(1, |p| p.len() / 3) as u16;
                            (source + 1) % entries
                        }
                        _ => {
                            let ceiling = if page.depth == crate::image::BitDepth::Sixteen {
                                u16::MAX
                            } else {
                                (1u16 << page.depth.bits()) - 1
                            };
                            ceiling - source
                        }
                    };
                    pixels.set_sample(x, y, channel, value);
                }
            }
        }
        let mut mask = Mask::empty(bounds);
        for y in bounds.y + 1..bounds.bottom() - 1 {
            for x in bounds.x + 1..bounds.right() - 1 {
                mask.set(x, y, true);
            }
        }
        Patch { id: "p1".into(), ink: mask.clone(), mask, pixels, order: 0, visible: true, provenance: provenance() }
    }

    /// The Phase 0 exit criterion, in full:
    /// every fixture, under **both** an empty and a synthetic non-empty patch
    /// set, keeps its mode, depth and profile, and changes only pixels inside
    /// `dilate(union(masks), edit_margin)`.
    ///
    /// Revision 1 required only the empty case, which passthrough satisfies
    /// with `cp` while never exercising the subset assertion the whole contract
    /// rests on. Both run here.
    #[test]
    fn every_fixture_survives_export_under_both_patch_sets() {
        for fixture in fixtures::all() {
            let page = &fixture.raster;
            let source_bytes = encode(page, lossless_format_for(page)).unwrap();

            for (label, patches) in [
                ("empty", Vec::new()),
                ("non-empty", vec![patch_over(page, Rect::new(8, 8, 24, 20))]),
            ] {
                let export = export_page(&source_bytes, &patches, Target::SameAsSource).unwrap();
                let out = decode(&export.bytes).unwrap();
                let name = format!("{} / {label}", fixture.name);

                assert_eq!(out.mode, page.mode, "{name}: mode");
                assert_eq!(out.depth, page.depth, "{name}: bit depth");
                assert_eq!(out.icc, page.icc, "{name}: icc profile");
                assert_eq!(out.palette, page.palette, "{name}: palette");

                let changed = changed_pixels(page, &out);
                if patches.is_empty() {
                    assert!(export.passthrough, "{name}: an untouched page was re-encoded");
                    assert_eq!(export.bytes, source_bytes, "{name}: passthrough changed the file");
                    assert!(changed.is_empty(), "{name}: an empty patch set changed pixels");
                } else {
                    assert!(!changed.is_empty(), "{name}: the patch changed nothing");
                    let permitted = permitted_region(&patches, page.width, page.height);
                    for (x, y) in &changed {
                        assert!(
                            permitted.contains(*x as i64, *y as i64),
                            "{name}: ({x}, {y}) changed outside the permitted region"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn passthrough_respects_a_chosen_format_rather_than_copying_regardless() {
        // The failure mode is a folder holding an untouched source
        // beside a cleaned page in another format.
        let page = fixtures::by_name("rgb8").raster;
        let source_bytes = encode(&page, Format::Png).unwrap();

        let same = export_page(&source_bytes, &[], Target::SameAsSource).unwrap();
        assert!(same.passthrough);
        assert_eq!(same.format, Format::Png);

        let converted = export_page(&source_bytes, &[], Target::Explicit(Format::Tiff)).unwrap();
        assert!(!converted.passthrough, "a format change is not a passthrough");
        assert_eq!(converted.format, Format::Tiff);
        assert_eq!(decode(&converted.bytes).unwrap().data, page.data);
    }

    #[test]
    fn cmyk_asked_for_as_png_is_declared_rather_than_converted_silently() {
        let page = fixtures::by_name("cmyk8").raster;
        let source_bytes = encode(&page, Format::Tiff).unwrap();
        let export = export_page(&source_bytes, &[], Target::Explicit(Format::Png)).unwrap();
        assert_eq!(export.format, Format::Tiff, "fell back to a format that can carry it");
        assert_eq!(export.declared.len(), 1);
        assert_eq!(
            export.declared[0],
            Declaration::FormatChanged { requested: Format::Png, used: Format::Tiff, mode: ColorMode::Cmyk }
        );
    }

    /// A caller writing to a file names it before it opens it, and the name
    /// carries the extension. That prediction is only safe while it *is* the
    /// export's own decision, so this asserts the two agree on every fixture
    /// under every target - including the CMYK-into-PNG pair, which is the one
    /// case where the answer is not the requested format.
    #[test]
    fn the_predicted_format_is_the_one_the_export_produces() {
        for fixture in fixtures::all() {
            let page = &fixture.raster;
            let source_bytes = encode(page, lossless_format_for(page)).unwrap();
            for target in
                [Target::SameAsSource, Target::Explicit(Format::Png), Target::Explicit(Format::Tiff)]
            {
                for patches in [Vec::new(), vec![patch_over(page, Rect::new(8, 8, 24, 20))]] {
                    let predicted = planned_format(&source_bytes, &patches, target).unwrap();
                    // A format neither the source nor the fallback can carry -
                    // gray+alpha into TIFF - is refused by the encoder rather
                    // than mispredicted, and `export_page` reports it.
                    let Ok(export) = export_page(&source_bytes, &patches, target) else { continue };
                    assert_eq!(
                        export.format, predicted,
                        "{} / {target:?}: the file would have been misnamed",
                        fixture.name
                    );
                }
            }
        }
    }

    /// Streaming and buffering produce the same file, so every assertion made
    /// of `export_page` above holds of `export_page_to`.
    #[test]
    fn streaming_a_page_into_a_sink_writes_what_export_page_returns() {
        for fixture in fixtures::all() {
            let page = &fixture.raster;
            let source_bytes = encode(page, lossless_format_for(page)).unwrap();
            for patches in [Vec::new(), vec![patch_over(page, Rect::new(8, 8, 24, 20))]] {
                let buffered = export_page(&source_bytes, &patches, Target::SameAsSource).unwrap();
                let mut sink = std::io::Cursor::new(Vec::new());
                let summary =
                    export_page_to(&source_bytes, &patches, Target::SameAsSource, Tone::Source, &mut sink)
                        .unwrap();
                let streamed = sink.into_inner();
                assert_eq!(streamed, buffered.bytes, "{}", fixture.name);
                assert_eq!(summary.format, buffered.format, "{}", fixture.name);
                assert_eq!(summary.passthrough, buffered.passthrough, "{}", fixture.name);
                assert_eq!(summary.written, streamed.len() as u64, "{}", fixture.name);
            }
        }
    }

    #[test]
    fn a_patch_with_an_empty_mask_still_counts_as_an_untouched_page() {
        // A region that was examined and left alone must not force a re-encode:
        // §5's byte-for-byte copy is the strongest form of the contract.
        let page = fixtures::by_name("l8").raster;
        let source_bytes = encode(&page, Format::Png).unwrap();
        let mut patch = patch_over(&page, Rect::new(8, 8, 8, 8));
        patch.mask = Mask::empty(patch.mask.bounds);
        let export = export_page(&source_bytes, &[patch], Target::SameAsSource).unwrap();
        assert!(export.passthrough);
    }

    /// The mask file is the union of the masks, white inside, the page's
    /// size, and nothing else - and a mask whose rectangle runs off the page
    /// is clipped rather than indexed past the row.
    #[test]
    fn the_mask_file_is_the_union_of_the_masks_in_white() {
        let page = fixtures::by_name("l8").raster;
        let a = patch_over(&page, Rect::new(8, 8, 24, 20));
        // Off the page's right and bottom edges, as a strip-coordinate mask
        // that spans a join looks from the page below it.
        let b = Mask::filled(Rect::new(40, 30, 40, 40));
        let masks = [&a.mask, &b];

        let mut sink = std::io::Cursor::new(Vec::new());
        let written = export_mask_to(&masks, page.width, page.height, &mut sink).unwrap();
        let bytes = sink.into_inner();
        assert_eq!(written, bytes.len() as u64);

        let mask = decode(&bytes).unwrap();
        assert_eq!((mask.width, mask.height), (page.width, page.height));
        assert_eq!((mask.mode, mask.depth), (ColorMode::Gray, BitDepth::Eight));
        for y in 0..page.height {
            for x in 0..page.width {
                let inside = a.mask.contains(x as i64, y as i64) || b.contains(x as i64, y as i64);
                assert_eq!(mask.sample(x, y, 0), if inside { 255 } else { 0 }, "({x}, {y})");
            }
        }

        let mut empty = std::io::Cursor::new(Vec::new());
        export_mask_to(&[], page.width, page.height, &mut empty).unwrap();
        let blank = decode(&empty.into_inner()).unwrap();
        assert!(blank.data.iter().all(|b| *b == 0), "a page with no masks is all black");
    }

    /// A fixture with every colour channel set to its first: grey stored as
    /// RGB or RGBA.
    fn grey(name: &str) -> Raster {
        let mut page = fixtures::by_name(name).raster;
        for y in 0..page.height {
            for x in 0..page.width {
                let v = page.sample(x, y, 0);
                page.set_sample(x, y, 1, v);
                page.set_sample(x, y, 2, v);
            }
        }
        page
    }

    #[test]
    fn grayscale_tone_writes_a_grey_page_gray_and_leaves_colour_alone() {
        for name in ["rgb8", "rgba8", "rgb16"] {
            let page = grey(name);
            let source = encode(&page, Format::Png).unwrap();
            let edits = vec![patch_over(&page, Rect::new(8, 8, 24, 20))];
            for patches in [&[][..], edits.as_slice()] {
                for target in [Target::SameAsSource, Target::Explicit(Format::Tiff)] {
                    let mut sink = std::io::Cursor::new(Vec::new());
                    let summary = export_page_to(&source, patches, target, Tone::Grayscale, &mut sink).unwrap();
                    assert!(!summary.passthrough, "{name}");
                    let written = decode(&sink.into_inner()).unwrap();
                    let expected = composite(&page, patches).unwrap();
                    assert_eq!(written.mode.samples(), page.mode.samples() - 2, "{name}");
                    assert_eq!(written.depth, page.depth, "{name}");
                    for y in 0..page.height {
                        for x in 0..page.width {
                            assert_eq!(written.sample(x, y, 0), expected.sample(x, y, 0), "{name} at {x},{y}");
                            if let Some(alpha) = page.mode.alpha_channel() {
                                assert_eq!(written.sample(x, y, 1), expected.sample(x, y, alpha), "{name} alpha");
                            }
                        }
                    }
                }
                // Source tone is the default and keeps RGB, untouched bytes included.
                let output = export_page(&source, patches, Target::SameAsSource).unwrap();
                assert_eq!(decode(&output.bytes).unwrap().mode, page.mode, "{name}");
                assert_eq!(output.passthrough, patches.is_empty(), "{name}");
            }
        }
        // A colour page is re-encoded, as its plan said, and keeps its pixels.
        let page = fixtures::by_name("rgb8").raster;
        let colour = encode(&page, Format::Png).unwrap();
        assert!(!plan_page(&colour, &[], Target::SameAsSource, Tone::Grayscale).unwrap().passthrough);
        let mut sink = std::io::Cursor::new(Vec::new());
        let summary = export_page_to(&colour, &[], Target::SameAsSource, Tone::Grayscale, &mut sink).unwrap();
        assert!(!summary.passthrough);
        let written = decode(&sink.into_inner()).unwrap();
        assert_eq!((written.mode, written.data), (page.mode, page.data));
        assert!(plan_page(&colour, &[], Target::SameAsSource, Tone::Source).unwrap().passthrough);
    }

    #[test]
    fn grayscale_tone_declares_what_a_toned_untouched_page_loses() {
        let mut page = grey("rgb8");
        page.color.significant_bits = Some(vec![8, 8, 8]);
        let source = encode(&page, Format::Png).unwrap();
        let plan = plan_page(&source, &[], Target::SameAsSource, Tone::Grayscale).unwrap();
        assert!(!plan.passthrough);
        assert!(plan.declared.iter().any(|d| matches!(d, Declaration::MetadataNormalized { field, .. } if field == "sBIT")), "{:?}", plan.declared);
        let mut sink = std::io::Cursor::new(Vec::new());
        let summary = export_page_to(&source, &[], Target::SameAsSource, Tone::Grayscale, &mut sink).unwrap();
        assert_eq!(summary.declared, plan.declared, "the write declares what the plan did");
        assert!(psd_declarations(&source, &[], Tone::Grayscale).unwrap().iter().any(|d| matches!(d, Declaration::MetadataNormalized { field, .. } if field == "sBIT")));
    }

    #[test]
    fn grayscale_tone_makes_a_gray_psd_only_when_every_layer_is_grey() {
        // PSD header: channels at byte 12, colour mode at byte 24 (1 gray, 3 RGB).
        let header = |bytes: &[u8]| (u16::from_be_bytes([bytes[12], bytes[13]]), u16::from_be_bytes([bytes[24], bytes[25]]));
        let page = grey("rgba8");
        let source = encode(&page, Format::Png).unwrap();
        let patches = vec![patch_over(&page, Rect::new(8, 8, 24, 20))];
        for layered in [false, true] {
            let mut gray = Vec::new();
            export_page_psd_to(&source, &patches, layered, Tone::Grayscale, &Default::default(), &mut gray).unwrap();
            assert_eq!(header(&gray), (2, 1), "gray with alpha");
            let mut kept = Vec::new();
            export_page_psd_to(&source, &patches, layered, Tone::Source, &Default::default(), &mut kept).unwrap();
            assert_eq!(header(&kept), (4, 3));
        }
        let mut coloured = patches.clone();
        coloured[0].pixels.set_sample(10, 10, 0, 0);
        coloured[0].pixels.set_sample(10, 10, 1, 255);
        let mut psd = Vec::new();
        export_page_psd_to(&source, &coloured, true, Tone::Grayscale, &Default::default(), &mut psd).unwrap();
        assert_eq!(header(&psd), (4, 3), "a colour edit keeps the document RGB");

        // Colour a region hides outside its mask never shows, so it is no reason to keep RGB.
        let mut hidden = patches.clone();
        hidden[0].pixels.set_sample(0, 0, 0, 0);
        hidden[0].pixels.set_sample(0, 0, 1, 255);
        let mut psd = Vec::new();
        export_page_psd_to(&source, &hidden, true, Tone::Grayscale, &Default::default(), &mut psd).unwrap();
        assert_eq!(header(&psd), (2, 1), "colour outside the mask");

        // Colour the cleaning covered: a flattened document is gray, a layered
        // one keeps it in its Background and stays RGB.
        let mut stained = page.clone();
        stained.set_sample(10, 10, 0, 0);
        stained.set_sample(10, 10, 1, 255);
        let stained = encode(&stained, Format::Png).unwrap();
        for (layered, expected) in [(false, (2, 1)), (true, (4, 3))] {
            let mut psd = Vec::new();
            export_page_psd_to(&stained, &patches, layered, Tone::Grayscale, &Default::default(), &mut psd).unwrap();
            assert_eq!(header(&psd), expected, "layered {layered}");
        }
    }
}
