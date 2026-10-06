//! The `tile://` custom protocol: pixels, straight to the browser.
//!
//! The frontend holds patch
//! *metadata* - id, bbox, engine, visibility, order - and URLs; the bytes of a
//! page never cross the IPC boundary, because a base64 data URL inflates the
//! payload by a third and decodes on the JS main thread.
//!
//! ## Whole responses, never a range
//!
//! A maintainer benchmark recorded **~5 ms on macOS against ~200 ms on
//! Windows for 10 MB**, about 50 MB/s - because WebView2 raises
//! `WebResourceRequested` on the host UI thread and pauses page load while the
//! handler runs. **Range and streaming responses are unsupported on WebView2.**
//! That is not our measurement and it is not ours to restate as one; it is why
//! this module builds one complete `Vec<u8>` per request and never emits
//! `Accept-Ranges`, `Content-Range` or a `206`. Tauri's own asset protocol does
//! serve ranges; this one deliberately does not.
//!
//! The throughput number itself was measured in
//! Phase 2, on a Windows machine. Nothing here has been executed on one.
//!
//! ## The URL
//!
//! ```text
//! tile://localhost/<chapterId>/<pageIndex>/<variant>[/<tile>]?v=<version>[&a=<appearance>]
//! http://tile.localhost/<chapterId>/<pageIndex>/<variant>[/<tile>]?v=<version>[&a=<appearance>]
//! ```
//!
//! The second form is Windows and Android; the first is macOS, iOS and Linux.
//! The path is identical on both, which is why the frontend chooses only the
//! origin ([`src/lib/api/tile.js`](../../src/lib/api/tile.js)) and never the
//! shape. `variant` is `source` or `cleaned`.
//!
//! ## The fourth segment is a proxy tile, and it is what the interface asks for
//!
//! Rule 7: *the UI holds proxies,
//! never the strip*. With `<tile>` the response is one tile of the page's
//! **proxy** - short edge capped at 1024, long axis cut into
//! `cleaner_core::image::proxy::PROXY_TILE_LONG_EDGE` bands - and that is the
//! form both components draw. Without it the response is the page at its own
//! resolution, which rule 7 reserves for the viewport past 100% zoom and which
//! nothing requests today.
//!
//! The whole page is decoded to produce any one tile, so a page's tiles are
//! cut together and kept, encoded, in a bounded cache keyed by the same
//! identity the URL's version names ([`TileCache`]). The *page* is the unit
//! here - never the strip, which is the distinction rule 1 draws.
//!
//! **`v` is read by nobody here, on purpose.** It is a cache-busting token the
//! interface derives from the page's appearance digest, [`page_appearance`],
//! which folds the source `sha256` with every visible patch that reaches the
//! page: its content, order, opacity and placement - so that a URL changes
//! exactly when the bytes behind it change. That is the rule it follows: a
//! hash decides staleness, an mtime only explains it.
//!
//! **`a` is read, because the interface's copy can be older than the
//! manifest.** It is the native digest the URL was built from. A `cleaned`
//! response is cached immutably only when `a` is the digest of the saved state
//! it was drawn from; any other answer is `no-store` and names the state it
//! was drawn from ([`serve_request`]). The digest is the one opinion, and this
//! side holds it.
//!
//! ## What `cleaned` means
//!
//! `raw + ordered visible patches`, through `cleaner_core::composite` - the same
//! function the exporter uses, so what the user is looking at is what the file
//! will hold. A page with no patches yields the source's own bytes: that is the
//! correct answer for an uncleaned page, not a placeholder.
//!
//! ## One detection's mask
//!
//! ```text
//! /<chapterId>/<pageIndex>/detection/<regionId>?v=<maskSha256>.<sequence>
//! ```
//!
//! Here the fourth segment is a percent-encoded region id, not a tile. The
//! answer is the stored detection's display set, its mask and its lettering
//! together, cropped to the pixels it holds: opaque white inside, transparent
//! outside, at page resolution. `x-mask-bounds` says where it sits on the
//! page, as `x,y,w,h` in page pixels. `v` is the mask's digest and its
//! sequence, one more than its hand edits, so it moves with every edit. It is
//! never cached here: the interface keeps its own copy, by URL.
//!
//! ## One patch's layer
//!
//! ```text
//! /<chapterId>/<pageIndex>/layer/<regionId>?v=<layerKey>
//! ```
//!
//! What the editor draws. The page is its `source` tiles, which a clean never
//! changes, and over them one image per visible patch, in compositing order.
//! The answer is the part of the patch that lands on the page the path names -
//! a longstrip patch across a join has a part on each page - as a
//! see-through PNG on that page's proxy grid, with `x-layer-bounds: x,y,w,h`
//! in proxy pixels. Drawn over the source tile at the layer's opacity it is
//! the `cleaned` tile, so a change to one patch changes one image and the rest
//! of the page stays on screen. `v` is [`layer_appearance`], which leaves
//! out opacity: the interface draws that. A patch with nothing on the page is
//! a `204`. Never cached here; the interface keeps its own copy, by URL.
//!
//! The `cleaned` variant stays for everything that wants the page flat: the
//! home screen's cover, and the denoise input.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard};

use cleaner_core::composite::composite;
use cleaner_core::image::proxy::{ProxyPlan, TileRect, managed_proxy_layer, managed_proxy_tile};
use cleaner_core::image::{ColorMode, Format, Raster, encode, encode_png_preview};
use cleaner_core::mask::Rect;
use cleaner_core::patch::Patch;
use cleaner_core::project::{Job, PatchRecord, Project, StripMode};
use cleaner_core::strip::Strip;
#[cfg(test)]
use cleaner_core::image::{decode, proxy::proxy_tile};
use tauri::http::{Request, Response, StatusCode, header};

use crate::library;

/// The scheme, registered on the builder and named by the frontend.
pub const SCHEME: &str = "tile";

/// Which of a page's two images is wanted.
///
/// Two images rather than one image with a toggle: `src/lib/editor/PageSheet.svelte`
/// clips one over the other to make the wipe, so both must be addressable at
/// once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Variant {
    /// The page as it arrived. Every region's text is on it.
    Source,
    /// The page as the pipeline left it.
    Cleaned,
}

impl Variant {
    fn parse(segment: &str) -> Option<Variant> {
        match segment {
            "source" => Some(Variant::Source),
            "cleaned" => Some(Variant::Cleaned),
            _ => None,
        }
    }
}

/// One parsed request. Deliberately owns its `chapter_id`: the borrow would be
/// of the request's URI, and the resolver outlives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileRequest {
    pub chapter_id: String,
    pub page_index: usize,
    pub variant: Variant,
    /// Which tile of the page's proxy, or `None` for the page at its own
    /// resolution. See the module note on the fourth segment.
    pub tile: Option<usize>,
}

