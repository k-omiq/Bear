//! The stitched export: one file for the whole strip, written a row at a time.
//!
//! Rule 8:
//!
//! > **Stitched**: incremental row-wise encoder or tiled TIFF.
//!
//! under rule 1's standing prohibition, which this is the hardest case of:
//!
//! > Nothing concatenates pixels, at any point, including export.
//!
//! So there is no strip buffer here. The writer holds **one page** - the one the
//! rows are currently passing through - composited with the parts of the global
//! patch set that land on it, and drops it when the rows move onto the next.
//! What that page cost is what the stitched export costs, whether the strip is
//! three pages or two hundred, which is the property the whole memory budget
//! exists to keep.
//!
//! ## The gutter is the one thing here that is not a source pixel
//!
//! Pages are centred (rule 1), so a strip whose pages are not all the same
//! width has columns beside the narrow ones that no source file has a pixel
//! for. A stitched file has to put *something* there. This writes **paper
//! white** - every colour channel at its maximum, or at zero for CMYK where
//! zero is no ink, and alpha at zero where the mode has one, so a mode that can
//! say "nothing here" says it rather than inventing a colour.
//!
//! That is an invented pixel either way, and the fidelity contract has
//! nothing to say about it: the contract is stated per source file, and a
//! gutter column
//! is in no source file. So it is **counted** instead -
//! [`Stitched::gutter_pixels`] is how many were written - and an interface that
//! offers stitched output is expected to say so before it runs, exactly as §3
//! requires of every other change the user did not ask for. Per-page export,
//! which is rule 8's default, has no gutter and no such statement to make.
//!
//! ## What it refuses
//!
//! One file has one colour mode, one bit depth and one profile. A strip whose
//! pages disagree about any of those cannot be stitched without converting
//! something, and §2's second clarification - "colour mode is never *upgraded*" -
//! forbids picking the wider of two. Indexed is refused outright: two pages
//! with different palettes would need a reconciled palette, and the
//! nearest-entry snapping that requires is refused.
//!
//! Every one of those is decided from **headers**, before a byte is written. A
//! refusal halfway through leaves a truncated file that looks like a crash.

use std::io::{Seek, Write};

use crate::composite::composite;
use crate::image::{ColorMode, Format, Header, Raster};
use crate::mask::Rect;
use crate::strip::Strip;

use super::rows::{Canvas, write_rows};
use super::split::{StripPatch, patches_on_page};
use super::{Declaration, ExportError, Target};

/// Where the writer gets pages from.
///
/// Two methods rather than one because the refusals above are decided from
/// headers and the rows are written from pixels, and asking for pixels to
/// answer a header question would decode the whole strip to find out it cannot
/// be stitched.
///
/// `position` is an index into [`Strip::pages`], not into a project's source
/// list. [`Strip::of`] is where the two are related.
pub trait PageSource {
    /// What the page's header says. Called once per page, before anything is
    /// written.
    fn header(&mut self, position: usize) -> Result<Header, ExportError>;

    /// The page's pixels. Called once per page, in strip order, and the result
    /// is dropped before the next call - which is the whole memory argument.
    fn page(&mut self, position: usize) -> Result<Raster, ExportError>;
}

/// Why a strip cannot be written as one file.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StitchRefusal {
    #[error("a strip with no pages has nothing to stitch")]
    NoPages,
    #[error("page {position} is {found:?} but the strip is {first:?}")]
    MixedMode { position: usize, first: ColorMode, found: ColorMode },
    #[error("page {position} is {found}-bit but the strip is {first}-bit")]
    MixedDepth { position: usize, first: u8, found: u8 },
    #[error("page {position} carries a different ICC profile from page 0")]
    MixedProfile { position: usize },
    #[error("page {position} has a different color or transparency description; use per-page export")]
    MixedDescription { position: usize },
    /// Not a limitation of the encoder - PNG carries indexed perfectly well.
    /// One file has one palette, and reconciling several is
    /// the nearest-entry snapping that is refused.
    #[error("an indexed strip would need one palette across pages")]
    Indexed,
}

