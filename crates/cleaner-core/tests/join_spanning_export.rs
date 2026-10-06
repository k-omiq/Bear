//! Rule 8, against files on disk.
//!
//! Rule 8 and the color-fidelity export rule are one decision seen from two
//! sides, and both of them are about what ends up in a *file*:
//!
//! > A patch spanning a join is split at the page boundary, and the passthrough
//! > test uses per-page **intersection** with the global patch set, not patch
//! > ownership.
//!
//! Fidelity assertions are made here the way they're required -
//! against the written file, decoded back, never against the buffer that was
//! about to be written. A split that rounds differently on either side of a
//! join, or a page declared untouched while pixels are written into it, is
//! visible in the files and in nothing else.
//!
//! The unit tests in `export::split` cover the arithmetic. This covers the
//! claim, which is about four files and one strip.

use std::path::PathBuf;

use cleaner_core::composite::{changed_pixels, permitted_region};
use cleaner_core::export::{
    ExportError, PageSource, StripPatch, Target, export_page_to, export_stitched, patches_on_page,
};
use cleaner_core::image::{
    BitDepth, ColorMode, Format, Header, Raster, decode, encode, fixtures, png_header,
};
use cleaner_core::mask::{Mask, Rect};
use cleaner_core::patch::{Engine, Patch, Provenance};
use cleaner_core::strip::Strip;