#[derive(Debug, thiserror::Error)]
pub enum TileError {
    #[error("a tile path is /<chapterId>/<pageIndex>/<variant>[/<tile>], not {0:?}")]
    Malformed(String),
    #[error("no chapter {0}")]
    NoSuchChapter(String),
    #[error("{job} has no page {page_index}")]
    NoSuchPage { job: String, page_index: usize },
    #[error("page {page_index}'s proxy has {tiles} tiles, not a tile {tile}")]
    NoSuchTile { page_index: usize, tile: usize, tiles: usize },
    #[error("page {page_index} has no detection {region_id}")]
    NoSuchDetection { page_index: usize, region_id: String },
    #[error("detection {0} has no pixels to draw")]
    EmptyDetection(String),
    #[error("no visible patch {0}")]
    NoSuchLayer(String),
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error(transparent)]
    Store(#[from] cleaner_core::project::StoreError),
    #[error(transparent)]
    Image(#[from] cleaner_core::image::ImageError),
    #[error(transparent)]
    Composite(#[from] cleaner_core::composite::CompositeError),
}

impl TileError {
    /// What the webview is told.
    ///
    /// A malformed path is the caller's error and a missing chapter or page is
    /// a `404` whatever the reason for it - the *why* goes in the body, where
    /// the devtools console shows it, rather than being encoded in a status
    /// nothing branches on.
    pub fn status(&self) -> StatusCode {
        match self {
            TileError::Malformed(_) => StatusCode::BAD_REQUEST,
            TileError::NoSuchChapter(_)
            | TileError::NoSuchPage { .. }
            | TileError::NoSuchTile { .. }
            | TileError::NoSuchDetection { .. }
            | TileError::EmptyDetection(_)
            | TileError::NoSuchLayer(_) => StatusCode::NOT_FOUND,
            // A file that is not there is a `404` wherever the absence was
            // noticed - the manifest, or the source it names. Anything else
            // that fails to read is a fault on this side.
            TileError::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound => {
                StatusCode::NOT_FOUND
            }
            TileError::Store(cleaner_core::project::StoreError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                StatusCode::NOT_FOUND
            }
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// `/<chapterId>/<pageIndex>/<variant>[/<tile>]` - the query is read apart,
/// by [`expected_appearance`]; see the module note on `v` and `a`.
///
/// Takes the path alone rather than the whole URI because that is the one part
/// of the URL that is the same on every platform: the origin differs
/// (`tile://localhost` against `http://tile.localhost`) and the path does not.
pub fn parse(path: &str) -> Result<TileRequest, TileError> {
    let malformed = || TileError::Malformed(path.to_owned());
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let (chapter, index, variant, tile) = match segments.as_slice() {
        [chapter, index, variant] => (chapter, index, variant, None),
        [chapter, index, variant, tile] => {
            (chapter, index, variant, Some(tile.parse().map_err(|_| malformed())?))
        }
        _ => return Err(malformed()),
    };

    let chapter_id = percent_decode(chapter);
    if chapter_id.is_empty() {
        return Err(malformed());
    }
    Ok(TileRequest {
        chapter_id,
        page_index: index.parse().map_err(|_| malformed())?,
        variant: Variant::parse(variant).ok_or_else(malformed)?,
        tile,
    })
}

/// One region's image on one page, asked for by region id: a stored
/// detection's display set, or a patch's layer. See the module notes on the
/// `detection` and `layer` forms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectionRequest {
    pub chapter_id: String,
    pub page_index: usize,
    pub region_id: String,
}

/// What a `tile://` path asks for: a page's pixels, one detection's mask, or
/// one patch's layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Page(TileRequest),
    Detection(DetectionRequest),
    Layer(DetectionRequest),
}

/// Any `tile://` path. `detection` or `layer` as the third segment makes the
/// fourth a region id; every other path is [`parse`]'s, so a number after
/// `source` or `cleaned` is still a tile.
pub fn route(path: &str) -> Result<Route, TileError> {
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let (chapter, index, kind, region) = match segments.as_slice() {
        [chapter, index, kind @ ("detection" | "layer"), region] => (chapter, index, *kind, region),
        _ => return parse(path).map(Route::Page),
    };
    let malformed = || TileError::Malformed(path.to_owned());
    let (chapter_id, region_id) = (percent_decode(chapter), percent_decode(region));
    if chapter_id.is_empty() || region_id.is_empty() {
        return Err(malformed());
    }
    let request = DetectionRequest { chapter_id, page_index: index.parse().map_err(|_| malformed())?, region_id };
    Ok(if kind == "layer" { Route::Layer(request) } else { Route::Detection(request) })
}

/// A chapter id is a path segment, and a path segment arrives percent-encoded.
///
/// Written out rather than taken as a dependency: the whole of what is needed
/// is `%XX`, and an invalid escape is left alone rather than replaced, so a
/// literal `%` in an id round-trips instead of becoming a replacement
/// character that would not match anything.
fn percent_decode(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// [`render_job`] from a manifest path, for tests that want the bytes alone.
#[cfg(test)]
pub fn render(
    job_path: &Path,
    page_index: usize,
    variant: Variant,
    tile: Option<usize>,
) -> Result<Vec<u8>, TileError> {
    render_job(&Job::open(job_path)?, job_path, page_index, variant, tile)
}

/// The bytes for one page, as PNG.
///
/// Split from the protocol glue so it can be tested against a real `.mtclean`
/// on disk without a webview, which is where everything that can actually be
/// wrong lives.
///
/// `page_index` is the URL's, which is `ApiPage.index`, which is a **position
/// in `strip.order`** - not an index into `sources`. The two are equal only
/// until a strip is reordered or a source is dropped, so the source index is
/// resolved through [`library::Library::resolve_page`], the one place that rule
/// is written. A position `strip.order` does not have is a `404`, the same
/// answer as a page that is not there, because that is what it is.
///
/// `tile` is the URL's fourth segment: `Some(i)` for tile `i` of the page's
/// proxy, `None` for the page at its own resolution.
///
/// It takes a manifest already read, so [`serve_request`] can check the
/// appearance of the very state the bytes were drawn from.
fn render_job(
    job: &Job,
    job_path: &Path,
    page_index: usize,
    variant: Variant,
    tile: Option<usize>,
) -> Result<Vec<u8>, TileError> {
    let Some(index) = tile else {
        let (source_bytes, patches) = page_inputs(job, job_path, page_index, variant)?;
        let source_idx = library::Library::resolve_page(&job.project, page_index).ok_or_else(|| TileError::NoSuchPage { job: job_path.display().to_string(), page_index })?;
        let page = job.display_page(source_idx, &source_bytes)?;
        let composited = if patches.is_empty() { page } else { composite(&page, &patches)? };
        return Ok(encode_for_display(&composited, false)?);
    };
    let mut tiles = render_tiles(job, job_path, page_index, variant)?;
    let count = tiles.len();
    if index >= count {
        return Err(TileError::NoSuchTile { page_index, tile: index, tiles: count });
    }
    Ok(tiles.swap_remove(index))
}

/// Every proxy tile of one page, from **one** decode.
///
/// A page is decoded and composited whole whichever tile is asked for, so
/// asking tile by tile decoded a 20 000 px segment once per tile - ten times
/// for its source and ten more for its cleaned image. Cut together, the page
/// costs one decode per variant and the rest of its tiles are already waiting
/// in [`TILES`] when the webview asks for them. They are encoded at the PNG
/// encoder's fast setting: lossless all the same, drawn once and never kept.
///
/// The passthrough survives where the proxy *is* the page: one tile of a page
/// small enough not to be reduced and not to be cut. The header is read for
/// the dimensions rather than the whole file being decoded to find them.
fn render_tiles(
    job: &Job,
    job_path: &Path,
    page_index: usize,
    variant: Variant,
) -> Result<Vec<Vec<u8>>, TileError> {
    let (source_bytes, patches) = page_inputs(job, job_path, page_index, variant)?;
    let source_idx = library::Library::resolve_page(&job.project, page_index).ok_or_else(|| TileError::NoSuchPage { job: job_path.display().to_string(), page_index })?;
    let page = job.display_page(source_idx, &source_bytes)?;
    let composited = if patches.is_empty() { page } else { composite(&page, &patches)? };
    let count = ProxyPlan::of(&composited).tiles;
    (0..count)
        .map(|index| {
            let drawn = managed_proxy_tile(&composited, index)?.ok_or(TileError::NoSuchTile {
                page_index,
                tile: index,
                tiles: count,
            })?;
            Ok(encode_png_preview(&drawn)?)
        })
        .collect()
}

/// Native-depth, oriented composite for processing; display PNGs must never
/// become the input to a destructive operation such as whole-page denoise.
pub(crate) fn native_composite(job_path: &Path, page_index: usize, expected: Option<&str>) -> Result<Option<Raster>, TileError> {
    let job = read_job(job_path)?;
    if expected.is_some_and(|value| value != page_appearance(&job.project,page_index)) { return Ok(None); }
    let (bytes,patches) = page_inputs(&job,job_path,page_index,Variant::Cleaned)?;
    let source_idx = library::Library::resolve_page(&job.project,page_index).ok_or_else(|| TileError::NoSuchPage {job:job_path.display().to_string(),page_index})?;
    let page = job.display_page(source_idx,&bytes)?;
    Ok(Some(if patches.is_empty() {page} else {composite(&page,&patches)?}))
}

/// The source file's bytes and the patches drawn over them, for one page.
///
/// `page_index` is a position in `strip.order`, resolved to its source through
/// [`library::Library::resolve_page`] (see [`render_job`]).
fn page_inputs(
    job: &Job,
    job_path: &Path,
    page_index: usize,
    variant: Variant,
) -> Result<(Vec<u8>, Vec<Patch>), TileError> {
    let no_such_page =
        || TileError::NoSuchPage { job: job_path.display().to_string(), page_index };

    let source_idx =
        library::Library::resolve_page(&job.project, page_index).ok_or_else(no_such_page)?;
    let source_path = job.source_path(source_idx).ok_or_else(no_such_page)?;
    let source_bytes = match std::fs::read(&source_path) {
        Ok(bytes) => bytes,
        Err(source) => return Err(TileError::Io { path: source_path, source }),
    };

    let patches = match variant {
        Variant::Source => Vec::new(),
        Variant::Cleaned => visible_patches(job, page_index, source_idx)?,
    };
    Ok((source_bytes, patches))
}

/// Every visible patch recorded against one page, rebuilt from the manifest.
///
/// In longstrip mode, a patch anchored on one page may span across a join onto
/// adjacent pages, so patches are resolved via intersection (`patches_on_page`).
/// For paginated chapters (`StripMode::Single`), patches are filtered strictly
/// by source index ownership.
pub(crate) fn visible_patches(
    job: &Job,
    page_index: usize,
    source_idx: usize,
) -> Result<Vec<Patch>, TileError> {
    let _ = source_idx;
    Ok(job.display_patches_on_page(page_index)?.into_iter().filter(|patch| patch.visible).collect())
}

/// The strip in the positions `render_job` and `page_appearance` both address
/// pages by: a position in `strip.order`, never a source index.
fn strip_by_position(project: &Project) -> Strip {
    let sizes: Vec<(u32, u32)> = project
        .strip
        .order
        .iter()
        .filter_map(|i| project.sources.get(*i).map(|s| s.orientation.size(s.w, s.h)))
        .collect();
    Strip::of_sizes(&sizes)
}

/// The strip positions a box drawn from page `anchor` overlaps, top to
/// bottom. Pages are stacked, so the first candidate is found by bisection and
/// the walk stops at the first page that starts below the box: a patch costs
/// the pages it touches, not the chapter.
fn strip_reach(strip: &Strip, anchor: usize, displayed: Rect) -> impl Iterator<Item = usize> + '_ {
    let placed = strip
        .to_strip(anchor, displayed.x, displayed.y)
        .map(|(x, y)| Rect::new(x, y, displayed.w, displayed.h));
    let pages = strip.pages();
    let first = placed.map_or(pages.len(), |bbox| {
        pages.partition_point(|page| page.y_offset + i64::from(page.height) <= bbox.y)
    });
    pages[first..]
        .iter()
        .enumerate()
        .take_while(move |(_, page)| placed.is_some_and(|bbox| page.y_offset < bbox.bottom()))
        .filter(move |(_, page)| {
            let page = page.rect();
            placed.is_some_and(|bbox| {
                bbox.x < page.right() && bbox.right() > page.x && bbox.y < page.bottom() && bbox.bottom() > page.y
            })
        })
        .map(move |(offset, _)| first + offset)
}

/* ------------------------------------------------------------------ */
/* Appearance: the identity a tile URL is cached under                 */
/* ------------------------------------------------------------------ */

/// Bump when the digest's inputs change meaning, so no old URL can name a
/// differently drawn tile.
const APPEARANCE_VERSION: &str = "appearance-v3-managed-orientation";

fn short_digest(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(text.as_bytes());
    digest.iter().take(8).map(|byte| format!("{byte:02x}")).collect()
}

/// Everything about one patch record that the compositor reads: the files
/// that hold its mask and pixels, its box, its place in the stack, whether it
/// is shown, and its opacity and geometry.
///
/// **Content is named by the sidecar references.** Every patch a job stores
/// goes through `Job::complete_region_with_policy`, which names its mask and
/// buffer `patch-revisions/<region>/<sha256>` over the mask, lettering and
/// pixel bytes, so a re-run that draws different pixels is a different
/// `buffer_ref` even within the same second and on the same rung. A v3 row
/// still carrying a mutable `<id>.buf` is never rewritten in place: a re-run
/// archives it first and the new row gets a content name. The commit-time
/// `mask_sha256`, `created` and engine are folded in as well; nothing
/// rewrites them after the commit.
///
/// **Nothing else in the provenance is.** Its params snapshot collects
/// bookkeeping after the commit - the review state a dependency flag saved
/// (`review_before_input_change`), and the layer's position lock - and none of
/// it changes a pixel. An immutable cache is only honest if the key moves
/// exactly when the pixels do, so a lower layer's edit and its undo must leave
/// this digest where it was.
pub fn record_appearance(record: &PatchRecord) -> String {
    let style = cleaner_core::patch::LayerStyle::from_snapshot(&record.provenance.params_snapshot);
    short_digest(&format!(
        "{APPEARANCE_VERSION}|{}|{}|{},{},{}x{}|{}|{}|{:?}|{}|{}|{:?}|{}|{}|{}|{}|{}|{}|{}",
        record.id,
        record.source_idx,
        record.bbox.x,
        record.bbox.y,
        record.bbox.w,
        record.bbox.h,
        record.order,
        record.visible,
        record.engine,
        record.buffer_ref,
        record.mask_ref,
        record.geometry_policy,
        record.text_shape_plan_identity.as_deref().unwrap_or(""),
        record.provenance.mask_sha256,
        record.provenance.created,
        style.opacity,
        style.offset_x,
        style.offset_y,
        style.rotation,
    ))
}

/// [`record_appearance`] without what the interface draws itself: the
/// layer's opacity, and its place and visibility in the stack. It moves
/// exactly when the pixels a `layer` response holds do - the patch's content,
/// box and geometry - so an opacity change or a reorder fetches nothing.
pub fn layer_appearance(record: &PatchRecord) -> String {
    let style = cleaner_core::patch::LayerStyle::from_snapshot(&record.provenance.params_snapshot);
    short_digest(&format!(
        "{APPEARANCE_VERSION}|layer|{}|{}|{},{},{}x{}|{:?}|{}|{}|{:?}|{}|{}|{}|{}|{}|{}",
        record.id,
        record.source_idx,
        record.bbox.x,
        record.bbox.y,
        record.bbox.w,
        record.bbox.h,
        record.engine,
        record.buffer_ref,
        record.mask_ref,
        record.geometry_policy,
        record.text_shape_plan_identity.as_deref().unwrap_or(""),
        record.provenance.mask_sha256,
        record.provenance.created,
        style.offset_x,
        style.offset_y,
        style.rotation,
    ))
}

/// How a chapter's records reach its pages, built once per manifest read.
struct Layout<'a> {
    project: &'a Project,
    /// Every position in `strip.order` that shows a source, by source index.
    positions: Vec<Vec<usize>>,
    /// The stacked strip, in a longstrip; `None` when pages are their own.
    strip: Option<Strip>,
}

impl<'a> Layout<'a> {
    fn of(project: &'a Project) -> Layout<'a> {
        let mut positions = vec![Vec::new(); project.sources.len()];
        for (position, source_idx) in project.strip.order.iter().enumerate() {
            if let Some(slot) = positions.get_mut(*source_idx) {
                slot.push(position);
            }
        }
        let strip = (project.strip.mode == StripMode::Longstrip).then(|| strip_by_position(project));
        Layout { project, positions, strip }
    }

    /// The page a record is drawn from: the first position showing its source.
    fn anchor(&self, record: &PatchRecord) -> Option<usize> {
        self.positions.get(record.source_idx)?.first().copied()
    }

    /// Every page a record draws on, with the anchor it is drawn from - the
    /// selection `render_job` composites, read from the manifest alone.
    fn reach(&self, record: &PatchRecord) -> Vec<(usize, usize)> {
        let Some(anchor) = self.anchor(record) else { return Vec::new() };
        match &self.strip {
            Some(strip) => strip_reach(strip, anchor, cleaner_core::project::orientation::display_bbox(self.project, record)).map(|page| (page, anchor)).collect(),
            None => self.positions[record.source_idx].iter().map(|page| (*page, anchor)).collect(),
        }
    }

    /// Where a page sits in the strip, as a digest part; empty off the strip.
    fn placement(&self, position: usize) -> String {
        self.strip
            .as_ref()
            .and_then(|strip| strip.pages().get(position))
            .map(|page| format!("{},{},{}x{}", page.x_offset, page.y_offset, page.width, page.height))
            .unwrap_or_default()
    }

    /// One page's digest, from the records that reach it.
    fn fold(&self, project: &Project, position: usize, mut reaching: Vec<(&PatchRecord, usize)>) -> String {
        let Some(source_idx) = library::Library::resolve_page(project, position) else {
            return String::new();
        };
        let source_sha = project.sources.get(source_idx).map(|source| source.sha256.as_str()).unwrap_or("");
        let mut text = format!(
            "{APPEARANCE_VERSION}|{}x{}|{source_sha}",
            cleaner_core::image::proxy::PROXY_SHORT_EDGE,
            cleaner_core::image::proxy::PROXY_TILE_LONG_EDGE,
        );
        if self.strip.is_some() {
            text.push_str(&format!("|strip@{}", self.placement(position)));
        }
        reaching.sort_by(|(a, _), (b, _)| a.order.cmp(&b.order).then_with(|| a.id.cmp(&b.id)));
        for (record, anchor) in reaching {
            text.push('|');
            text.push_str(&record_appearance(record));
            if self.strip.is_some() {
                text.push_str(&format!("@{anchor}:{}", self.placement(anchor)));
            }
        }
        short_digest(&text)
    }
}

/// The cache identity of a page's `cleaned` tiles, from the manifest alone.
///
/// Built from the same selection [`render_job`] composites - the page's own
/// visible records, or in a longstrip every visible record whose displayed
/// box reaches this page across a join - in compositing order, so a patch
/// moved onto a neighbour changes the neighbour's identity too. It folds the
/// proxy geometry this side cuts tiles with, which closes the gap
/// `src/lib/api/tile.js` documents from its side: moving that constant here
/// now moves every URL.
///
/// Because it is read out of the manifest, it survives a reload, a reopen and
/// an undo: the same saved state always answers the same identity, and any
/// saved change to what the page shows answers a different one. The
/// interface folds it into `v` and sends it back as `a`, and [`serve`]
/// compares it with the manifest it renders from before it lets a response be
/// cached.
///
/// One page, one pass over the records. A caller that wants every page of a
/// chapter asks [`page_appearances`], which is the same answer for each.
pub fn page_appearance(project: &Project, page_index: usize) -> String {
    let layout = Layout::of(project);
    let reaching = project
        .patches
        .iter()
        .filter(|record| record.visible)
        .filter_map(|record| {
            layout.reach(record).into_iter().find(|(page, _)| *page == page_index).map(|(_, anchor)| (record, anchor))
        })
        .collect();
    layout.fold(project, page_index, reaching)
}

/// [`page_appearance`] of the page with nothing drawn on it: what it is when
/// no visible patch reaches it. A page whose appearance is this one shows its
/// source alone.
pub fn source_appearance(project: &Project, page_index: usize) -> String {
    Layout::of(project).fold(project, page_index, Vec::new())
}

/// [`page_appearance`] for every page of a chapter, by position, in one pass:
/// each record is placed once and handed to the pages it reaches, so a
/// chapter listing costs its records plus its pages rather than their product.
pub fn page_appearances(project: &Project) -> Vec<String> {
    let layout = Layout::of(project);
    let mut buckets: Vec<Vec<(&PatchRecord, usize)>> = vec![Vec::new(); project.strip.order.len()];
    for record in project.patches.iter().filter(|record| record.visible) {
        for (page, anchor) in layout.reach(record) {
            if let Some(bucket) = buckets.get_mut(page) {
                bucket.push((record, anchor));
            }
        }
    }
    buckets.into_iter().enumerate().map(|(position, reaching)| layout.fold(project, position, reaching)).collect()
}

/// Display derivatives are explicitly tagged sRGB, using the same transform for full and tiled views.
fn encode_for_display(raster: &Raster, preview: bool) -> Result<Vec<u8>, cleaner_core::image::ImageError> {
    let display = cleaner_core::image::proxy::display(raster)?;
    if preview { encode_png_preview(&display) } else { encode(&display, Format::Png) }
}

/// The handler, over a chapter resolver.
///
/// The resolver is a parameter rather than a direct call to
/// `library::resolve_chapter` for two reasons: this module can then be tested
/// against a job on disk with no index at all, and the library and the protocol
/// are two files that would otherwise have to land together. (`render_job` does
/// call `library::Library::resolve_page`, which is arithmetic over a `Project`
/// already in hand and touches neither the index nor the disk - a second copy
/// of that rule is exactly what this module must not have.)
///
/// It returns `Result<PathBuf, String>` rather than the library's own error
/// because there is exactly one thing this can do with a failure - a `404` and
/// the message in the body - so a typed error would be widened here and never
/// read.
///
/// **Asynchronous, and never on the UI thread.** WKWebView calls a scheme
/// handler on the main thread, and a synchronous handler answers there - so
/// every tile used to read the library index, parse the manifest and decode
/// the whole page on the thread that also delivers scroll and pinch events.
/// Scrolling a strip stalled for as long as the tiles coming into view took to
/// draw. The work now runs on the blocking executor, the same place
/// `library::run_blocking` sends every filesystem command, and the webview is
/// answered when it is done.
pub fn protocol<F>(
    resolve: F,
) -> impl Fn(tauri::UriSchemeContext<'_, tauri::Wry>, Request<Vec<u8>>, tauri::UriSchemeResponder)
+ Send
+ Sync
+ 'static
where
    F: Fn(&tauri::AppHandle, &str) -> Result<PathBuf, String> + Send + Sync + 'static,
{
    let resolve = Arc::new(resolve);
    move |ctx, request, responder| {
        let app = ctx.app_handle().clone();
        let resolve = Arc::clone(&resolve);
        let path = request.uri().path().to_owned();
        let expected = expected_appearance(request.uri().query());
        tauri::async_runtime::spawn_blocking(move || {
            responder.respond(serve(&app, &*resolve, &path, expected.as_deref()).unwrap_or_else(refuse));
        });
    }
}

/// The `a` query parameter: the appearance digest the interface built this
/// URL from, which is the `page.appearance` its last page listing carried.
pub fn expected_appearance(query: Option<&str>) -> Option<String> {
    query?
        .split('&')
        .find_map(|pair| pair.strip_prefix("a="))
        .filter(|digest| !digest.is_empty())
        .map(str::to_owned)
}

/// Whether a response may be cached under its URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Freshness {
    /// The bytes are what the URL names, for as long as the URL exists.
    Immutable,
    /// The URL was built from another saved state than the one drawn: the
    /// bytes are right for now and must not be kept. `current` is the
    /// appearance they were drawn from.
    Uncached { current: String },
}

/// Draw a request and say whether its URL may keep the answer.
///
/// A `cleaned` URL is cached immutably only when its `a` names the saved
/// state the bytes come from. The interface builds a URL from its own copy of
/// the page, and that copy can be older than the manifest - a page listing
/// read before a layer write landed, installed after it - so an old state's
/// URL would otherwise cache the new state's pixels for good. `source` bytes
/// are the source file's alone, which its sha already names.
pub fn serve_request(
    job_path: &Path,
    request: &TileRequest,
    expected: Option<&str>,
) -> Result<(Vec<u8>, Freshness), TileError> {
    let job = Job::open(job_path)?;
    let current = match request.variant {
        Variant::Source => None,
        Variant::Cleaned => Some(page_appearance(&job.project, request.page_index)),
    };
    let bytes = match request.tile {
        Some(tile) => {
            let identity = match &current {
                Some(appearance) => appearance.clone(),
                None => source_identity(&job, job_path, request.page_index)?,
            };
            let key = PageKey {
                job: job_path.to_path_buf(),
                page_index: request.page_index,
                variant: request.variant,
                identity,
            };
            TILES.tile(key, tile, || render_tiles(&job, job_path, request.page_index, request.variant))?
        }
        None => render_job(&job, job_path, request.page_index, request.variant, None)?,
    };
    let freshness = match current {
        None => Freshness::Immutable,
        Some(current) if expected == Some(current.as_str()) => Freshness::Immutable,
        Some(current) => Freshness::Uncached { current },
    };
    Ok((bytes, freshness))
}

/// What a page's `source` tiles are drawn from: the source's own `sha256`,
/// which is also what the interface's `v` token names for them.
fn source_identity(job: &Job, job_path: &Path, page_index: usize) -> Result<String, TileError> {
    library::Library::resolve_page(&job.project, page_index)
        .and_then(|source_idx| job.project.sources.get(source_idx))
        .map(|source| source.sha256.clone())
        .ok_or_else(|| TileError::NoSuchPage { job: job_path.display().to_string(), page_index })
}

/// One detection's display set, drawn: the PNG and where it sits on the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectionMask {
    /// The image's top-left and size, in page pixels.
    pub bounds: Rect,
    pub png: Vec<u8>,
}

/// Draw a stored detection's display set, cropped to the pixels it holds:
/// white and opaque inside, transparent outside, one image pixel per page
/// pixel.
///
/// The detection has to be on the page the path names. An id that is a patch,
/// an untouched row or another page's detection is a `404`, as is a detection
/// with nothing to draw. The job is opened to read only: drawing never takes
/// the lock or sweeps a file.
pub fn serve_detection(job_path: &Path, request: &DetectionRequest) -> Result<DetectionMask, TileError> {
    let job = Job::open_read_only(job_path)?;
    let source_idx = library::Library::resolve_page(&job.project, request.page_index).ok_or_else(|| {
        TileError::NoSuchPage { job: job_path.display().to_string(), page_index: request.page_index }
    })?;
    let no_such = || TileError::NoSuchDetection {
        page_index: request.page_index,
        region_id: request.region_id.clone(),
    };
    if !job.project.detections.iter().any(|row| row.id == request.region_id && row.source_idx == source_idx) {
        return Err(no_such());
    }
    let found = job.load_detection(&request.region_id)?.ok_or_else(no_such)?;
    let display = crate::region::display_set(&found.mask, &found.ink)
        .ok_or_else(|| TileError::EmptyDetection(request.region_id.clone()))?;
    let mut data = vec![0u8; display.bits.len() * 4];
    for (pixel, bit) in data.chunks_exact_mut(4).zip(&display.bits) {
        if *bit != 0 {
            pixel.fill(255);
        }
    }
    let raster = Raster {
        width: display.bounds.w,
        height: display.bounds.h,
        mode: ColorMode::Rgba,
        depth: cleaner_core::image::BitDepth::Eight,
        icc: None,
        palette: None,
        trns: None,
        srgb_intent: None,
        color: Default::default(),
        data,
    };
    Ok(DetectionMask { bounds: display.bounds, png: encode_png_preview(&raster)? })
}

/// One patch's layer on one page, drawn: the PNG and where it sits on the
/// page's proxy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layer {
    /// The image's top-left and size, in **proxy** pixels of the page the
    /// path names - the grid its tiles are cut from.
    pub bounds: TileRect,
    pub png: Vec<u8>,
}

/// Draw the part of one visible patch that lands on the page the path names,
/// as a see-through image on that page's proxy grid
/// ([`cleaner_core::image::proxy::managed_proxy_layer`]). `None` when none of it does:
/// a longstrip neighbour the patch does not reach, or a patch moved off the
/// page, which the interface is told with a `204`.
///
/// The selection is [`visible_patches`]'s, one record at a time: the page's
/// own patches, and in a longstrip the part of a neighbour's patch that
/// crosses the join. Nothing here decodes the page. The patch's files are
/// read and transformed, and the page contributes its size and mode from the
/// manifest and its profile from its header ([`source_header`]). An id that
/// is not a visible patch is a `404`. The job is opened to read only, and
/// shared by a page's worth of requests ([`read_job`]).
pub fn serve_layer(job_path: &Path, request: &DetectionRequest) -> Result<Option<Layer>, TileError> {
    let job = read_job(job_path)?;
    let no_such_page =
        || TileError::NoSuchPage { job: job_path.display().to_string(), page_index: request.page_index };
    let source_idx = library::Library::resolve_page(&job.project, request.page_index).ok_or_else(no_such_page)?;
    let source = job.project.sources.get(source_idx).ok_or_else(no_such_page)?;
    let record = job
        .project
        .patches
        .iter()
        .find(|record| record.visible && record.id == request.region_id)
        .ok_or_else(|| TileError::NoSuchLayer(request.region_id.clone()))?;

    let _ = record;
    let Some(mut patch) = job.display_patches_on_page(request.page_index)?.into_iter().find(|p| p.id == request.region_id) else { return Ok(None) };
    let (width, height) = source.orientation.size(source.w, source.h);
    let plan = ProxyPlan::for_page(width, height);
    let header = source_header(&job, job_path, source_idx)?;
    patch.pixels.icc = header.icc;
    patch.pixels.palette = header.palette;
    patch.pixels.trns = header.trns;
    patch.pixels.srgb_intent = header.srgb_intent;
    patch.pixels.color = header.color;
    let Some((bounds, raster)) = managed_proxy_layer(&patch.pixels, &patch.mask, &plan)? else { return Ok(None) };
    Ok(Some(Layer { bounds, png: encode_png_preview(&raster)? }))
}

/// A manifest opened to read only, shared while the file is unchanged.
///
/// A page's layers are asked for together - one request per patch, tens of
/// them - and each would otherwise read and parse the whole manifest, which
/// is megabytes on a cleaned chapter. The file is identified by its length,
/// its modification time and, where there is one, its inode: every save
/// replaces it by an atomic rename, which moves all three, so a changed
/// manifest is never answered from an old parse.
fn read_job(job_path: &Path) -> Result<Arc<Job>, TileError> {
    type Stamp = (u64, Option<std::time::SystemTime>, u64);
    type Read = (PathBuf, Stamp, Arc<Job>);
    static JOBS: LazyLock<Mutex<VecDeque<Read>>> = LazyLock::new(Mutex::default);
    const KEPT: usize = 4;
    let meta = std::fs::metadata(job_path).map_err(|source| TileError::Io { path: job_path.to_path_buf(), source })?;
    #[cfg(unix)]
    let inode = std::os::unix::fs::MetadataExt::ino(&meta);
    #[cfg(not(unix))]
    let inode = 0;
    let stamp: Stamp = (meta.len(), meta.modified().ok(), inode);
    if let Some((_, _, job)) = lock(&JOBS).iter().find(|(path, seen, _)| path == job_path && *seen == stamp) {
        return Ok(Arc::clone(job));
    }
    let job = Arc::new(Job::open_read_only(job_path)?);
    let mut jobs = lock(&JOBS);
    jobs.retain(|(path, _, _)| path != job_path);
    if jobs.len() >= KEPT {
        jobs.pop_front();
    }
    jobs.push_back((job_path.to_path_buf(), stamp, Arc::clone(&job)));
    Ok(job)
}

/// A source's ICC profile, read from its header and kept by the source's
/// `sha256`: a layer carries the page's profile exactly as the page's tiles
/// do, and the header is the only place it is written. Keyed by content, so
/// an entry is never stale; bounded by being dropped whole when it fills.
fn source_header(job: &Job, job_path: &Path, source_idx: usize) -> Result<cleaner_core::image::Header, TileError> {
    static PROFILES: LazyLock<Mutex<HashMap<String, cleaner_core::image::Header>>> = LazyLock::new(Mutex::default);
    const KEPT: usize = 256;
    let sha = job.project.sources.get(source_idx).map(|source| source.sha256.clone()).unwrap_or_default();
    if let Some(profile) = lock(&PROFILES).get(&sha) {
        return Ok(profile.clone());
    }
    let path = job.source_path(source_idx).ok_or_else(|| TileError::NoSuchPage {
        job: job_path.display().to_string(),
        page_index: source_idx,
    })?;
    let bytes = std::fs::read(&path).map_err(|source| TileError::Io { path, source })?;
    let header = cleaner_core::image::header(&bytes)?;
    let mut profiles = lock(&PROFILES);
    if profiles.len() >= KEPT {
        profiles.clear();
    }
    profiles.insert(sha, header.clone());
    Ok(header)
}

/* ------------------------------------------------------------------ */
/* The tile cache                                                      */
/* ------------------------------------------------------------------ */

/// One page's tiles under one saved state. `identity` is the appearance
/// digest for `cleaned` and the source's hash for `source`: the same identity
/// an immutable URL is cached under, so an entry is never stale - an edit
/// makes a new key and the old one ages out.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PageKey {
    job: PathBuf,
    page_index: usize,
    variant: Variant,
    identity: String,
}