impl StitchRefusal {
    /// The catalogue key for this refusal, so an interface can say why rather
    /// than report that something went wrong.
    ///
    /// **The sentences name the condition and not the page.** Three of these
    /// variants carry a `position`, and a key alone crosses the seam -
    /// `exportChapter` answers `{status, fileCount, path, reasonKey}`
    /// and has nowhere to put the
    /// number. Naming the page in the sentence and not in the parameters would
    /// be worse than leaving it out: the reader would believe a page had been
    /// identified. The `Display` impls above still carry it for a log.
    pub fn reason_key(&self) -> &'static str {
        match self {
            StitchRefusal::NoPages => "notice.export.stitchRefused.noPages",
            StitchRefusal::MixedMode { .. } => "notice.export.stitchRefused.mixedMode",
            StitchRefusal::MixedDepth { .. } => "notice.export.stitchRefused.mixedDepth",
            StitchRefusal::MixedProfile { .. } => "notice.export.stitchRefused.mixedProfile",
            StitchRefusal::MixedDescription { .. } => "notice.export.stitchRefused.mixedProfile",
            StitchRefusal::Indexed => "notice.export.stitchRefused.indexed",
        }
    }
}

/// What a stitched export produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stitched {
    pub format: Format,
    pub width: u32,
    pub height: u32,
    /// How many pixels of the file no source page had - see the module note.
    /// Zero for a strip whose pages are all the strip's width, which is every
    /// webtoon chapter cut from one canvas.
    pub gutter_pixels: u64,
    pub declared: Vec<Declaration>,
}

/// Write the whole strip as one file.
///
/// The patch set is **global**, in strip coordinates, and each page's parts are
/// taken from it by [`patches_on_page`] - the same intersection the per-page
/// path uses, so a patch spanning a join lands in the stitched file exactly
/// where it lands in the two per-page files.
pub fn export_stitched<W: Write + Seek>(
    strip: &Strip,
    global: &[StripPatch],
    pages: &mut dyn PageSource,
    target: Target,
    sink: &mut W,
) -> Result<Stitched, ExportError> {
    let placements = strip.pages().to_vec();
    if placements.is_empty() {
        return Err(StitchRefusal::NoPages.into());
    }

    let first = pages.header(0)?;
    if first.mode == ColorMode::Indexed {
        return Err(StitchRefusal::Indexed.into());
    }
    for position in 1..placements.len() {
        let header = pages.header(position)?;
        if header.mode != first.mode {
            return Err(StitchRefusal::MixedMode {
                position,
                first: first.mode,
                found: header.mode,
            }
            .into());
        }
        if header.depth != first.depth {
            return Err(StitchRefusal::MixedDepth {
                position,
                first: first.depth.bits(),
                found: header.depth.bits(),
            }
            .into());
        }
        if !header.color.same_interpretation(&first.color) || header.srgb_intent != first.srgb_intent || header.trns != first.trns {
            return Err(StitchRefusal::MixedDescription { position }.into());
        }
        crate::image::metadata::validate_profile(header.icc.as_deref(), header.mode)?;
        if header.icc != first.icc {
            return Err(StitchRefusal::MixedProfile { position }.into());
        }
    }

    crate::image::metadata::validate_profile(first.icc.as_deref(), first.mode)?;
    // Page 0 is decoded before the header is written, for the one piece of
    // metadata a header read does not carry: the `sRGB` rendering intent. It is
    // also the page the first rows need, so nothing is decoded twice.
    let mut resident = Resident::load(strip, global, pages, 0)?;

    let canvas = Canvas {
        width: strip.width(),
        height: strip.height(),
        mode: first.mode,
        depth: first.depth,
        icc: first.icc.clone(),
        palette: None,
        trns: first.trns.clone(),
        srgb_intent: resident.page.srgb_intent,
        color: if global.is_empty() && strip.pages().len() == 1 { first.color.clone() } else { first.color.after_edit() },
    };

    let (format, mut declared) = decide_stitched(&first, target);
    declared.extend(super::metadata_declarations(&first, !global.is_empty() || strip.pages().len() > 1, format == Format::Tiff));

    let blank = blank_row(&canvas);
    let mut gutter_pixels = 0u64;

    write_rows(sink, &canvas, format, &mut |y, row| {
        let position = strip.page_at(y as i64).expect("a strip row belongs to a page");
        if position != resident.position {
            resident = Resident::load(strip, global, pages, position)?;
        }
        let placement = placements[position];
        gutter_pixels += (canvas.width - placement.width) as u64;
        fill_row(row, &blank, &resident.page, placement.rect(), y);
        Ok(())
    })?;

    Ok(Stitched {
        format,
        width: canvas.width,
        height: canvas.height,
        gutter_pixels,
        declared,
    })
}