/// A directory that removes itself. The tests here are about files, so they
/// write files.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir()
            .join("cleaner-join-export")
            .join(format!("{name}-{}", NEXT.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch directory");
        Scratch(dir)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const FILL_ACROSS_JOIN_0: u16 = 200;
const FILL_ACROSS_JOIN_1: u16 = 77;

/// Four pages, all grayscale, all carrying the same profile, and the third one
/// narrower than the strip so a page's own x offset is in play.
fn pages() -> Vec<Raster> {
    let profile = fixtures::gray_profile();
    (0..4)
        .map(|n| {
            let mut page = fixtures::by_name("l8").raster;
            page.icc = Some(profile.clone());
            if n == 2 {
                // 40 of 64: centred, so this page starts at x = 12.
                let stride = page.stride();
                page.data = (0..page.height as usize)
                    .flat_map(|y| page.data[y * stride..y * stride + 40].to_vec())
                    .collect();
                page.width = 40;
            }
            // A per-page band, so a file holding the wrong page is visible.
            for x in 0..page.width {
                page.set_sample(x, 0, 0, (n * 5 + 3) as u16);
            }
            page
        })
        .collect()
}

fn write_sources(scratch: &Scratch, pages: &[Raster]) -> Vec<PathBuf> {
    pages
        .iter()
        .enumerate()
        .map(|(n, page)| {
            let path = scratch.join(&format!("{n:03}.png"));
            std::fs::write(&path, encode(page, Format::Png).unwrap()).unwrap();
            path
        })
        .collect()
}

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

/// A rung-0 planar fill in **strip** coordinates: one tone over the whole
/// rectangle. Rule 1 asks the two halves of a split patch to "share a fill tone
/// with no visible seam", and a constant tone is the case where a seam would be
/// unmistakable - one level of difference between the row above the join and
/// the row below it is a line across the bubble.
fn fill(id: &str, bounds: Rect, tone: u16) -> StripPatch {
    let mut pixels = Raster {
        width: bounds.w,
        height: bounds.h,
        mode: ColorMode::Gray,
        depth: BitDepth::Eight,
        icc: None,
        palette: None,
        trns: None,
        srgb_intent: None,
        color: Default::default(),
        data: vec![tone as u8; (bounds.w * bounds.h) as usize],
    };
    // Belt and braces: written through `set_sample` so the buffer is right even
    // if the depth of this fixture ever changes.
    for y in 0..bounds.h {
        for x in 0..bounds.w {
            pixels.set_sample(x, y, 0, tone);
        }
    }
    StripPatch::of(Patch {
        id: id.into(),
        mask: Mask::filled(bounds),
        ink: Mask::filled(bounds),
        pixels,
        order: 0,
        visible: true,
        provenance: provenance(),
    })
}

/// The global patch set: one patch across join 0, one across join 1 and onto
/// the narrow page. Page 3 is touched by neither.
fn global() -> Vec<StripPatch> {
    vec![
        // Strip rows 40..56, across the join at 48.
        fill("across-join-0", Rect::new(10, 40, 20, 16), FILL_ACROSS_JOIN_0),
        // Strip rows 90..100, across the join at 96, and strip columns 14..26,
        // which on the narrow page are columns 2..14.
        fill("across-join-1", Rect::new(14, 90, 12, 10), FILL_ACROSS_JOIN_1),
    ]
}

fn strip_of(pages: &[Raster]) -> Strip {
    Strip::of_sizes(&pages.iter().map(|p| (p.width, p.height)).collect::<Vec<_>>())
}

/// Export every page to its own file, and hand back what was written.
fn export_pages(scratch: &Scratch, sources: &[PathBuf], strip: &Strip, global: &[StripPatch]) -> Vec<PathBuf> {
    (0..strip.pages().len())
        .map(|position| {
            let bytes = std::fs::read(&sources[position]).unwrap();
            let patches = patches_on_page(strip, position, global);
            let out = scratch.join(&format!("out-{position:03}.png"));
            let mut file = std::fs::File::create(&out).unwrap();
            export_page_to(&bytes, &patches, Target::SameAsSource, &mut file).unwrap();
            out
        })
        .collect()
}

/// The fidelity contract, on every written file: mode, depth and profile
/// survive, and the only pixels that changed are inside
/// `dilate(union(masks), edit_margin)`.
///
/// The masks it is stated over are the **page's own parts** of the global set,
/// which is the whole subject of rule 8 - for page 1 there is no patch that
/// belongs to it, and there is a part of one that writes into it.
#[test]
fn every_page_file_keeps_the_contract_under_a_join_spanning_patch_set() {
    let scratch = Scratch::new("fidelity");
    let pages = pages();
    let sources = write_sources(&scratch, &pages);
    let strip = strip_of(&pages);
    let global = global();
    let written = export_pages(&scratch, &sources, &strip, &global);

    for (position, page) in pages.iter().enumerate() {
        let source = decode(&std::fs::read(&sources[position]).unwrap()).unwrap();
        let out = decode(&std::fs::read(&written[position]).unwrap()).unwrap();

        assert_eq!(out.mode, source.mode, "page {position}: mode");
        assert_eq!(out.depth, source.depth, "page {position}: bit depth");
        assert_eq!(out.icc, source.icc, "page {position}: icc profile");
        assert_eq!(out.palette, source.palette, "page {position}: palette");
        assert_eq!((out.width, out.height), (page.width, page.height), "page {position}: size");

        let patches = patches_on_page(&strip, position, &global);
        let permitted = permitted_region(&patches, page.width, page.height);
        for (x, y) in changed_pixels(&source, &out) {
            assert!(
                permitted.contains(x as i64, y as i64),
                "page {position}: ({x}, {y}) changed outside the permitted region"
            );
        }
    }
}

/// **The rule ownership gets wrong.**
///
/// Page 1 owns no patch: both patches are anchored above it, and every patch
/// record in a manifest names one page. Ask "which patches belong to page 1"
/// and the answer is none, so page 1 is a passthrough - its file is copied
/// byte-for-byte and the bottom half of the bubble is missing from it, while
/// page 0's file carries the top half.
///
/// The failure is invisible to the fidelity contract, which is an upper bound
/// on what may change: a passthrough changes nothing, so it passes. This is the
/// test that sees it.
#[test]
fn a_page_no_patch_belongs_to_is_still_written_and_a_page_nothing_reaches_is_not() {
    let scratch = Scratch::new("intersection");
    let pages = pages();
    let sources = write_sources(&scratch, &pages);
    let strip = strip_of(&pages);
    let global = global();
    let written = export_pages(&scratch, &sources, &strip, &global);

    let file = |n: usize| std::fs::read(&written[n]).unwrap();
    let source = |n: usize| std::fs::read(&sources[n]).unwrap();

    assert_ne!(file(0), source(0), "page 0 lost the half of the patch it anchors");
    assert_ne!(file(1), source(1), "page 1 was declared untouched and is not");
    assert_ne!(file(2), source(2), "the narrow page lost its part");

    // And the other half of §5, which is the half that already worked: a page
    // the global set genuinely does not reach is copied, not re-encoded.
    assert_eq!(file(3), source(3), "an untouched page was re-encoded");
    assert!(patches_on_page(&strip, 3, &global).is_empty());
}

/// The seam itself, read back from two files.
///
/// A planar fill is one tone. If the split recomputed anything per page -
/// re-fitted the mask, re-measured a ring, re-ran the engine on the part - the
/// two files would carry two tones that differ by a level or two, which is a
/// line across the bubble at exactly the join. Every sample on both sides is
/// the tone the patch was made with, because the split is a copy.
#[test]
fn the_two_files_either_side_of_a_join_carry_the_same_fill_tone() {
    let scratch = Scratch::new("seam");
    let pages = pages();
    let sources = write_sources(&scratch, &pages);
    let strip = strip_of(&pages);
    let global = global();
    let written = export_pages(&scratch, &sources, &strip, &global);

    let above = decode(&std::fs::read(&written[0]).unwrap()).unwrap();
    let below = decode(&std::fs::read(&written[1]).unwrap()).unwrap();

    for x in 10..30 {
        // Strip row 47 is page 0's last row; strip row 48 is page 1's first.
        assert_eq!(above.sample(x, 47, 0), FILL_ACROSS_JOIN_0, "page 0 column {x}");
        assert_eq!(below.sample(x, 0, 0), FILL_ACROSS_JOIN_0, "page 1 column {x}");
    }
    // And nothing outside the patch's columns was touched at the seam.
    assert_eq!(above.sample(9, 47, 0), pages[0].sample(9, 47, 0));
    assert_eq!(below.sample(30, 0, 0), pages[1].sample(30, 0, 0));

    // The second join, where the page below is narrower and inset by 12: strip
    // columns 14..26 are the narrow page's columns 2..14.
    let narrow = decode(&std::fs::read(&written[2]).unwrap()).unwrap();
    let middle = decode(&std::fs::read(&written[1]).unwrap()).unwrap();
    for column in 0..12u32 {
        assert_eq!(middle.sample(14 + column, 47, 0), FILL_ACROSS_JOIN_1, "page 1 column {column}");
        assert_eq!(narrow.sample(2 + column, 0, 0), FILL_ACROSS_JOIN_1, "page 2 column {column}");
    }
    assert_eq!(narrow.sample(1, 0, 0), pages[2].sample(1, 0, 0), "the inset was applied twice");
}

/// Pages read from files, one at a time. The core has no file policy, so the
/// trait is where a caller's does - this is the same shape
/// `src-tauri/src/exporting.rs` uses.
struct Files(Vec<PathBuf>);

impl PageSource for Files {
    fn header(&mut self, position: usize) -> Result<Header, ExportError> {
        let bytes = std::fs::read(&self.0[position]).map_err(|e| ExportError::Sink(e.to_string()))?;
        Ok(png_header(&bytes)?)
    }

    fn page(&mut self, position: usize) -> Result<Raster, ExportError> {
        let bytes = std::fs::read(&self.0[position]).map_err(|e| ExportError::Sink(e.to_string()))?;
        Ok(decode(&bytes)?)
    }
}

/// The stitched file and the per-page files are the same pixels.
///
/// Rule 8 offers both, and a user who exports the same chapter twice should get
/// the same cleaning either way. Asserting one against the other is stronger
/// than asserting either against a fixture: it catches a split that lands
/// correctly per page and at a different offset in the strip, which no
/// per-page assertion can see.
#[test]
fn the_stitched_file_agrees_with_the_per_page_files_everywhere_but_the_gutter() {
    let scratch = Scratch::new("stitched");
    let pages = pages();
    let sources = write_sources(&scratch, &pages);
    let strip = strip_of(&pages);
    let global = global();
    let written = export_pages(&scratch, &sources, &strip, &global);

    let out = scratch.join("stitched.png");
    let mut file = std::fs::File::create(&out).unwrap();
    let stitched =
        export_stitched(&strip, &global, &mut Files(sources.clone()), Target::SameAsSource, &mut file)
            .unwrap();
    drop(file);

    assert_eq!(stitched.format, Format::Png);
    assert_eq!(stitched.gutter_pixels, (64 - 40) * 48, "one narrow page's worth of gutter");

    let whole = decode(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!((whole.width, whole.height), (strip.width(), strip.height()));
    assert_eq!(whole.icc, pages[0].icc, "the strip lost the profile its pages carried");

    for (position, placement) in strip.pages().iter().enumerate() {
        let page = decode(&std::fs::read(&written[position]).unwrap()).unwrap();
        for y in 0..page.height {
            for x in 0..page.width {
                assert_eq!(
                    whole.sample(x + placement.x_offset as u32, y + placement.y_offset as u32, 0),
                    page.sample(x, y, 0),
                    "page {position} at ({x}, {y})"
                );
            }
        }
    }

    // The gutter beside the narrow page is paper white, and it is the only
    // part of the stitched file no source page has a pixel for.
    let top = strip.pages()[2].y_offset as u32;
    assert_eq!(whole.sample(0, top, 0), 255);
    assert_eq!(whole.sample(63, top + 47, 0), 255);
}