/// A page's encoded tiles, filled by whichever request arrives first. Later
/// requests for the same page wait on the lock instead of decoding it again.
type Slot = Arc<Mutex<Option<Arc<Vec<Vec<u8>>>>>>;

/// Encoded tiles kept across requests. The webview's own cache is not
/// dependable for a custom scheme, and a strip unmounts a page's `<img>`s as it
/// scrolls past them, so the same tiles are asked for again and again.
const CACHE_BYTES: usize = 256 << 20;

/// Pages decoded at once. A decode holds the whole page - tens of MB for a
/// 20 000 px segment - so this bounds the peak rather than the throughput.
const RENDERS: usize = 3;

static TILES: LazyLock<TileCache> = LazyLock::new(TileCache::default);

#[derive(Default)]
struct TileCache {
    state: Mutex<CacheState>,
    busy: Mutex<usize>,
    freed: Condvar,
}

#[derive(Default)]
struct CacheState {
    entries: HashMap<PageKey, (Slot, usize)>,
    /// Least recently used first.
    order: VecDeque<PageKey>,
    bytes: usize,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl TileCache {
    /// Tile `index` of the page `key` names, drawing the page's tiles with
    /// `render` only when no earlier request has. A failure is not kept: the
    /// next request tries again.
    fn tile(
        &self,
        key: PageKey,
        index: usize,
        render: impl FnOnce() -> Result<Vec<Vec<u8>>, TileError>,
    ) -> Result<Vec<u8>, TileError> {
        let slot = self.slot(&key);
        let tiles = {
            let mut filled = lock(&slot);
            match filled.as_ref() {
                Some(tiles) => Arc::clone(tiles),
                None => {
                    let tiles = Arc::new(self.gated(render)?);
                    *filled = Some(Arc::clone(&tiles));
                    self.settle(&key, &slot, tiles.iter().map(Vec::len).sum());
                    tiles
                }
            }
        };
        tiles.get(index).cloned().ok_or(TileError::NoSuchTile {
            page_index: key.page_index,
            tile: index,
            tiles: tiles.len(),
        })
    }

    fn slot(&self, key: &PageKey) -> Slot {
        let mut state = lock(&self.state);
        if let Some(at) = state.order.iter().position(|candidate| candidate == key) {
            let key = state.order.remove(at).expect("a position just found");
            state.order.push_back(key);
        }
        if let Some((slot, _)) = state.entries.get(key) {
            return Arc::clone(slot);
        }
        let slot = Slot::default();
        state.entries.insert(key.clone(), (Arc::clone(&slot), 0));
        state.order.push_back(key.clone());
        slot
    }

    /// Count a filled page and evict the least recently used until the cache
    /// fits again. The page just filled is never the one evicted.
    fn settle(&self, key: &PageKey, slot: &Slot, bytes: usize) {
        let mut state = lock(&self.state);
        match state.entries.get_mut(key) {
            // Evicted while it was drawing: the requests holding it are
            // answered, and nothing is counted for an entry the cache dropped.
            Some((held, size)) if Arc::ptr_eq(held, slot) => *size = bytes,
            _ => return,
        }
        state.bytes += bytes;
        while state.bytes > CACHE_BYTES {
            let Some(at) = state.order.iter().position(|candidate| candidate != key) else { break };
            let oldest = state.order.remove(at).expect("a position just found");
            if let Some((_, size)) = state.entries.remove(&oldest) {
                state.bytes -= size;
            }
        }
    }

    fn gated<T>(&self, work: impl FnOnce() -> T) -> T {
        {
            let mut busy = lock(&self.busy);
            while *busy >= RENDERS {
                busy = self.freed.wait(busy).unwrap_or_else(|poisoned| poisoned.into_inner());
            }
            *busy += 1;
        }
        struct Release<'a>(&'a TileCache);
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                *lock(&self.0.busy) -= 1;
                self.0.freed.notify_one();
            }
        }
        let _release = Release(self);
        work()
    }
}