/// **Which format a stitched export will produce, without producing it.**
///
/// The per-page sibling of this is [`super::planned_format`], and it exists for
/// the same reason: a caller writing to a file has to name the file - extension
/// and all - before it opens it, and predicting the answer with a second copy
/// of the rule is how the two start to disagree. `export_stitched` calls this
/// rather than deciding again, so the prediction is not a prediction.
///
/// `first` is page 0's header, which is also the strip's: every other page has
/// already been refused if it disagrees.
pub fn planned_stitched_format(first: &Header, target: Target) -> Format {
    decide_stitched(first, target).0
}

/// "Same as source" has no single source file here, so it means the format that
/// carries the strip's mode with nothing declared away - which for CMYK is
/// TIFF, the format already named for stitched longstrip anyway.
fn decide_stitched(first: &Header, target: Target) -> (Format, Vec<Declaration>) {
    let wanted = match target {
        Target::SameAsSource => super::fallback_format(first.mode),
        Target::Explicit(format) => format,
    };
    if first.color.associated_alpha && wanted != Format::Tiff {
        (Format::Tiff, vec![Declaration::FormatChanged { requested: wanted, used: Format::Tiff, mode: first.mode }])
    } else { super::decide(wanted, first.mode, first.depth) }
}

/// The one page the writer holds, already composited with its parts of the
/// global patch set.
struct Resident {
    position: usize,
    page: Raster,
}

impl Resident {
    fn load(
        strip: &Strip,
        global: &[StripPatch],
        pages: &mut dyn PageSource,
        position: usize,
    ) -> Result<Resident, ExportError> {
        let page = pages.page(position)?;
        let patches = patches_on_page(strip, position, global);
        Ok(Resident { position, page: composite(&page, &patches)? })
    }
}

/// One strip row: the gutter, then the page's own samples over it.
fn fill_row(row: &mut Raster, blank: &[u8], page: &Raster, rect: Rect, y: u32) {
    let line = y as i64 - rect.y;
    // The common case, and a webtoon chapter is nothing but this case: the page
    // is the full width of the strip, so the row is the page's row.
    if rect.x == 0 && rect.w == row.width {
        let stride = page.stride();
        let at = line as usize * stride;
        row.data.copy_from_slice(&page.data[at..at + stride]);
        return;
    }

    row.data.copy_from_slice(blank);
    let samples = page.mode.samples();
    for x in rect.x..rect.right() {
        for channel in 0..samples {
            row.set_sample(x as u32, 0, channel, page.sample((x - rect.x) as u32, line as u32, channel));
        }
    }
}