fn serve<F>(
    app: &tauri::AppHandle,
    resolve: &F,
    path: &str,
    expected: Option<&str>,
) -> Result<Response<Vec<u8>>, TileError>
where
    F: Fn(&tauri::AppHandle, &str) -> Result<PathBuf, String>,
{
    let job_path = |chapter_id: &str| {
        resolve(app, chapter_id).map_err(|_| TileError::NoSuchChapter(chapter_id.to_owned()))
    };
    match route(path)? {
        Route::Page(request) => {
            let (bytes, freshness) = serve_request(&job_path(&request.chapter_id)?, &request, expected)?;
            Ok(ok(bytes, freshness))
        }
        Route::Detection(request) => {
            Ok(detection_response(serve_detection(&job_path(&request.chapter_id)?, &request)?))
        }
        Route::Layer(request) => Ok(layer_response(serve_layer(&job_path(&request.chapter_id)?, &request)?)),
    }
}

/// The header naming where a layer image sits on its page's proxy.
pub const LAYER_BOUNDS_HEADER: &str = "x-layer-bounds";

/// A patch's layer, never cached here: see the module note on `layer`. A
/// patch with nothing on this page is a `204`, which the interface caches
/// as "nothing to draw".
fn layer_response(layer: Option<Layer>) -> Response<Vec<u8>> {
    let builder = Response::builder()
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(header::CACHE_CONTROL, "no-store");
    let Some(layer) = layer else {
        return builder.status(StatusCode::NO_CONTENT).body(Vec::new()).expect("a response with static headers");
    };
    let TileRect { x, y, w, h } = layer.bounds;
    builder
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "image/png")
        .header(LAYER_BOUNDS_HEADER, format!("{x},{y},{w},{h}"))
        .header(header::ACCESS_CONTROL_EXPOSE_HEADERS, LAYER_BOUNDS_HEADER)
        .body(layer.png)
        .expect("a response with well-formed headers")
}

/// The header naming where a detection's mask image sits on its page.
pub const MASK_BOUNDS_HEADER: &str = "x-mask-bounds";

/// A detection's mask, never cached: see the module note on `detection`.
fn detection_response(mask: DetectionMask) -> Response<Vec<u8>> {
    let Rect { x, y, w, h } = mask.bounds;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "image/png")
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(header::CACHE_CONTROL, "no-store")
        .header(MASK_BOUNDS_HEADER, format!("{x},{y},{w},{h}"))
        .header(header::ACCESS_CONTROL_EXPOSE_HEADERS, MASK_BOUNDS_HEADER)
        .body(mask.png)
        .expect("a response with well-formed headers")
}

/// The header naming the appearance a response was drawn from, when its URL
/// named another.
pub const APPEARANCE_HEADER: &str = "x-tile-appearance";

/// A whole-response image.
///
/// `immutable` is honest only because the URL carries the version token and
/// [`serve_request`] has checked its `a` against the manifest the bytes came
/// from. A URL built from any other state is answered `no-store`, with the
/// state it was drawn from in [`APPEARANCE_HEADER`].
fn ok(bytes: Vec<u8>, freshness: Freshness) -> Response<Vec<u8>> {
    let builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "image/png")
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*");
    let builder = match freshness {
        Freshness::Immutable => builder.header(header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        Freshness::Uncached { current } => builder
            .header(header::CACHE_CONTROL, "no-store")
            .header(APPEARANCE_HEADER, current)
            .header(header::ACCESS_CONTROL_EXPOSE_HEADERS, APPEARANCE_HEADER),
    };
    builder.body(bytes).expect("a response with static headers")
}

/// The message is English, and it is allowed to be: it goes to the devtools
/// console, never to a user. Nothing here reaches the seam, where
/// "no English crosses
/// the seam" applies - a broken `<img>` is what the user sees, and the
/// interface's own empty states cover it.
fn refuse(error: TileError) -> Response<Vec<u8>> {
    Response::builder()
        .status(error.status())
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .body(error.to_string().into_bytes())
        .expect("a response with static headers")
}

#[cfg(test)]
mod tests {
    use super::*;
    use cleaner_core::image::{fixtures, lossless_format_for};
    use cleaner_core::mask::{Mask, Rect};
    use cleaner_core::patch::{Engine, Provenance};
    use cleaner_core::project::{Project, StripMode};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            use std::sync::atomic::{AtomicU32, Ordering};
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir()
                .join("manga-cleaner-tile")
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

    /// A one-page job over a named fixture, written to disk exactly as a real
    /// one is.
    fn a_job(scratch: &Scratch, fixture: &str) -> Job {
        let page = fixtures::by_name(fixture).raster;
        let bytes = encode(&page, lossless_format_for(&page)).unwrap();
        let raws = scratch.join("raws");
        std::fs::create_dir_all(&raws).unwrap();
        let source = raws.join(format!("001.{}", match lossless_format_for(&page) {
            Format::Png => "png",
            Format::Tiff => "tif",
            Format::Jpeg => unreachable!("lossless_format_for never selects JPEG"),
        }));
        std::fs::write(&source, &bytes).unwrap();
        let reference = cleaner_core::ingest::source_ref(&source, &bytes).unwrap();

        let manifest = scratch.join("out/chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let project =
            Project::new(manifest.parent().unwrap(), "0.1.0", StripMode::Single, &[reference]);
        Job::create(&manifest, project).unwrap()
    }

    /// A job over `count` distinct pages, whose `strip.order` is whatever the
    /// caller says - the case where a page index and a source index part
    /// company.
    fn a_strip(scratch: &Scratch, count: usize, order: Vec<usize>) -> Job {
        let raws = scratch.join("raws");
        std::fs::create_dir_all(&raws).unwrap();
        let mut references = Vec::new();
        for n in 0..count {
            let mut page = fixtures::by_name("l8").raster;
            // Distinct bytes per page, so a test can say which one it got.
            page.data[0] = 10 * (n as u8 + 1);
            let bytes = encode(&page, Format::Png).unwrap();
            let source = raws.join(format!("{:03}.png", n + 1));
            std::fs::write(&source, &bytes).unwrap();
            references.push(cleaner_core::ingest::source_ref(&source, &bytes).unwrap());
        }

        let manifest = scratch.join("out/chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let mut project =
            Project::new(manifest.parent().unwrap(), "0.1.0", StripMode::Single, &references);
        project.strip.order = order;
        Job::create(&manifest, project).unwrap()
    }

    /// A one-page job over a flat gray page of a stated size - the shape a
    /// webtoon segment has and no fixture does (they are all 64×48).
    fn a_big_job(scratch: &Scratch, width: u32, height: u32) -> Job {
        let page = Raster {
            width,
            height,
            mode: ColorMode::Gray,
            depth: cleaner_core::image::BitDepth::Eight,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None,
            color: Default::default(),
            data: vec![40; (width as usize) * (height as usize)],
        };
        let bytes = encode(&page, Format::Png).unwrap();
        let raws = scratch.join("raws");
        std::fs::create_dir_all(&raws).unwrap();
        let source = raws.join("001.png");
        std::fs::write(&source, &bytes).unwrap();
        let reference = cleaner_core::ingest::source_ref(&source, &bytes).unwrap();

        let manifest = scratch.join("out/chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let project =
            Project::new(manifest.parent().unwrap(), "0.1.0", StripMode::Single, &[reference]);
        Job::create(&manifest, project).unwrap()
    }

    /// A patch that paints a flat block over the middle of the page, in the
    /// page's own mode.
    fn a_patch(page: &Raster, bounds: Rect, value: u16) -> Patch {
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
                    pixels.set_sample(x, y, channel, value);
                }
            }
        }
        Patch {
            id: "r0".into(),
            mask: Mask::filled(bounds),
            ink: Mask::filled(bounds),
            pixels,
            order: 0,
            visible: true,
            provenance: Provenance {
                engine: Engine::Fill,
                engine_version: "test".into(),
                model_sha256: None,
                execution_provider: "cpu".into(),
                params_snapshot: serde_json::json!({}),
                mask_sha256: "0".repeat(64),
                source_sha256: "1".repeat(64),
                cloud: None,
                created: 0,
            },
        }
    }