/// Paper white, in the canvas's own mode and depth: every colour channel at its
/// maximum, or at zero for CMYK where zero is no ink, and alpha at zero so a
/// mode that can say "nothing here" says it.
fn blank_row(canvas: &Canvas) -> Vec<u8> {
    let mut row = canvas.row_buffer();
    let white = match canvas.mode {
        ColorMode::Cmyk => 0,
        _ if canvas.depth.bits() == 16 => u16::MAX,
        _ => (1u16 << canvas.depth.bits()) - 1,
    };
    let alpha = canvas.mode.alpha_channel();
    for x in 0..canvas.width {
        for channel in 0..canvas.mode.samples() {
            let value = if Some(channel) == alpha { 0 } else { white };
            row.set_sample(x, 0, channel, value);
        }
    }
    row.data
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{BitDepth, decode, fixtures};

    /// Pages held in memory, so a test can say what each page is without
    /// writing files. The file-backed implementation is
    /// `src-tauri/src/exporting.rs`; both satisfy the same trait, and the
    /// integration test in `tests/` is the one that goes to disk.
    struct InMemory {
        pages: Vec<Raster>,
        decoded: usize,
    }

    impl InMemory {
        fn new(pages: Vec<Raster>) -> InMemory {
            InMemory { pages, decoded: 0 }
        }
    }

    impl PageSource for InMemory {
        fn header(&mut self, position: usize) -> Result<Header, ExportError> {
            let page = &self.pages[position];
            Ok(Header {
                width: page.width,
                height: page.height,
                mode: page.mode,
                depth: page.depth,
                icc: page.icc.clone(),
                color: page.color.clone(),
                srgb_intent: page.srgb_intent,
                trns: page.trns.clone(),
                palette: page.palette.clone(),
            })
        }

        fn page(&mut self, position: usize) -> Result<Raster, ExportError> {
            self.decoded += 1;
            Ok(self.pages[position].clone())
        }
    }

    fn strip_of(pages: &[Raster]) -> Strip {
        Strip::of_sizes(&pages.iter().map(|p| (p.width, p.height)).collect::<Vec<_>>())
    }

    fn three_pages(name: &str) -> Vec<Raster> {
        (0..3)
            .map(|n| {
                let mut page = fixtures::by_name(name).raster;
                // Distinguishable pages: a band of a per-page value along the
                // top, so a stitched file that repeats or reorders pages fails.
                for x in 0..page.width {
                    for channel in 0..page.mode.samples() {
                        page.set_sample(x, 0, channel, (n * 7 + 1) as u16);
                    }
                }
                page
            })
            .collect()
    }

    #[test]
    fn unedited_stitch_drops_page_local_precision_and_luminance_summaries() {
        let mut pages = three_pages("l8");
        pages[0].color.significant_bits = Some(vec![4]);
        pages[0].color.content_light = Some(vec![0; 8]);
        pages[1].color.significant_bits = Some(vec![8]);
        let strip = strip_of(&pages);
        let mut source = InMemory::new(pages);
        let mut out = std::io::Cursor::new(Vec::new());
        let result = export_stitched(&strip, &[], &mut source, Target::SameAsSource, &mut out).unwrap();
        for expected in ["sBIT", "cLLI"] {
            assert!(result.declared.iter().any(|value| matches!(value, Declaration::MetadataNormalized { field, .. } if field == expected)));
        }
        let encoded = out.into_inner();
        let mut at = 8;
        while at + 12 <= encoded.len() {
            assert!(!matches!(&encoded[at + 4..at + 8], b"sBIT" | b"cLLI"));
            at += 12 + u32::from_be_bytes(encoded[at..at + 4].try_into().unwrap()) as usize;
        }
    }

    /// Every source row lands at its strip offset, once, in order.
    #[test]
    fn the_stitched_file_is_the_pages_in_order_at_their_offsets() {
        let pages = three_pages("l8");
        let strip = strip_of(&pages);
        let mut source = InMemory::new(pages.clone());
        let mut out = std::io::Cursor::new(Vec::new());

        let stitched =
            export_stitched(&strip, &[], &mut source, Target::SameAsSource, &mut out).unwrap();
        assert_eq!(stitched.format, Format::Png);
        assert_eq!((stitched.width, stitched.height), (strip.width(), strip.height()));
        assert_eq!(stitched.gutter_pixels, 0, "three pages of equal width have no gutter");

        let back = decode(&out.into_inner()).unwrap();
        assert_eq!((back.width, back.height), (strip.width(), strip.height()));
        for (index, page) in pages.iter().enumerate() {
            let offset = strip.pages()[index].y_offset as u32;
            for y in 0..page.height {
                for x in 0..page.width {
                    assert_eq!(
                        back.sample(x, offset + y, 0),
                        page.sample(x, y, 0),
                        "page {index} at ({x}, {y})"
                    );
                }
            }
        }
    }

    /// Each page is asked for **once**, which is what says the writer streams
    /// rather than revisits. That it holds only one at a time is structural -
    /// [`Resident`] has one page in it and `export_stitched` has one
    /// `Resident` - and a test cannot see the difference between a held page
    /// and a dropped one; a second `page` call for a position already written
    /// is the observable form of getting it wrong, and this catches that.
    #[test]
    fn every_page_is_asked_for_exactly_once() {
        let pages = three_pages("rgb8");
        let strip = strip_of(&pages);
        let mut source = InMemory::new(pages);
        let mut out = std::io::Cursor::new(Vec::new());

        export_stitched(&strip, &[], &mut source, Target::SameAsSource, &mut out).unwrap();
        assert_eq!(source.decoded, 3, "a page was decoded more than once");
    }

    /// The gutter beside a narrow page, counted and written as paper white
    /// rather than as whatever happened to be in the buffer.
    #[test]
    fn a_narrow_page_gets_a_white_gutter_and_the_count_says_so() {
        let mut pages = three_pages("l8");
        let narrow = &mut pages[1];
        narrow.width = 40;
        narrow.data = (0..narrow.height)
            .flat_map(|y| (0..40).map(move |x| ((x + y) % 251) as u8))
            .collect();

        let strip = strip_of(&pages);
        assert_eq!(strip.pages()[1].x_offset, 12, "64 wide, 40 wide, centred");

        let mut source = InMemory::new(pages.clone());
        let mut out = std::io::Cursor::new(Vec::new());
        let stitched =
            export_stitched(&strip, &[], &mut source, Target::SameAsSource, &mut out).unwrap();
        assert_eq!(stitched.gutter_pixels, (64 - 40) * u64::from(pages[1].height));

        let back = decode(&out.into_inner()).unwrap();
        let top = strip.pages()[1].y_offset as u32;
        assert_eq!(back.sample(0, top, 0), 255, "the gutter is not paper white");
        assert_eq!(back.sample(63, top, 0), 255);
        assert_eq!(back.sample(12, top, 0), pages[1].sample(0, 0, 0), "the page moved");
    }

    /// A CMYK strip goes to TIFF, which is the one format that carries it,
    /// and its gutter is zero ink rather than four channels of
    /// maximum, which would be the blackest black the file can hold.
    #[test]
    fn a_cmyk_strip_is_written_as_tiff_with_an_unlinked_gutter() {
        let mut pages = three_pages("cmyk8");
        pages[1].width = 40;
        pages[1].data = vec![9; 40 * pages[1].height as usize * 4];
        let strip = strip_of(&pages);

        let mut source = InMemory::new(pages);
        let mut out = std::io::Cursor::new(Vec::new());
        let stitched =
            export_stitched(&strip, &[], &mut source, Target::SameAsSource, &mut out).unwrap();
        assert_eq!(stitched.format, Format::Tiff);

        let back = decode(&out.into_inner()).unwrap();
        assert_eq!(back.mode, ColorMode::Cmyk);
        let top = strip.pages()[1].y_offset as u32;
        for channel in 0..4 {
            assert_eq!(back.sample(0, top, channel), 0, "the gutter carries ink");
        }
    }

    /// Two ordinary grayscale pages with `odd` between them, and the refusal
    /// that comes back. Asserts here what every case shares: nothing was
    /// written and nothing was decoded.
    fn refusal_with_page_1_replaced_by(odd: Raster) -> StitchRefusal {
        let pages = vec![fixtures::by_name("l8").raster, odd, fixtures::by_name("l8").raster];
        let strip = strip_of(&pages);
        let mut source = InMemory::new(pages);
        let mut out = std::io::Cursor::new(Vec::new());
        let error = export_stitched(&strip, &[], &mut source, Target::SameAsSource, &mut out)
            .expect_err("a mixed strip was stitched");
        assert!(out.into_inner().is_empty(), "a refused stitch wrote bytes");
        assert_eq!(source.decoded, 0, "a refused stitch decoded a page");
        match error {
            ExportError::NotStitchable(refusal) => refusal,
            other => panic!("wrong error: {other}"),
        }
    }

    /// One file has one mode, one depth and one profile. Each disagreement is
    /// refused from the headers, before anything is written - a truncated file
    /// is indistinguishable from a crash.
    #[test]
    fn a_strip_whose_pages_disagree_is_refused_before_a_byte_is_written() {
        assert!(matches!(
            refusal_with_page_1_replaced_by(fixtures::by_name("rgb8").raster),
            StitchRefusal::MixedMode { position: 1, .. }
        ));
        assert!(matches!(
            refusal_with_page_1_replaced_by(fixtures::by_name("l16").raster),
            StitchRefusal::MixedDepth { position: 1, .. }
        ));
        let tagged = Raster { icc: Some(fixtures::gray_profile()), ..fixtures::by_name("l8").raster };
        assert!(matches!(
            refusal_with_page_1_replaced_by(tagged),
            StitchRefusal::MixedProfile { position: 1 }
        ));
    }

    /// Every variant answers a key, and no two answer the same one. A shared
    /// key is how five distinct reasons become one unhelpful sentence - a
    /// known failure this pins.
    #[test]
    fn every_refusal_has_a_key_of_its_own() {
        let keys = [
            StitchRefusal::NoPages.reason_key(),
            StitchRefusal::MixedMode {
                position: 1,
                first: ColorMode::Gray,
                found: ColorMode::Rgb,
            }
            .reason_key(),
            StitchRefusal::MixedDepth { position: 1, first: 8, found: 16 }.reason_key(),
            StitchRefusal::MixedProfile { position: 1 }.reason_key(),
            StitchRefusal::Indexed.reason_key(),
        ];
        let mut unique = keys.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), keys.len(), "two refusals share a sentence");
        for key in keys {
            assert!(key.starts_with("notice.export.stitchRefused."), "{key}");
        }
    }

    #[test]
    fn an_indexed_strip_and_an_empty_one_are_refused_by_name() {
        let pages = vec![fixtures::by_name("indexed-p").raster];
        let strip = strip_of(&pages);
        let mut source = InMemory::new(pages);
        let mut out = std::io::Cursor::new(Vec::new());
        assert!(matches!(
            export_stitched(&strip, &[], &mut source, Target::SameAsSource, &mut out),
            Err(ExportError::NotStitchable(StitchRefusal::Indexed))
        ));

        let mut empty = InMemory::new(Vec::new());
        assert!(matches!(
            export_stitched(
                &Strip::of_sizes(&[]),
                &[],
                &mut empty,
                Target::SameAsSource,
                &mut std::io::Cursor::new(Vec::new())
            ),
            Err(ExportError::NotStitchable(StitchRefusal::NoPages))
        ));
    }

    /// A bitonal strip: the packed-row case, and the one where a page's x
    /// offset can start mid-byte.
    #[test]
    fn a_bitonal_strip_keeps_its_packing() {
        let pages = vec![fixtures::by_name("bitonal").raster, fixtures::by_name("bitonal").raster];
        let strip = strip_of(&pages);
        let mut source = InMemory::new(pages.clone());
        let mut out = std::io::Cursor::new(Vec::new());

        export_stitched(&strip, &[], &mut source, Target::SameAsSource, &mut out).unwrap();
        let back = decode(&out.into_inner()).unwrap();
        assert_eq!(back.depth, BitDepth::One);
        for y in 0..pages[0].height {
            for x in 0..pages[0].width {
                assert_eq!(back.sample(x, y, 0), pages[0].sample(x, y, 0), "({x}, {y})");
                assert_eq!(
                    back.sample(x, pages[0].height + y, 0),
                    pages[1].sample(x, y, 0),
                    "({x}, {y}) on the second page"
                );
            }
        }
    }
}