    #[test]
    fn a_url_parses_into_a_chapter_a_page_and_a_variant() {
        assert_eq!(
            parse("/ch-01/7/cleaned").unwrap(),
            TileRequest {
                chapter_id: "ch-01".into(),
                page_index: 7,
                variant: Variant::Cleaned,
                tile: None,
            }
        );
        assert_eq!(parse("/ch-01/0/source").unwrap().variant, Variant::Source);
    }

    /// The fourth segment, and the difference between "the page" and "tile 0 of
    /// the page's proxy" - which are the same picture only for a page small
    /// enough that the proxy is a copy.
    #[test]
    fn a_fourth_segment_names_a_proxy_tile() {
        assert_eq!(
            parse("/ch-01/7/cleaned/3").unwrap(),
            TileRequest {
                chapter_id: "ch-01".into(),
                page_index: 7,
                variant: Variant::Cleaned,
                tile: Some(3),
            }
        );
        assert_eq!(parse("/ch-01/7/cleaned/0").unwrap().tile, Some(0));
        assert_eq!(parse("/ch-01/7/cleaned").unwrap().tile, None);
        // Not an index, so not a tile. A guess here would serve tile 0 under a
        // URL that asked for something else.
        assert_eq!(parse("/ch-01/7/cleaned/-1").unwrap_err().status(), StatusCode::BAD_REQUEST);
        assert_eq!(parse("/ch-01/7/cleaned/first").unwrap_err().status(), StatusCode::BAD_REQUEST);
    }

    /// The query is the interface's business, not this module's: a version
    /// token it does not understand must not change what it serves.
    #[test]
    fn the_version_token_is_not_part_of_the_path_and_changes_nothing() {
        // `Request::uri().path()` has already dropped the query; the assertion
        // that matters is that a path is all `parse` is ever given, so a `?v=`
        // reaching it would be a malformed path rather than a silent match.
        assert!(parse("/ch-01/0/source?v=abc").is_err());
        assert_eq!(parse("/ch-01/0/source").unwrap().page_index, 0);
    }

    #[test]
    fn a_chapter_id_arrives_percent_decoded() {
        assert_eq!(parse("/ch%2001/0/source").unwrap().chapter_id, "ch 01");
        // A literal `%` that is not an escape survives rather than becoming a
        // replacement character, which would match no chapter at all.
        assert_eq!(parse("/100%/0/source").unwrap().chapter_id, "100%");
    }

    #[test]
    fn a_malformed_path_is_a_bad_request_rather_than_a_guess() {
        for path in ["/", "/ch-01", "/ch-01/0", "/ch-01/0/source/extra", "//0/source"] {
            let error = parse(path).expect_err(path);
            assert_eq!(error.status(), StatusCode::BAD_REQUEST, "{path}");
        }
        assert_eq!(parse("/ch-01/x/source").unwrap_err().status(), StatusCode::BAD_REQUEST);
        // A variant nobody serves is refused rather than defaulted to `source`:
        // a tile that silently shows the uncleaned page is worse than no tile.
        assert_eq!(parse("/ch-01/0/original").unwrap_err().status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn a_page_the_chapter_does_not_have_is_a_404() {
        let scratch = Scratch::new("no-page");
        let job = a_job(&scratch, "l8");
        let error = render(job.path(), 4, Variant::Source, None).unwrap_err();
        assert_eq!(error.status(), StatusCode::NOT_FOUND);
    }

    /// The URL's page index is a position in `strip.order`, not an index into
    /// `sources` - `library.rs`'s own module doc says so, and `exporting.rs`
    /// already iterates `strip.order`. They are equal only until something
    /// reorders the pages, and then this protocol serves the wrong page's
    /// pixels under a URL the interface believes is immutable.
    #[test]
    fn a_reordered_strip_serves_the_page_the_order_names() {
        let scratch = Scratch::new("reordered");
        let job = a_strip(&scratch, 3, vec![2, 0, 1]);

        for (page_index, source_idx) in [(0usize, 2usize), (1, 0), (2, 1)] {
            let on_disk = std::fs::read(job.source_path(source_idx).unwrap()).unwrap();
            assert_eq!(
                render(job.path(), page_index, Variant::Source, None).unwrap(),
                encode_for_display(&decode(&on_disk).unwrap(), false).unwrap(),
                "page {page_index} was not source {source_idx}"
            );
        }
    }

    /// A patch is recorded against a *source*, so a reordered strip must not
    /// move it: page 0's cleaned tile carries source 2's patch, and page 2's
    /// carries none.
    #[test]
    fn a_patch_follows_its_source_rather_than_its_position() {
        let scratch = Scratch::new("reordered-patch");
        let mut job = a_strip(&scratch, 3, vec![2, 0, 1]);
        let page = decode(&std::fs::read(job.source_path(2).unwrap()).unwrap()).unwrap();
        job.complete_region(2, &a_patch(&page, Rect::new(8, 8, 12, 10), 200), None).unwrap();

        let cleaned = decode(&render(job.path(), 0, Variant::Cleaned, None).unwrap()).unwrap();
        assert_eq!(cleaned.sample(10, 10, 0), 200, "the patch is not on the page its source is");
        assert_eq!(
            render(job.path(), 2, Variant::Cleaned, None).unwrap(),
            render(job.path(), 2, Variant::Source, None).unwrap(),
            "another page's patch reached this tile"
        );
    }

    /// A source `strip.order` does not name is not a page. `sources` still has
    /// it - that is what makes this the case a bare `source_path(page_index)`
    /// answers with pixels instead of a `404`.
    #[test]
    fn a_source_the_strip_dropped_is_not_reachable_as_a_page() {
        let scratch = Scratch::new("dropped");
        let job = a_strip(&scratch, 3, vec![1]);
        assert_eq!(job.project.sources.len(), 3);

        assert!(render(job.path(), 0, Variant::Source, None).is_ok());
        for page_index in [1, 2] {
            let error = render(job.path(), page_index, Variant::Source, None)
                .expect_err("a dropped source was served as a page");
            assert_eq!(error.status(), StatusCode::NOT_FOUND, "page {page_index}");
        }
    }

    /// The browser decodes natively, so every tile is an image format a
    /// webview has a decoder for. TIFF is not one on Windows, and the core
    /// reads TIFF sources.
    #[test]
    fn every_fixture_is_served_as_a_png_a_browser_can_decode() {
        for fixture in fixtures::all() {
            let scratch = Scratch::new("fixture");
            let job = a_job(&scratch, fixture.name);
            let bytes = render(job.path(), 0, Variant::Source, None).unwrap();
            assert_eq!(
                Format::sniff(&bytes),
                Some(Format::Png),
                "{} was served as something other than PNG",
                fixture.name
            );
            let out = decode(&bytes).unwrap();
            assert_eq!(out.width, fixture.raster.width, "{}", fixture.name);
            assert_eq!(out.height, fixture.raster.height, "{}", fixture.name);
        }
    }

    #[test]
    fn all_display_paths_emit_explicit_srgb_without_relabeling_source_profiles() {
        for fixture in fixtures::all() {
            let scratch=Scratch::new("managed-mode"); let job=a_job(&scratch,fixture.name);
            let whole=decode(&render(job.path(),0,Variant::Source,None).unwrap()).unwrap();
            let tile=decode(&render(job.path(),0,Variant::Source,Some(0)).unwrap()).unwrap();
            assert_eq!(whole.data,tile.data,"{}",fixture.name);
            assert_eq!(whole.srgb_intent,Some(1)); assert!(whole.icc.is_none());
            assert_eq!(whole.depth,cleaner_core::image::BitDepth::Eight);
            assert_eq!(std::fs::read(job.source_path(0).unwrap()).unwrap(),encode(&fixture.raster,lossless_format_for(&fixture.raster)).unwrap());
        }
    }

    /// The brief's sentence, asserted: "a page with no patches yields the
    /// source bytes; that is correct, not a stub." Including for a TIFF source,
    /// where the two are re-encoded rather than copied and must still agree.
    #[test]
    fn cleaned_and_source_are_the_same_image_until_something_is_applied() {
        let scratch = Scratch::new("no-patches");
        let job = a_job(&scratch, "cmyk8");
        assert_eq!(
            render(job.path(), 0, Variant::Source, None).unwrap(),
            render(job.path(), 0, Variant::Cleaned, None).unwrap()
        );
    }

    #[test]
    fn a_cleaned_tile_is_the_source_with_the_patches_composited_over_it() {
        let scratch = Scratch::new("cleaned");
        let mut job = a_job(&scratch, "l8");
        let page = decode(&std::fs::read(job.source_path(0).unwrap()).unwrap()).unwrap();
        let bounds = Rect::new(8, 8, 12, 10);
        job.complete_region(0, &a_patch(&page, bounds, 200), None).unwrap();

        let source = decode(&render(job.path(), 0, Variant::Source, None).unwrap()).unwrap();
        let cleaned = decode(&render(job.path(), 0, Variant::Cleaned, None).unwrap()).unwrap();

        assert_eq!(source.data, cleaner_core::image::proxy::display(&page).unwrap().data, "the source variant was not the managed source");
        assert_eq!(cleaned.sample(10, 10, 0), 200, "the patch is not on the cleaned tile");
        assert_eq!(
            cleaned.sample(0, 0, 0),
            page.sample(0, 0, 0),
            "the cleaned tile changed a pixel outside every mask"
        );
    }

    /// `visible` is persisted per patch precisely so it can be turned off, and
    /// the compositor honours it. The tile must too, or the canvas and the
    /// export disagree about what the page is.
    #[test]
    fn a_hidden_patch_is_not_on_the_cleaned_tile() {
        let scratch = Scratch::new("hidden");
        let mut job = a_job(&scratch, "l8");
        let page = decode(&std::fs::read(job.source_path(0).unwrap()).unwrap()).unwrap();
        let mut patch = a_patch(&page, Rect::new(8, 8, 12, 10), 200);
        patch.visible = false;
        job.complete_region(0, &patch, None).unwrap();

        assert_eq!(
            render(job.path(), 0, Variant::Cleaned, None).unwrap(),
            render(job.path(), 0, Variant::Source, None).unwrap()
        );
    }

    /// A patch belongs to one page. Serving another page's edits would be
    /// invisible on a one-page fixture and wrong on every real chapter.
    #[test]
    fn a_patch_on_another_page_does_not_reach_this_ones_tile() {
        let scratch = Scratch::new("other-page");
        let mut job = a_job(&scratch, "l8");
        let page = decode(&std::fs::read(job.source_path(0).unwrap()).unwrap()).unwrap();
        let mut patch = a_patch(&page, Rect::new(8, 8, 12, 10), 200);
        patch.id = "r-elsewhere".into();
        job.complete_region(1, &patch, None).unwrap();

        assert_eq!(
            render(job.path(), 0, Variant::Cleaned, None).unwrap(),
            render(job.path(), 0, Variant::Source, None).unwrap()
        );
    }

    /// The whole point: a 20 000 px segment must never reach the webview as
    /// one image, and it must not be squeezed to a sliver either. Ten tiles,
    /// each the page's full width, together covering it.
    #[test]
    fn a_long_page_is_served_as_proxy_tiles_and_never_as_one_image() {
        let scratch = Scratch::new("proxy-tiles");
        let job = a_big_job(&scratch, 800, 5000);
        let plan = ProxyPlan::for_page(800, 5000);
        assert_eq!(plan.tiles, 3);

        let mut rows = 0;
        for index in 0..plan.tiles {
            let tile =
                decode(&render(job.path(), 0, Variant::Source, Some(index)).unwrap()).unwrap();
            assert_eq!(tile.width, 800, "tile {index} did not span the page's width");
            rows += tile.height;
        }
        assert_eq!(rows, 5000, "the tiles did not cover the page");

        // And the tile nobody has is a 404, not the last one again.
        let error = render(job.path(), 0, Variant::Source, Some(plan.tiles)).unwrap_err();
        assert_eq!(error.status(), StatusCode::NOT_FOUND);
    }

    /// The short edge is what the cap applies to. A page wider than 1024 is
    /// reduced; the aspect ratio the interface draws it at is unchanged.
    #[test]
    fn a_proxy_caps_the_short_edge_and_keeps_the_aspect_ratio() {
        let scratch = Scratch::new("proxy-cap");
        let job = a_big_job(&scratch, 2400, 3000);
        let tile = decode(&render(job.path(), 0, Variant::Source, Some(0)).unwrap()).unwrap();
        assert_eq!(tile.width, 1024);
        // 3000 × 1024/2400 = 1280, which is one tile.
        assert_eq!(tile.height, 1280);
        assert!(render(job.path(), 0, Variant::Source, Some(1)).is_err());
    }

    /// A proxy is a view of the *cleaned* page when that is what was asked for -
    /// the patches are composited at native resolution first, so a mask does
    /// not move under the downscale.
    #[test]
    fn a_cleaned_proxy_tile_carries_the_page_s_patches() {
        let scratch = Scratch::new("proxy-cleaned");
        let mut job = a_big_job(&scratch, 2400, 3000);
        let page = decode(&std::fs::read(job.source_path(0).unwrap()).unwrap()).unwrap();
        job.complete_region(0, &a_patch(&page, Rect::new(0, 0, 240, 240), 200), None).unwrap();

        let cleaned = decode(&render(job.path(), 0, Variant::Cleaned, Some(0)).unwrap()).unwrap();
        // The block covers the first tenth of the page, so the proxy's first
        // 102 pixels are the patch and everything past it is the page.
        assert_eq!(cleaned.sample(50, 50, 0), 200, "the patch is not on the proxy");
        assert_eq!(cleaned.sample(500, 500, 0), 40, "the proxy changed a pixel outside the mask");

        let source = decode(&render(job.path(), 0, Variant::Source, Some(0)).unwrap()).unwrap();
        assert_eq!(source.sample(50, 50, 0), 40, "the source proxy carried a patch");
    }

    /// Passthrough survives the proxy path, and only where it is honest: tile 0
    /// of a page the proxy would neither reduce nor cut *is* the page.
    #[test]
    fn whole_and_tile_paths_agree_and_originals_stay_unchanged() {
        let scratch = Scratch::new("proxy-passthrough");
        let job = a_job(&scratch, "l8");
        let on_disk = std::fs::read(job.source_path(0).unwrap()).unwrap();
        let whole=render(job.path(),0,Variant::Source,None).unwrap();
        let tiled = decode(&render(job.path(),0,Variant::Source,Some(0)).unwrap()).unwrap();
        let whole = decode(&whole).unwrap();
        assert_eq!((tiled.width,tiled.height,tiled.mode,tiled.depth,tiled.srgb_intent),(whole.width,whole.height,whole.mode,whole.depth,whole.srgb_intent));
        assert!(tiled.data == whole.data);
        assert_eq!(std::fs::read(job.source_path(0).unwrap()).unwrap(),on_disk);
        // And a page that would be cut is not: the bytes served for tile 0 are
        // a tile, never the whole file.
        let tall_scratch = Scratch::new("proxy-passthrough-tall");
        let tall = a_big_job(&tall_scratch, 800, 5000);
        let whole = std::fs::read(tall.source_path(0).unwrap()).unwrap();
        assert_ne!(render(tall.path(), 0, Variant::Source, Some(0)).unwrap(), whole);
    }

    /// The response is whole, and says so. WebView2 supports neither
    /// ranges nor streaming, so a handler that advertised either would be
    /// advertising something one of the two platforms cannot do.
    #[test]
    fn a_response_is_whole_and_advertises_no_range_support() {
        let response = ok(vec![1, 2, 3], Freshness::Immutable);
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get(header::CONTENT_TYPE).unwrap(), "image/png");
        assert!(response.headers().get(header::ACCEPT_RANGES).is_none());
        assert!(response.headers().get(header::CONTENT_RANGE).is_none());
        assert_eq!(response.body(), &vec![1, 2, 3]);
    }

    /// The `immutable` on the response is only true because the URL carries a
    /// content-derived version token. If one is ever dropped the other has to
    /// go with it.
    #[test]
    fn the_response_is_cached_immutably() {
        let cache = ok(Vec::new(), Freshness::Immutable).headers().get(header::CACHE_CONTROL).unwrap().clone();
        assert!(cache.to_str().unwrap().contains("immutable"));
    }

    #[test]
    fn a_failure_carries_its_status_and_not_an_image_content_type() {
        let response = refuse(TileError::NoSuchChapter("ch-01".into()));
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_ne!(response.headers().get(header::CONTENT_TYPE).unwrap(), "image/png");
        assert!(String::from_utf8(response.body().clone()).unwrap().contains("ch-01"));
    }

    fn a_longstrip(scratch: &Scratch, count: usize) -> Job {
        let raws = scratch.join("raws");
        std::fs::create_dir_all(&raws).unwrap();
        let mut references = Vec::new();
        for n in 0..count {
            let mut page = fixtures::by_name("l8").raster;
            page.data[0] = 10 * (n as u8 + 1);
            let bytes = encode(&page, Format::Png).unwrap();
            let source = raws.join(format!("{:03}.png", n + 1));
            std::fs::write(&source, &bytes).unwrap();
            references.push(cleaner_core::ingest::source_ref(&source, &bytes).unwrap());
        }

        let manifest = scratch.join("out/chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let project =
            Project::new(manifest.parent().unwrap(), "0.1.0", StripMode::Longstrip, &references);
        Job::create(&manifest, project).unwrap()
    }

    /// In longstrip mode, a patch anchored on page 0 that spans past the bottom
    /// edge into page 1 must render onto page 1's cleaned tile.
    #[test]
    fn a_longstrip_patch_spanning_a_join_renders_on_both_pages() {
        let scratch = Scratch::new("longstrip-span");
        let mut job = a_longstrip(&scratch, 2);
        let page = fixtures::by_name("l8").raster;
        let bounds = Rect::new(10, 40, 20, 16);
        let mut pixels = page.clone();
        pixels.width = bounds.w;
        pixels.height = bounds.h;
        pixels.icc = None;
        pixels.data = vec![215; (bounds.w * bounds.h) as usize];
        let patch = Patch {
            id: "spanning".into(),
            mask: Mask::filled(bounds),
            ink: Mask::filled(bounds),
            pixels,
            order: 0,
            visible: true,
            provenance: Provenance {
                engine: Engine::Fill,
                engine_version: "test".into(),
                model_sha256: None,
                execution_provider: "cpu".into(),
                params_snapshot: serde_json::json!({}),
                mask_sha256: "0".repeat(64),
                source_sha256: "0".repeat(64),
                cloud: None,
                created: 0,
            },
        };
        job.complete_region(0, &patch, None).unwrap();

        // Page 0 has the top part of the patch (y: 40..48)
        let page0 = decode(&render(job.path(), 0, Variant::Cleaned, None).unwrap()).unwrap();
        assert_eq!(page0.sample(15, 42, 0), 215, "page 0 did not render patch");

        // Page 1 has the bottom part of the patch (y: 0..8)
        let page1 = decode(&render(job.path(), 1, Variant::Cleaned, None).unwrap()).unwrap();
        assert_eq!(page1.sample(15, 2, 0), 215, "page 1 did not render spanning patch");
    }

    #[test]
    fn moved_or_rotated_patch_reaches_the_new_longstrip_page() {
        for angle in [0.0, 90.0] {
            let scratch = Scratch::new(if angle == 0.0 { "moved-join" } else { "rotated-join" });
            let mut job = a_longstrip(&scratch, 2);
            let bounds = Rect::new(10, 10, 20, 10);
            let mut pixels = fixtures::by_name("l8").raster;
            pixels.width = bounds.w;
            pixels.height = bounds.h;
            pixels.icc = None;
            pixels.data = vec![215; (bounds.w * bounds.h) as usize];
            job.complete_region(0, &Patch {
                id: "moved".into(),
                mask: Mask::filled(bounds), ink: Mask::filled(bounds), pixels,
                order: 0, visible: true,
                provenance: Provenance {
                    engine: Engine::Fill, engine_version: "test".into(),
                    model_sha256: None, execution_provider: "cpu".into(),
                    params_snapshot: serde_json::json!({"layer": {"offsetY": 40, "rotation": angle}}),
                    mask_sha256: "0".repeat(64), source_sha256: "0".repeat(64),
                    cloud: None, created: 0,
                },
            }, None).unwrap();
            let source = decode(&render(job.path(), 1, Variant::Source, None).unwrap()).unwrap();
            let cleaned = decode(&render(job.path(), 1, Variant::Cleaned, None).unwrap()).unwrap();
            assert_ne!(source.sample(20, 5, 0), 215);
            assert_eq!(cleaned.sample(20, 5, 0), 215, "angle {angle} disappeared across the join");
        }
    }

    fn toned_patch(id: &str, bounds: Rect, tone: u8, layer: serde_json::Value) -> Patch {
        let mut pixels = fixtures::by_name("l8").raster;
        pixels.width = bounds.w;
        pixels.height = bounds.h;
        pixels.icc = None;
        pixels.data = vec![tone; (bounds.w * bounds.h) as usize];
        Patch {
            id: id.into(),
            mask: Mask::filled(bounds), ink: Mask::filled(bounds), pixels,
            order: 0, visible: true,
            provenance: Provenance {
                engine: Engine::Fill, engine_version: "test".into(),
                model_sha256: None, execution_provider: "cpu".into(),
                params_snapshot: serde_json::json!({"layer": layer}),
                mask_sha256: "0".repeat(64), source_sha256: "0".repeat(64),
                cloud: None, created: 0,
            },
        }
    }

    /// What the tile serves at 0, 50 and 100 percent, read back off disk
    /// the way the webview gets it: the page, the rounded half-way blend, and
    /// the patch. The flattened export of the same saved state is pinned to
    /// the same pixels in `exporting.rs`.
    #[test]
    fn a_tile_at_zero_half_and_full_opacity_shows_the_expected_pixels() {
        for opacity in [0u8, 50, 100] {
            let scratch = Scratch::new(&format!("opacity-{opacity}"));
            let mut job = a_job(&scratch, "l8");
            let bounds = Rect::new(8, 8, 12, 10);
            job.complete_region(0, &toned_patch("faded", bounds, 200, serde_json::json!({"opacity": opacity})), None)
                .unwrap();
            let source = decode(&render(job.path(), 0, Variant::Source, None).unwrap()).unwrap();
            let cleaned = decode(&render(job.path(), 0, Variant::Cleaned, Some(0)).unwrap()).unwrap();
            for (x, y) in [(10u32, 10u32), (19, 17), (8, 8)] {
                let below = source.sample(x, y, 0);
                let wanted = match opacity {
                    0 => below,
                    100 => 200,
                    _ => ((below as f64 + 200.0) / 2.0).round() as u16,
                };
                assert_eq!(cleaned.sample(x, y, 0), wanted, "{opacity}% at ({x}, {y})");
            }
            assert_eq!(cleaned.sample(30, 30, 0), source.sample(30, 30, 0), "{opacity}% leaked");
        }
    }

    /// **What the user sees is what the file holds, at 100 percent and below.**
    /// A cloud patch composited the way preprocessing 2.0.0 composites one -
    /// an answer that came back 14 levels bright, tone aligned and written
    /// through a feathered edge over screentone - is the export's pixels on
    /// the full-resolution tile, and on a proxy tile (a 1600 px short edge
    /// drawn at 1024, a fractional zoom) it is that export reduced by the same
    /// proxy function. The patch reaches the webview already flattened into
    /// the page, so no transparent edge texel is ever sampled against the
    /// artwork below it, and at both scales the lettering is gone with no
    /// contour left: within two levels of the page as it was under it.
    #[test]
    fn a_feathered_cloud_patch_is_the_export_on_the_full_tile_and_on_a_proxy_tile() {
        use cleaner_core::engines::render::{GeneratedCrop, PreparedRender};
        use cleaner_core::export::{export_page, Target};

        let scratch = Scratch::new("feathered-export");
        let (w, h) = (1600u32, 2000u32);
        let tone = |x: u32, y: u32| if x % 6 < 3 && y % 6 < 3 { 90u8 } else { 220 };
        let clean = Raster {
            width: w, height: h, mode: ColorMode::Gray, depth: cleaner_core::image::BitDepth::Eight,
            icc: None, palette: None, trns: None, srgb_intent: None,
            color: Default::default(), data: (0..h).flat_map(|y| (0..w).map(move |x| tone(x, y))).collect(),
        };
        let mut page = clean.clone();
        let glyph = Rect::new(700, 200, 60, 24);
        for y in glyph.y..glyph.bottom() {
            for x in glyph.x..glyph.right() {
                page.set_sample(x as u32, y as u32, 0, 15);
            }
        }
        let bytes = encode(&page, Format::Png).unwrap();
        std::fs::create_dir_all(scratch.join("raws")).unwrap();
        let source = scratch.join("raws/001.png");
        std::fs::write(&source, &bytes).unwrap();
        let reference = cleaner_core::ingest::source_ref(&source, &bytes).unwrap();
        let manifest = scratch.join("out/chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let project = Project::new(manifest.parent().unwrap(), "0.1.0", StripMode::Single, &[reference]);
        let mut job = Job::create(&manifest, project).unwrap();

        let seed = Mask::filled(glyph);
        let fitted = cleaner_core::fit::fit(&page, &seed, 1.0, 0.0, &cleaner_core::fit::EdgeMap::none(w, h), true);
        let prepared = PreparedRender::prepare(&page, &fitted).unwrap();
        let crop = prepared.crop();
        let mut answer = Vec::with_capacity((crop.w * crop.h * 3) as usize);
        for y in crop.y..crop.bottom() {
            for x in crop.x..crop.right() {
                let under = tone(x.clamp(0, w as i64 - 1) as u32, y.clamp(0, h as i64 - 1) as u32);
                answer.extend([under.saturating_add(14); 3]);
            }
        }
        let rendered = prepared.composite(&GeneratedCrop::new(crop.w, crop.h, &answer)).unwrap();
        let mut patch = a_patch(&page, glyph, 0);
        patch.id = "cloud".into();
        patch.mask = rendered.mask.clone();
        patch.ink = fitted.ink.clone();
        patch.pixels = rendered.pixels;
        job.complete_region(0, &patch, None).unwrap();

        let full = decode(&render(job.path(), 0, Variant::Cleaned, None).unwrap()).unwrap();
        let exported = decode(&export_page(&bytes, std::slice::from_ref(&patch), Target::SameAsSource).unwrap().bytes)
            .unwrap();
        assert!(full.data == cleaner_core::image::proxy::display(&exported).unwrap().data, "the full-resolution tile is not the managed export");
        let edited = rendered.mask.bounds;
        for y in edited.y..edited.bottom() {
            for x in edited.x..edited.right() {
                let (got, was) = (full.sample(x as u32, y as u32, 0), clean.sample(x as u32, y as u32, 0));
                assert!(got.abs_diff(was) <= 2, "({x}, {y}): {got} over {was} at 100%");
            }
        }

        let tile = decode(&render(job.path(), 0, Variant::Cleaned, Some(0)).unwrap()).unwrap();
        assert!(tile.width < w, "the proxy is not a fractional zoom");
        assert_eq!(tile.data, proxy_tile(&exported, 0).unwrap().data, "the proxy tile is not the export reduced");
        let under = proxy_tile(&clean, 0).unwrap();
        let scale = f64::from(tile.width) / f64::from(w);
        let (x0, y0) = ((edited.x as f64 * scale) as u32, (edited.y as f64 * scale) as u32);
        let (x1, y1) = ((edited.right() as f64 * scale).ceil() as u32, (edited.bottom() as f64 * scale).ceil() as u32);
        for y in y0..y1 {
            for x in x0..x1 {
                let (got, was) = (tile.sample(x, y, 0), under.sample(x, y, 0));
                assert!(got.abs_diff(was) <= 2, "proxy ({x}, {y}): {got} over {was}");
            }
        }
    }

    #[test]
    fn a_page_identity_moves_with_a_neighbours_patch_across_a_join() {
        let scratch = Scratch::new("appearance-join");
        let mut job = a_longstrip(&scratch, 3);
        job.complete_region(0, &toned_patch("mover", Rect::new(10, 10, 20, 10), 215, serde_json::json!({})), None)
            .unwrap();
        let identities = |job: &Job| (0..3).map(|page| page_appearance(&job.project, page)).collect::<Vec<_>>();
        let before = identities(&job);
        assert!(before.iter().all(|digest| digest.len() == 16));

        job.project.patches[0].provenance.params_snapshot["layer"] = serde_json::json!({"offsetY": 40});
        job.flush().unwrap();
        let moved = identities(&job);
        assert_ne!(moved[0], before[0]);
        assert_ne!(moved[1], before[1], "the page the patch moved onto kept its identity");
        assert_eq!(moved[2], before[2], "a page the patch never reached changed");

        // The same saved state, reopened: the same identities.
        let reopened = Job::open(job.path()).unwrap();
        assert_eq!(identities(&reopened), moved);

        // Opacity is an appearance; the lock is not.
        job.project.patches[0].provenance.params_snapshot["layer"] = serde_json::json!({"offsetY": 40, "locked": true});
        assert_eq!(identities(&job), moved);
        job.project.patches[0].provenance.params_snapshot["layer"] = serde_json::json!({"offsetY": 40, "opacity": 50});
        let faded = identities(&job);
        assert_ne!(faded[1], moved[1]);
        assert_eq!(faded[2], moved[2]);
    }

    /// A re-run on the same rung, in the same second, with the same params,
    /// that draws different pixels: the manifest row differs only in the
    /// content-named sidecars, and that is enough to move the identity.
    #[test]
    fn a_same_second_rerun_with_new_pixels_is_a_new_identity() {
        let scratch = Scratch::new("appearance-rerun");
        let mut job = a_job(&scratch, "l8");
        let bounds = Rect::new(8, 8, 12, 10);
        job.complete_region(0, &toned_patch("again", bounds, 90, serde_json::json!({})), None).unwrap();
        let first = (page_appearance(&job.project, 0), record_appearance(&job.project.patches[0]));
        let first_pixels = render(job.path(), 0, Variant::Cleaned, None).unwrap();

        job.complete_region(0, &toned_patch("again", bounds, 170, serde_json::json!({})), None).unwrap();
        assert_eq!(job.project.patches.len(), 1);
        assert_eq!(job.project.patches[0].provenance.created, 0, "the rerun is meant to share its second");
        let second = (page_appearance(&job.project, 0), record_appearance(&job.project.patches[0]));
        assert_ne!(render(job.path(), 0, Variant::Cleaned, None).unwrap(), first_pixels);
        assert_ne!(second.1, first.1, "new pixels kept the layer identity");
        assert_ne!(second.0, first.0, "new pixels kept the page identity");
    }

    /// Review bookkeeping written into a record after its commit - a
    /// dependency flag, the state it saved, and that flag cleared again - is
    /// not an appearance, so a lower layer's edit and its undo leave the
    /// identity of unchanged pixels where it was.
    #[test]
    fn review_bookkeeping_is_not_an_appearance() {
        let scratch = Scratch::new("appearance-review");
        let mut job = a_job(&scratch, "l8");
        job.complete_region(0, &toned_patch("upper", Rect::new(8, 8, 12, 10), 90, serde_json::json!({})), None)
            .unwrap();
        let clean = page_appearance(&job.project, 0);
        let record = &mut job.project.patches[0];
        record.provenance.params_snapshot["review_before_input_change"] = serde_json::Value::Null;
        record.review_state = Some("review.reason.inputChanged".into());
        assert_eq!(page_appearance(&job.project, 0), clean, "a dependency flag changed the identity");
        let record = &mut job.project.patches[0];
        record.review_state = None;
        record.provenance.params_snapshot.as_object_mut().unwrap().remove("review_before_input_change");
        assert_eq!(page_appearance(&job.project, 0), clean);
    }

    /// Every page's identity at once is each page's identity asked alone,
    /// across joins, moves and hidden layers.
    #[test]
    fn a_chapter_of_identities_is_each_page_asked_alone() {
        let scratch = Scratch::new("appearance-all");
        let mut job = a_longstrip(&scratch, 4);
        job.complete_region(0, &toned_patch("span", Rect::new(10, 40, 20, 16), 200, serde_json::json!({})), None)
            .unwrap();
        job.complete_region(2, &toned_patch("moved", Rect::new(4, 4, 8, 8), 60, serde_json::json!({"offsetY": 50})), None)
            .unwrap();
        job.complete_region(3, &toned_patch("hidden", Rect::new(4, 4, 8, 8), 60, serde_json::json!({})), None)
            .unwrap();
        job.project.patches[2].visible = false;
        let all = page_appearances(&job.project);
        assert_eq!(all.len(), 4);
        for (page, digest) in all.iter().enumerate() {
            assert_eq!(digest, &page_appearance(&job.project, page), "page {page}");
        }
        let paginated = a_strip(&Scratch::new("appearance-all-paginated"), 3, vec![2, 0, 1]);
        let all = page_appearances(&paginated.project);
        assert_eq!(all, (0..3).map(|page| page_appearance(&paginated.project, page)).collect::<Vec<_>>());
    }

    /// A `cleaned` response may be kept only when its URL was built from the
    /// saved state it was drawn from. An older page listing's URL is answered
    /// with the right pixels and `no-store`, naming the state they show.
    #[test]
    fn a_tile_is_cached_only_under_the_appearance_it_was_drawn_from() {
        let scratch = Scratch::new("appearance-served");
        let mut job = a_job(&scratch, "l8");
        job.complete_region(0, &toned_patch("slider", Rect::new(8, 8, 12, 10), 200, serde_json::json!({})), None)
            .unwrap();
        let listed = page_appearance(&job.project, 0);
        let cleaned = TileRequest { chapter_id: "c".into(), page_index: 0, variant: Variant::Cleaned, tile: Some(0) };
        let (_, fresh) = serve_request(job.path(), &cleaned, Some(&listed)).unwrap();
        assert_eq!(fresh, Freshness::Immutable);

        // A preview write lands after the listing was read.
        job.project.patches[0].provenance.params_snapshot["layer"] = serde_json::json!({"opacity": 40});
        job.flush().unwrap();
        let now = page_appearance(&job.project, 0);
        let (bytes, stale) = serve_request(job.path(), &cleaned, Some(&listed)).unwrap();
        assert_eq!(stale, Freshness::Uncached { current: now.clone() });
        assert_eq!(bytes, render(job.path(), 0, Variant::Cleaned, Some(0)).unwrap(), "the pixels were not the saved state");
        let response = ok(bytes, stale);
        assert_eq!(response.headers().get(header::CACHE_CONTROL).unwrap(), "no-store");
        assert_eq!(response.headers().get(APPEARANCE_HEADER).unwrap(), now.as_str());

        // No `a` cannot be checked, so it is not kept either; a source tile
        // needs none.
        assert!(matches!(serve_request(job.path(), &cleaned, None).unwrap().1, Freshness::Uncached { .. }));
        let source = TileRequest { variant: Variant::Source, ..cleaned };
        assert_eq!(serve_request(job.path(), &source, None).unwrap().1, Freshness::Immutable);
    }

    #[test]
    fn the_appearance_is_read_from_the_query_alone() {
        assert_eq!(expected_appearance(Some("v=00ff&a=0123456789abcdef")).as_deref(), Some("0123456789abcdef"));
        assert_eq!(expected_appearance(Some("a=feed&v=1")).as_deref(), Some("feed"));
        assert_eq!(expected_appearance(Some("v=00ff")), None);
        assert_eq!(expected_appearance(Some("v=00ff&a=")), None);
        assert_eq!(expected_appearance(None), None);
    }

    #[test]
    fn a_paginated_page_identity_is_its_own_patches_alone() {
        let scratch = Scratch::new("appearance-paginated");
        let mut job = a_strip(&scratch, 2, vec![0, 1]);
        let blank = [page_appearance(&job.project, 0), page_appearance(&job.project, 1)];
        job.complete_region(0, &toned_patch("own", Rect::new(4, 4, 8, 8), 90, serde_json::json!({})), None)
            .unwrap();
        assert_ne!(page_appearance(&job.project, 0), blank[0]);
        assert_eq!(page_appearance(&job.project, 1), blank[1]);
        job.project.patches[0].visible = false;
        assert_eq!(page_appearance(&job.project, 0), blank[0], "a hidden patch still counted");
    }

    #[test]
    fn a_page_is_drawn_once_for_all_of_its_tiles_and_again_only_when_it_changes() {
        let cache = TileCache::default();
        let key = |identity: &str| PageKey {
            job: PathBuf::from("/job"),
            page_index: 0,
            variant: Variant::Cleaned,
            identity: identity.into(),
        };
        let draws = std::cell::Cell::new(0);
        let render = || {
            draws.set(draws.get() + 1);
            Ok(vec![vec![1], vec![2]])
        };

        assert_eq!(cache.tile(key("a"), 1, render).unwrap(), vec![2]);
        assert_eq!(cache.tile(key("a"), 0, render).unwrap(), vec![1]);
        assert_eq!(draws.get(), 1, "the second tile decoded the page again");

        assert!(matches!(cache.tile(key("a"), 2, render), Err(TileError::NoSuchTile { tiles: 2, .. })));
        assert_eq!(draws.get(), 1);

        // A saved edit is a new identity, and so a new drawing.
        cache.tile(key("b"), 0, render).unwrap();
        assert_eq!(draws.get(), 2);

        // A failure is answered and forgotten, so the next request tries again.
        let failed = cache.tile(key("c"), 0, || Err(TileError::Malformed("broken".into())));
        assert!(matches!(failed, Err(TileError::Malformed(_))));
        assert_eq!(cache.tile(key("c"), 0, render).unwrap(), vec![1]);
        assert_eq!(draws.get(), 3);
    }

    /// `detection` makes the fourth segment a region id, percent-decoded. A
    /// number after `source` or `cleaned` is still a tile, and the page parser
    /// alone does not take the new form.
    #[test]
    fn a_detection_path_names_a_region_rather_than_a_tile() {
        assert_eq!(
            route("/ch-01/2/detection/ch-01-p003-d4").unwrap(),
            Route::Detection(DetectionRequest {
                chapter_id: "ch-01".into(),
                page_index: 2,
                region_id: "ch-01-p003-d4".into(),
            })
        );
        let Route::Detection(decoded) = route("/ch%2001/0/detection/ch%2001-p001-d0").unwrap() else {
            panic!("not a detection");
        };
        assert_eq!((decoded.chapter_id.as_str(), decoded.region_id.as_str()), ("ch 01", "ch 01-p001-d0"));
        assert_eq!(route("/ch-01/7/cleaned/3").unwrap(), Route::Page(parse("/ch-01/7/cleaned/3").unwrap()));
        assert_eq!(route("/ch-01/7/source").unwrap(), Route::Page(parse("/ch-01/7/source").unwrap()));
        for path in ["/ch-01/0/detection", "/ch-01/0/detection/", "/ch-01/x/detection/a", "//0/detection/a",
            "/ch-01/0/detection/a/b", "/ch-01/0/cleaned/a"] {
            assert_eq!(route(path).expect_err(path).status(), StatusCode::BAD_REQUEST, "{path}");
        }
        assert!(parse("/ch-01/0/detection/a").is_err());
    }

    #[test]
    fn a_layer_path_names_a_region_rather_than_a_tile() {
        assert_eq!(
            route("/ch-01/2/layer/ch-01-p003-r4").unwrap(),
            Route::Layer(DetectionRequest {
                chapter_id: "ch-01".into(),
                page_index: 2,
                region_id: "ch-01-p003-r4".into(),
            })
        );
        for path in ["/ch-01/0/layer", "/ch-01/0/layer/", "/ch-01/x/layer/a", "/ch-01/0/layer/a/b"] {
            assert_eq!(route(path).expect_err(path).status(), StatusCode::BAD_REQUEST, "{path}");
        }
    }

    /// A grey source tile with a served layer drawn over it the way a browser
    /// draws an `<img>` at CSS `opacity`: source-over, 8-bit.
    fn drawn_over(source: &Raster, layer: &Layer, opacity: u8) -> Raster {
        let image = decode(&layer.png).unwrap();
        assert_eq!(image.mode, ColorMode::Rgba);
        assert_eq!((image.width, image.height), (layer.bounds.w, layer.bounds.h));
        let mut out = source.clone();
        for y in 0..image.height {
            for x in 0..image.width {
                let alpha = f64::from(image.sample(x, y, 3)) * f64::from(opacity) / 100.0 / 255.0;
                let (px, py) = (layer.bounds.x + x, layer.bounds.y + y);
                let below = f64::from(source.sample(px, py, 0));
                let mixed = f64::from(image.sample(x, y, 0)) * alpha + below * (1.0 - alpha);
                out.set_sample(px, py, 0, mixed.round() as u16);
            }
        }
        out
    }

    fn largest_difference(a: &Raster, b: &Raster) -> u16 {
        assert_eq!((a.width, a.height), (b.width, b.height));
        let mut worst = 0;
        for y in 0..a.height {
            for x in 0..a.width {
                worst = worst.max(a.sample(x, y, 0).abs_diff(b.sample(x, y, 0)));
            }
        }
        worst
    }

    /// **The layer is the cleaned tile, taken apart.** Drawn over the source
    /// tile at the layer's opacity, it is the flattened `cleaned` tile of the
    /// same saved state: moved, turned, at half opacity. A turned layer's rim
    /// has partial coverage, and the two paths round it once each.
    #[test]
    fn a_layer_over_the_source_tile_is_the_cleaned_tile() {
        let cases = [
            ("plain", serde_json::json!({}), 100u8),
            ("moved", serde_json::json!({"offsetX": 3, "offsetY": -2}), 100),
            ("turned", serde_json::json!({"rotation": 30.0}), 100),
            ("faded", serde_json::json!({"opacity": 50}), 50),
        ];
        for (name, style, opacity) in cases {
            let scratch = Scratch::new(&format!("layer-{name}"));
            let mut job = a_job(&scratch, "l8");
            let bounds = Rect::new(8, 8, 12, 10);
            job.complete_region(0, &toned_patch("r0", bounds, 200, style), None).unwrap();
            let source = decode(&render(job.path(), 0, Variant::Source, Some(0)).unwrap()).unwrap();
            let cleaned = decode(&render(job.path(), 0, Variant::Cleaned, Some(0)).unwrap()).unwrap();
            let request = DetectionRequest { chapter_id: "c".into(), page_index: 0, region_id: "r0".into() };
            let layer = serve_layer(job.path(), &request).unwrap().expect("a layer on its own page");
            let worst = largest_difference(&drawn_over(&source, &layer, opacity), &cleaned);
            assert!(worst <= 1, "{name}: {worst} levels from the cleaned tile");
        }
    }

    /// Across a join each page gets its own part, placed on its own grid, and
    /// a page the patch does not reach gets nothing - a `204`, not an error.
    #[test]
    fn a_layer_spanning_a_join_is_drawn_on_each_page_it_reaches() {
        let scratch = Scratch::new("layer-join");
        let mut job = a_longstrip(&scratch, 3);
        job.complete_region(0, &toned_patch("spanning", Rect::new(10, 40, 20, 16), 215, serde_json::json!({})), None)
            .unwrap();
        let request = |page_index| DetectionRequest { chapter_id: "c".into(), page_index, region_id: "spanning".into() };
        for page in [0, 1] {
            let source = decode(&render(job.path(), page, Variant::Source, Some(0)).unwrap()).unwrap();
            let cleaned = decode(&render(job.path(), page, Variant::Cleaned, Some(0)).unwrap()).unwrap();
            let layer = serve_layer(job.path(), &request(page)).unwrap().expect("the patch reaches this page");
            assert_eq!(largest_difference(&drawn_over(&source, &layer, 100), &cleaned), 0, "page {page}");
        }
        assert_eq!(serve_layer(job.path(), &request(1)).unwrap().unwrap().bounds.y, 0);
        let beyond = serve_layer(job.path(), &request(2)).unwrap();
        assert!(beyond.is_none(), "a page the patch does not reach drew it");
        let response = layer_response(beyond);
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(response.body().is_empty());
    }

    /// Only a visible patch has a layer. A paginated patch on another page is
    /// nothing to draw here; an id nobody has is a `404`.
    #[test]
    fn a_layer_is_a_visible_patch_on_the_page() {
        let scratch = Scratch::new("layer-404");
        let mut job = a_strip(&scratch, 2, vec![0, 1]);
        job.complete_region(0, &toned_patch("r0", Rect::new(8, 8, 12, 10), 200, serde_json::json!({})), None).unwrap();
        let request = |page_index, id: &str| DetectionRequest { chapter_id: "c".into(), page_index, region_id: id.into() };
        assert!(serve_layer(job.path(), &request(1, "r0")).unwrap().is_none());
        let missing = serve_layer(job.path(), &request(0, "nobody")).expect_err("an unknown id");
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        let layer = serve_layer(job.path(), &request(0, "r0")).unwrap().unwrap();
        let response = layer_response(Some(layer.clone()));
        let headers = response.headers();
        let TileRect { x, y, w, h } = layer.bounds;
        assert_eq!(headers.get(header::CONTENT_TYPE).unwrap(), "image/png");
        assert_eq!(headers.get(header::CACHE_CONTROL).unwrap(), "no-store");
        assert_eq!(headers.get(LAYER_BOUNDS_HEADER).unwrap(), &format!("{x},{y},{w},{h}"));
        assert_eq!(headers.get(header::ACCESS_CONTROL_EXPOSE_HEADERS).unwrap(), LAYER_BOUNDS_HEADER);
    }

    /// A page's layers share one read of the manifest, and a save is never
    /// answered from the read before it.
    #[test]
    fn a_layer_is_drawn_from_the_manifest_as_saved() {
        let scratch = Scratch::new("layer-saved");
        let mut job = a_job(&scratch, "l8");
        let bounds = Rect::new(8, 8, 12, 10);
        job.complete_region(0, &toned_patch("r0", bounds, 200, serde_json::json!({})), None).unwrap();
        let request = DetectionRequest { chapter_id: "c".into(), page_index: 0, region_id: "r0".into() };
        let tone = |layer: Layer| decode(&layer.png).unwrap().sample(2, 2, 0);
        assert_eq!(tone(serve_layer(job.path(), &request).unwrap().unwrap()), 200);
        assert_eq!(tone(serve_layer(job.path(), &request).unwrap().unwrap()), 200);
        job.complete_region(0, &toned_patch("r0", bounds, 90, serde_json::json!({})), None).unwrap();
        assert_eq!(tone(serve_layer(job.path(), &request).unwrap().unwrap()), 90);
    }

    /// The key a layer URL is versioned by moves with what its pixels are and
    /// never with what the interface draws itself.
    #[test]
    fn the_layer_key_ignores_opacity_and_order_and_moves_with_geometry() {
        let scratch = Scratch::new("layer-key");
        let mut job = a_job(&scratch, "l8");
        job.complete_region(0, &toned_patch("r0", Rect::new(8, 8, 12, 10), 200, serde_json::json!({})), None).unwrap();
        let record = job.project.patches[0].clone();
        let with = |layer: serde_json::Value| {
            let mut changed = record.clone();
            changed.provenance.params_snapshot = serde_json::json!({"layer": layer});
            changed
        };
        let key = layer_appearance(&record);
        let faded = with(serde_json::json!({"opacity": 40}));
        assert_eq!(layer_appearance(&faded), key);
        assert_ne!(record_appearance(&faded), record_appearance(&record));
        let mut raised = record.clone();
        raised.order += 3;
        assert_eq!(layer_appearance(&raised), key);
        assert_ne!(layer_appearance(&with(serde_json::json!({"offsetX": 1}))), key);
        assert_ne!(layer_appearance(&with(serde_json::json!({"rotation": 5.0}))), key);
    }

    fn store_detection(job: &mut Job, source_idx: usize, mask: &Mask, ink: &Mask) -> String {
        let id = job.next_detection_id(&format!("c-p{:03}", source_idx + 1));
        job.store_detection(cleaner_core::project::DetectedRegion {
            id: id.clone(), source_idx, bbox: mask.bounds, mask_ref: String::new(), inside: true,
            balloon_color: None, script: None, pick: "fill".into(), detector: "local".into(), created: 0,
            mask_sha256: String::new(), order: 0, review_state: None, fit: None, group: None,
            padding_from_seed: false,
            padding_px: 0,
        }, mask, ink).unwrap();
        id
    }

    /// The image is the mask and the lettering together, cropped to them,
    /// opaque white exactly there and transparent everywhere else, and the
    /// header says where it goes.
    #[test]
    fn a_detection_is_served_as_its_display_set_with_its_bounds() {
        let scratch = Scratch::new("detection-mask");
        let mut job = a_job(&scratch, "l8");
        let mask = Mask::filled(Rect::new(8, 8, 12, 10));
        // Lettering that reaches past the mask, as 3.7% of it does in a real
        // library, is part of what a clean may erase.
        let ink = Mask::filled(Rect::new(18, 15, 6, 4));
        let id = store_detection(&mut job, 0, &mask, &ink);
        let request = DetectionRequest { chapter_id: "c".into(), page_index: 0, region_id: id };
        let served = serve_detection(job.path(), &request).unwrap();
        assert_eq!(served.bounds, Rect::new(8, 8, 16, 11));

        let image = decode(&served.png).unwrap();
        assert_eq!((image.width, image.height, image.mode), (16, 11, ColorMode::Rgba));
        for y in 0..image.height {
            for x in 0..image.width {
                let (px, py) = (8 + i64::from(x), 8 + i64::from(y));
                let shown = mask.contains(px, py) || ink.contains(px, py);
                let want = if shown { 255 } else { 0 };
                for channel in 0..4 {
                    assert_eq!(image.sample(x, y, channel), want, "({px}, {py}) channel {channel}");
                }
            }
        }

        let response = detection_response(served);
        let headers = response.headers();
        assert_eq!(headers.get(header::CONTENT_TYPE).unwrap(), "image/png");
        assert_eq!(headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).unwrap(), "*");
        assert_eq!(headers.get(header::CACHE_CONTROL).unwrap(), "no-store");
        assert_eq!(headers.get(MASK_BOUNDS_HEADER).unwrap(), "8,8,16,11");
        assert_eq!(headers.get(header::ACCESS_CONTROL_EXPOSE_HEADERS).unwrap(), MASK_BOUNDS_HEADER);
    }

    /// Only a detection on the page the path names is drawn: another page's,
    /// a patch's id, an id nobody has and a detection with no pixels are all
    /// `404`, and so is a page the chapter does not have.
    #[test]
    fn a_detection_the_page_does_not_hold_is_a_404() {
        let scratch = Scratch::new("detection-404");
        let mut job = a_strip(&scratch, 2, vec![0, 1]);
        let mask = Mask::filled(Rect::new(8, 8, 12, 10));
        let on_first = store_detection(&mut job, 0, &mask, &mask);
        let blank = store_detection(&mut job, 0, &Mask::empty(Rect::new(0, 0, 4, 4)), &Mask::empty(Rect::new(0, 0, 4, 4)));
        let page = decode(&std::fs::read(job.source_path(0).unwrap()).unwrap()).unwrap();
        job.complete_region(0, &a_patch(&page, Rect::new(30, 30, 4, 4), 200), None).unwrap();

        let ask = |page_index: usize, region_id: &str| {
            serve_detection(job.path(), &DetectionRequest {
                chapter_id: "c".into(),
                page_index,
                region_id: region_id.into(),
            })
        };
        assert!(ask(0, &on_first).is_ok());
        for (page_index, region_id) in [(1, on_first.as_str()), (0, "r0"), (0, "c-p001-d99"), (0, blank.as_str()),
            (5, on_first.as_str())] {
            let error = ask(page_index, region_id).expect_err(region_id);
            assert_eq!(error.status(), StatusCode::NOT_FOUND, "page {page_index}, {region_id}");
        }
    }

    /// Detection masks are read with `fetch`, not an `<img>`, because the
    /// bounds come in a header. `fetch` answers to `connect-src`, and a policy
    /// without the tile scheme blocks every mask in the shipped app while the
    /// Vite mock, which has no policy, shows them.
    #[test]
    fn the_content_policy_lets_a_fetch_reach_the_tile_protocol() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri.conf.json parses");
        let policy = &config["app"]["security"]["csp"];
        for directive in ["img-src", "connect-src"] {
            let sources: Vec<&str> = policy[directive].as_str().unwrap_or_default().split_whitespace().collect();
            for origin in ["tile:", "http://tile.localhost"] {
                assert!(sources.contains(&origin), "{directive} must allow {origin}");
            }
        }
    }
}
